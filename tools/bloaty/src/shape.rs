//! Fn shapes that excuse some of a fn's numbers from the smell score, and
//! explicit opt-outs:
//! - a *table*: the body ends in one literal list (`vec![..]`, an array, a
//!   struct literal, or a match) of simple entries. The literal reads like
//!   data: its lines are excused from size; everything else (the lets
//!   before it) is scored as usual.
//! - a *dispatch*: the body ends in one match (or `Ok(match ..)`) with many
//!   mostly small arms. Each arm is measured like a fn; the fn is scored as
//!   its remainder (lets, scrutinee) plus its worst arm, so only the
//!   match's width is excused. Arms that open with the same statements are
//!   reported as a repeated prelude.
//! - `#[allow(clippy::too_many_lines)]` / `#[allow(clippy::cognitive_complexity)]`
//!   (or `expect`, also inside `cfg_attr`) on the fn: excused from size /
//!   complexity, up to a ceiling (see `rollup`).
//!
//! Panics are never excused.

use crate::fnmetrics::{FnMetrics, arm_metrics, expr_complexity, remainder_metrics};
use crate::visit::body::token_hashes;
use crate::visit::syntax::compact;
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use std::collections::HashMap;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Block, Expr, Meta, Pat, Stmt, Token};

/// Fewer entries, or code lines, than this is just a literal, not a table.
pub const MIN_ENTRIES: usize = 4;
/// Fewer arms than this is ordinary branching, not a dispatch.
pub const MIN_ARMS: usize = 4;
/// A *small* arm or table entry: at most this many code lines (and an arm
/// at most this cyclomatic complexity). Only small arms are excused; a
/// table entry may be no bigger; and a match is a dispatch only when its
/// middle arm (the upper median) is small: a match of medium-sized arms
/// is a god fn, not a dispatch.
pub const SMALL_ARM_LINES: usize = 12;
pub const SMALL_ARM_COMPLEXITY: u32 = 3;
/// `let`s allowed before the table or match (scored as the remainder).
pub const MAX_LEAD_STMTS: usize = 2;
/// How deep a table entry may nest calls, literals and the like.
const MAX_ENTRY_DEPTH: usize = 4;
/// A repeated prelude needs this many tokens and a call or control flow
/// (`let a = p; let b = q;` repeated is nothing to extract)...
pub const MIN_PRELUDE_TOKENS: usize = 12;
/// ...in at least this many arms.
const MIN_PRELUDE_ARMS: usize = 2;
/// Where a prelude repeats: the first this many (`count` has them all).
pub const MAX_PRELUDE_AT: usize = 20;

/// A clippy lint whose `allow` excuses a smell dimension.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Lint {
    /// Excuses size.
    TooManyLines,
    /// Excuses complexity (cyclomatic complexity and nesting).
    CognitiveComplexity,
}

impl Lint {
    pub fn as_str(self) -> &'static str {
        match self {
            Lint::TooManyLines => "too_many_lines",
            Lint::CognitiveComplexity => "cognitive_complexity",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Table(Table),
    Dispatch(Dispatch),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub entries: usize,
    /// The literal's lines (`lines`: code lines, set by `settle`).
    pub start_line: usize,
    pub end_line: usize,
    pub lines: usize,
    /// Each entry's first and last line (for `settle`'s size check).
    pub entry_spans: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dispatch {
    pub arms: Vec<Arm>,
    /// From the first arm to the end of the last (`lines`: code lines, set
    /// by `settle`).
    pub start_line: usize,
    pub end_line: usize,
    pub lines: usize,
    /// The fn without the arms.
    pub remainder: FnMetrics,
    pub repeat: Option<Repeat>,
}

/// A match arm, measured like a fn.
#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    /// From the pattern's structure: `Call::Donate`, `(State::Idle, Ev::Go)`,
    /// `A | B`.
    pub name: String,
    pub line: usize,
    pub end_line: usize,
    /// Code lines, set by `settle`.
    pub lines: usize,
    /// Where the body starts, and its code lines (set by `settle`): a
    /// destructuring pattern spread over lines isn't code to judge.
    pub body_line: usize,
    pub body_lines: usize,
    pub metrics: FnMetrics,
}

impl Arm {
    /// Small enough to be excused (`SMALL_ARM_*`), judged by its body.
    pub fn small(&self) -> bool {
        self.body_lines <= SMALL_ARM_LINES && self.metrics.complexity <= SMALL_ARM_COMPLEXITY
    }
}

/// Arms opening with the same statements (names and literals aside).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Repeat {
    /// How many statements the prelude has.
    pub statements: usize,
    /// How many arms open with it.
    pub count: usize,
    /// Where the first `MAX_PRELUDE_AT` copies start, in source order.
    pub at: Vec<usize>,
}

/// The `#[allow]` / `#[expect]` clippy lints among `attrs` (directly or in
/// a `cfg_attr`, whatever its condition) that excuse a dimension. Only the
/// fn's own attributes: impl- and module-level allows, and lint groups
/// (`clippy::all`, `clippy::pedantic`), don't count.
pub fn allowed(attrs: &[Attribute]) -> Vec<Lint> {
    let mut out = Vec::new();
    for a in attrs {
        lints_in(&a.meta, &mut out);
    }
    out
}

fn lints_in(meta: &Meta, out: &mut Vec<Lint>) {
    let Meta::List(list) = meta else { return };
    let metas = || {
        list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .map(|p| p.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    if list.path.is_ident("cfg_attr") {
        // The condition first, then attributes; `any()` never holds.
        let metas = metas();
        let never = matches!(metas.first(), Some(Meta::List(l)) if l.path.is_ident("any") && l.tokens.is_empty());
        for m in metas.iter().skip(1).filter(|_| !never) {
            lints_in(m, out);
        }
    } else if list.path.is_ident("allow") || list.path.is_ident("expect") {
        for m in metas() {
            let segs: Vec<String> = m
                .path()
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let lint = match segs.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                ["clippy", "too_many_lines"] => Some(Lint::TooManyLines),
                ["clippy", "cognitive_complexity"] => Some(Lint::CognitiveComplexity),
                _ => None,
            };
            out.extend(lint.filter(|l| !out.contains(l)));
        }
    }
}

/// The fn's shape, if it ends in a table or a match. A match's dispatch
/// shape is final only after `settle`, which needs code lines.
pub fn shape(block: &Block) -> Option<Shape> {
    let tail = tail(block)?;
    // Entries hiding logic (an `if` inside `format!`) disqualify it.
    let logic_free = || expr_complexity(tail) <= 1 + u32::from(matches!(tail, Expr::Match(_)));
    if let Some(spans) = table_entries(tail, 0).filter(|s| s.len() >= MIN_ENTRIES && logic_free()) {
        // A match table's entries are its arms: its scrutinee is code.
        let (start, end) = match tail {
            Expr::Match(m) => (
                m.arms.first()?.pat.span().start().line,
                m.arms.last()?.body.span().end().line,
            ),
            _ => (tail.span().start().line, tail.span().end().line),
        };
        return Some(Shape::Table(Table {
            entries: spans.len(),
            start_line: start,
            end_line: end,
            lines: 0,
            entry_spans: spans,
        }));
    }
    let m = match tail {
        Expr::Match(m) => m,
        // `Ok(match ..)`, `Some(match ..)`.
        Expr::Call(c) if matches!(&*c.func, Expr::Path(_)) && c.args.len() == 1 => match &c.args[0]
        {
            Expr::Match(m) => m,
            _ => return None,
        },
        _ => return None,
    };
    if m.arms.len() < MIN_ARMS {
        return None;
    }
    let first = m.arms.first()?;
    let last = m.arms.last()?;
    Some(Shape::Dispatch(Dispatch {
        arms: m.arms.iter().map(arm).collect(),
        start_line: first.pat.span().start().line,
        end_line: last.body.span().end().line,
        lines: 0,
        remainder: remainder_metrics(block, m),
        repeat: repeat(&m.arms),
    }))
}

/// Fills in code lines (`code_lines(start, end)`), and drops tables of
/// fewer than `MIN_ENTRIES` lines (nothing to excuse) or with an entry
/// over `SMALL_ARM_LINES`, and dispatch shapes whose middle arm isn't
/// small.
pub fn settle(shape: &mut Option<Shape>, code_lines: impl Fn(usize, usize) -> usize) {
    match shape {
        Some(Shape::Table(t)) => {
            t.lines = code_lines(t.start_line, t.end_line);
            let big_entry = t
                .entry_spans
                .iter()
                .any(|&(s, e)| code_lines(s, e) > SMALL_ARM_LINES);
            if t.lines < MIN_ENTRIES || big_entry {
                *shape = None;
            }
        }
        Some(Shape::Dispatch(d)) => {
            d.lines = code_lines(d.start_line, d.end_line);
            for a in &mut d.arms {
                a.lines = code_lines(a.line, a.end_line);
                a.body_lines = code_lines(a.body_line, a.end_line);
            }
            let median = |mut v: Vec<usize>| {
                v.sort_unstable();
                v[v.len() / 2]
            };
            let lines = median(d.arms.iter().map(|a| a.body_lines).collect());
            let cc = median(
                d.arms
                    .iter()
                    .map(|a| a.metrics.complexity as usize)
                    .collect(),
            );
            if lines > SMALL_ARM_LINES || cc > SMALL_ARM_COMPLEXITY as usize {
                *shape = None;
            }
        }
        None => {}
    }
}

/// The body's final expression, after at most `MAX_LEAD_STMTS` `let`s.
fn tail(block: &Block) -> Option<&Expr> {
    match block.stmts.split_last() {
        Some((Stmt::Expr(e, None), lead))
            if lead.len() <= MAX_LEAD_STMTS && lead.iter().all(|s| matches!(s, Stmt::Local(_))) =>
        {
            Some(e)
        }
        _ => None,
    }
}

/// The entries (first and last line of each) of a table literal whose
/// entries are all simple: `vec![..]`, `[..]`, `&[..]`, a struct literal, a
/// match of simple arms, or one of those as the only argument of a call
/// (`HashMap::from([..])`).
fn table_entries(e: &Expr, depth: usize) -> Option<Vec<(usize, usize)>> {
    let lines = |sp: proc_macro2::Span| (sp.start().line, sp.end().line);
    let all = |es: &mut dyn Iterator<Item = &Expr>| {
        let mut out = Vec::new();
        for e in es {
            if !simple(e, 0) {
                return None;
            }
            out.push(lines(e.span()));
        }
        Some(out)
    };
    match e {
        Expr::Array(a) => all(&mut a.elems.iter()),
        Expr::Reference(r) => table_entries(&r.expr, depth),
        Expr::Paren(p) => table_entries(&p.expr, depth),
        Expr::Macro(m) if m.mac.path.is_ident("vec") => {
            let elems = m
                .mac
                .parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated)
                .ok()?;
            all(&mut elems.iter())
        }
        Expr::Struct(s) if s.rest.as_ref().is_none_or(|r| simple(r, 0)) => {
            all(&mut s.fields.iter().map(|f| &f.expr))
        }
        Expr::Match(m) => {
            let simple_arms = m
                .arms
                .iter()
                .all(|a| a.guard.as_ref().is_none_or(|(_, g)| simple(g, 0)) && simple(&a.body, 0));
            simple_arms.then(|| {
                m.arms
                    .iter()
                    .map(|a| (a.pat.span().start().line, a.body.span().end().line))
                    .collect()
            })
        }
        Expr::Call(c) if depth == 0 && c.args.len() == 1 => table_entries(&c.args[0], depth + 1),
        _ => None,
    }
}

/// Whether macro tokens that aren't comma-separated expressions read as
/// data: no `{ }` blocks, `;`, control-flow keywords or `&&` / `||`.
fn plain_tokens(ts: TokenStream) -> bool {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    toks.iter().enumerate().all(|(i, tt)| match tt {
        TokenTree::Group(g) => g.delimiter() != Delimiter::Brace && plain_tokens(g.stream()),
        TokenTree::Ident(id) => !["if", "match", "loop", "for", "while", "return"]
            .iter()
            .any(|k| id == k),
        TokenTree::Punct(p) => {
            let c = p.as_char();
            let doubled = matches!(toks.get(i + 1), Some(TokenTree::Punct(q)) if q.as_char() == c);
            c != ';' && !((c == '&' || c == '|') && doubled)
        }
        TokenTree::Literal(_) => true,
    })
}

/// A literal, path, or a call / constructor / struct literal / tuple of
/// those: an entry that reads as data.
fn simple(e: &Expr, depth: usize) -> bool {
    if depth > MAX_ENTRY_DEPTH {
        return false;
    }
    let s = |e: &Expr| simple(e, depth + 1);
    match e {
        Expr::Lit(_) | Expr::Path(_) => true,
        // `format!("{}", x)`: its arguments must be simple too.
        Expr::Macro(m) => match m
            .mac
            .parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated)
        {
            Ok(args) => args.iter().all(s),
            Err(_) => plain_tokens(m.mac.tokens.clone()),
        },
        Expr::Unary(u) => s(&u.expr),
        Expr::Reference(r) => s(&r.expr),
        Expr::Paren(p) => s(&p.expr),
        Expr::Cast(c) => s(&c.expr),
        Expr::Field(f) => s(&f.base),
        Expr::Tuple(t) => t.elems.iter().all(s),
        Expr::Array(a) => a.elems.iter().all(s),
        Expr::Call(c) => s(&c.func) && c.args.iter().all(s),
        Expr::MethodCall(m) => s(&m.receiver) && m.args.iter().all(s),
        Expr::Struct(st) => {
            st.fields.iter().all(|f| s(&f.expr)) && st.rest.as_ref().is_none_or(|r| s(r))
        }
        _ => false,
    }
}

/// A pattern's name from its structure: paths without generic arguments
/// (`Foo::<u8>::Bar {..}` -> `Foo::Bar`), a wrapper around one named
/// pattern by that (`Ok(Reply::Error(e))` -> `Reply::Error`), tuples and
/// alternatives kept (`(State::Idle, Ev::Go)`, `A | B`), bindings by what
/// they bind (`x @ "z"` -> `"z"`).
fn pat_name(p: &Pat) -> String {
    let path = |p: &syn::Path| {
        p.segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::")
    };
    let list = |ps: &mut dyn Iterator<Item = &Pat>, sep: &str| {
        ps.map(pat_name).collect::<Vec<_>>().join(sep)
    };
    match p {
        Pat::Ident(i) => match &i.subpat {
            Some((_, sub)) => pat_name(sub),
            None => i.ident.to_string(),
        },
        Pat::Path(pp) => path(&pp.path),
        Pat::Struct(st) => path(&st.path),
        Pat::TupleStruct(ts) => {
            let inner = ts.elems.first().map(pat_name).unwrap_or_default();
            if ts.elems.len() == 1 && inner.contains("::") {
                inner
            } else {
                path(&ts.path)
            }
        }
        Pat::Tuple(t) => format!("({})", list(&mut t.elems.iter(), ", ")),
        Pat::Or(o) => list(&mut o.cases.iter(), " | "),
        Pat::Reference(r) => pat_name(&r.pat),
        Pat::Paren(pp) => pat_name(&pp.pat),
        Pat::Wild(_) => "_".to_string(),
        Pat::Rest(_) => "..".to_string(),
        other => compact(other),
    }
}

fn arm(a: &syn::Arm) -> Arm {
    Arm {
        name: pat_name(&a.pat),
        line: a.pat.span().start().line,
        end_line: a.body.span().end().line,
        lines: 0,
        body_line: a.body.span().start().line,
        body_lines: 0,
        metrics: arm_metrics(a),
    }
}

/// Tokens of `ts`, and whether there's a call or control flow among them.
fn weigh(ts: TokenStream) -> (usize, bool) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    let mut n = 0;
    let mut active = false;
    for (i, tt) in toks.iter().enumerate() {
        n += 1;
        match tt {
            TokenTree::Group(g) => {
                let called = g.delimiter() == Delimiter::Parenthesis
                    && i > 0
                    && matches!(&toks[i - 1], TokenTree::Ident(_));
                let (inner, inner_active) = weigh(g.stream());
                n += inner;
                active |= called || inner_active;
            }
            TokenTree::Ident(id) => {
                active |= [
                    "match", "if", "return", "loop", "for", "while", "break", "continue",
                ]
                .iter()
                .any(|k| id == k);
            }
            TokenTree::Punct(p) => active |= p.as_char() == '?' || p.as_char() == '!',
            TokenTree::Literal(_) => {}
        }
    }
    (n, active)
}

/// The largest group of arms whose bodies open with the same statements
/// (compared by shape: names and literals aside), and how far that common
/// opening goes, if it amounts to something (`MIN_PRELUDE_TOKENS`, a call
/// or control flow).
fn repeat(arms: &[syn::Arm]) -> Option<Repeat> {
    let stmts: Vec<&[Stmt]> = arms
        .iter()
        .map(|a| match &*a.body {
            // A prelude needs something after it.
            Expr::Block(b) if b.block.stmts.len() >= 2 => &b.block.stmts[..],
            _ => &[][..],
        })
        .collect();
    let hash = |s: &Stmt| token_hashes(s.to_token_stream()).shape;
    let mut groups: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, s) in stmts.iter().enumerate() {
        if let Some(first) = s.first() {
            groups.entry(hash(first)).or_default().push(i);
        }
    }
    let mut best: Option<Repeat> = None;
    for mut group in groups.into_values().filter(|g| g.len() >= MIN_PRELUDE_ARMS) {
        group.sort_unstable();
        // Extend the common opening while every arm in the group has the
        // same next statement (and still something after it).
        let first = stmts[group[0]];
        let mut k = 1;
        while group
            .iter()
            .all(|&i| stmts[i].len() > k + 1 && hash(&stmts[i][k]) == hash(&first[k]))
        {
            k += 1;
        }
        let prelude: TokenStream = first[..k].iter().map(|s| s.to_token_stream()).collect();
        let (tokens, active) = weigh(prelude);
        if tokens < MIN_PRELUDE_TOKENS || !active {
            continue;
        }
        let candidate = Repeat {
            statements: k,
            count: group.len(),
            at: group
                .iter()
                .take(MAX_PRELUDE_AT)
                .map(|&i| stmts[i][0].span().start().line)
                .collect(),
        };
        // Most arms, then most statements, then earliest.
        let better = best.as_ref().is_none_or(|b| {
            (
                candidate.count,
                candidate.statements,
                std::cmp::Reverse(candidate.at[0]),
            ) > (b.count, b.statements, std::cmp::Reverse(b.at[0]))
        });
        if better {
            best = Some(candidate);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fact, facts};

    fn shape_of(src: &str, name: &str) -> Option<Shape> {
        fact(&facts(src), name).shape.clone()
    }

    /// The entries of `f` if it is a table.
    fn t(src: &str) -> Option<usize> {
        match shape_of(src, "f") {
            Some(Shape::Table(t)) => Some(t.entries),
            _ => None,
        }
    }

    #[test]
    fn tables() {
        assert_eq!(
            t(
                "fn f() -> Vec<R> { vec![R::new(A, \"a\", h::a), R::new(B, \"b\", h::b), R::only(C), R { x: 1, y: (2, 3) }] }"
            ),
            Some(4)
        );
        assert_eq!(t("fn f() -> &'static [u8] { &[1, 2, 3, 4, 5] }"), Some(5));
        assert_eq!(
            t(
                "fn f(k: K) -> &'static str { match k { K::A => \"a\", K::B => \"b\", K::C => \"c\", _ => \"?\" } }"
            ),
            Some(4)
        );
        assert_eq!(
            t(
                "fn f() -> HashMap<u8, u8> { let n = 2; HashMap::from([(1, n), (2, n), (3, n), (4, n)]) }"
            ),
            Some(4)
        );
        // Too few entries; an entry with logic; a loop around it.
        assert_eq!(t("fn f() -> [u8; 3] { [1, 2, 3] }"), None);
        assert_eq!(
            t("fn f() -> Vec<u8> { vec![1, 2, 3, if x { 4 } else { 5 }] }"),
            None
        );
        assert_eq!(
            t("fn f() -> Vec<u8> { for _ in 0..2 {} vec![1, 2, 3, 4] }"),
            None
        );
        // Macro entries count when their arguments are simple...
        assert_eq!(
            t(
                "fn f(a: u8) -> Vec<String> { vec![format!(\"{}\", a), format!(\"{}\", 1), m!(a), n![a b]] }"
            ),
            Some(4)
        );
        // ...not when they hide logic: an `if` or a block in `format!`,
        // statements in a custom macro.
        assert_eq!(
            t(
                "fn f(a: u8) -> Vec<String> { vec![format!(\"{}\", if a > 1 { 1 } else { 2 }), format!(\"{}\", 2), format!(\"{}\", 3), format!(\"{}\", 4)] }"
            ),
            None
        );
        assert_eq!(
            t(
                "fn f(o: O) -> String { match o { O::A => format!(\"{}\", { let y = 1; y }), O::B => s(), O::C => s(), O::D => s() } }"
            ),
            None
        );
        assert_eq!(t("fn f() -> Vec<u8> { vec![m! { a; b }, 1, 2, 3] }"), None);
        // Only the literal is excused: a match table's arms, not its
        // scrutinee.
        let src = "fn f(x: u8) -> u8 {\n    match {\n        g(x)\n    } {\n        0 => 1,\n        1 => 2,\n        2 => 3,\n        _ => 0,\n    }\n}";
        let Some(Shape::Table(t)) = shape_of(src, "f") else {
            panic!("not a table")
        };
        assert_eq!((t.start_line, t.end_line), (5, 8));
    }

    #[test]
    fn table_entries_of_every_simple_kind() {
        for src in [
            "fn f() -> [i8; 4] { [-1, -2, -3, -4] }",
            "fn f() -> [&u8; 4] { [&A, &B, &C, &D] }",
            "fn f() -> [u8; 4] { [(1), (2), (3), (4)] }",
            "fn f() -> [u8; 4] { [1 as u8, 2 as u8, 3 as u8, 4 as u8] }",
            "fn f() -> [u8; 4] { [a.x, a.y, a.z, a.w] }",
            "fn f() -> [[u8; 2]; 4] { [[1, 2], [3, 4], [5, 6], [7, 8]] }",
            "fn f() -> Vec<S> { vec![a.into(), b.into(), c.into(), d.into()] }",
            "fn f() -> R { R { a: 1, b: 2, c: 3, d: 4 } }",
            // The literal may sit in parentheses; calls nest 4 deep.
            "fn f() -> [u8; 4] { ([1, 2, 3, 4]) }",
            "fn f() -> Vec<u8> { vec![g(g(g(g(1)))), 2, 3, 4] }",
        ] {
            assert_eq!(t(src), Some(4), "{src}");
        }
    }

    #[test]
    fn table_entries_hide_no_logic() {
        for src in [
            // Logic in a call's, a method's or a struct's argument.
            "fn f() -> Vec<u8> { vec![g(a + 1), g(2), g(3), g(4)] }",
            "fn f() -> Vec<u8> { vec![a.g(b + 1), a.g(2), a.g(3), a.g(4)] }",
            "fn f() -> Vec<R> { vec![R { x: a + 1, ..D }, R { x: 1, ..D }, R { x: 2, ..D }, R { x: 3, ..D }] }",
            "fn f() -> R { R { a: 1, b: 2, c: 3, d: 4, ..make(x + 1) } }",
            // Too deep; only `vec!` among macros; one wrapping call only.
            "fn f() -> Vec<u8> { vec![g(g(g(g(g(1))))), 2, 3, 4] }",
            "fn f() -> X { foo![1, 2, 3, 4] }",
            "fn f() -> X { A::from(B::from([1, 2, 3, 4])) }",
            // Logic in a match table's scrutinee counts against the whole
            // table, though only its arms would be excused (see plan.md).
            "fn f(x: u8, c: bool) -> u8 { match if c { x } else { 0 } { 0 => 1, 1 => 2, 2 => 3, _ => 0 } }",
        ] {
            assert_eq!(t(src), None, "{src}");
        }
    }

    #[test]
    fn macro_entry_tokens_read_as_data_without_blocks_or_logic() {
        let entry = |tokens: &str| {
            t(&format!(
                "fn f() -> Vec<u8> {{ vec![n![{tokens}], 1, 2, 3] }}"
            ))
        };
        for tokens in ["a (b c)", "a & b c", "a | b c", "a::b c"] {
            assert_eq!(entry(tokens), Some(4), "{tokens}");
        }
        for tokens in ["a {b} c", "a && b c", "a || b c"] {
            assert_eq!(entry(tokens), None, "{tokens}");
        }
    }

    #[test]
    fn dispatches_measure_each_arm_and_find_preludes() {
        let src = "fn f(c: C) -> u8 {
    match c {
        C::A(x) => {
            let r = match go(x, 1) {
                Ok(r) => r,
                Err(e) => return e,
            };
            r + 1
        }
        C::B { y } => {
            let r = match go(y, 2) {
                Ok(r) => r,
                Err(e) => return e,
            };
            r * 2
        }
        C::D if ready() => {
            if a { if b { 1 } else { 2 } } else { 3 }
        }
        C::E | C::F => 0,
    }
}";
        let Some(Shape::Dispatch(d)) = shape_of(src, "f") else {
            panic!("not a dispatch")
        };
        let names: Vec<&str> = d.arms.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["C::A", "C::B", "C::D", "C::E | C::F"]);
        assert_eq!((d.start_line, d.end_line), (3, 20));
        assert_eq!(d.remainder.complexity, 1);
        // Measured from inside the arm: the guard adds nothing by itself,
        // the two ifs add 2 and nest 2 deep.
        let worst = &d.arms[2];
        assert_eq!((worst.line, worst.end_line), (17, 19));
        assert_eq!((worst.metrics.complexity, worst.metrics.nesting), (3, 2));
        assert_eq!(d.arms[0].metrics.complexity, 2);
        assert_eq!(
            d.repeat,
            Some(Repeat {
                statements: 1,
                count: 2,
                at: vec![4, 11]
            })
        );
        // Three arms is ordinary branching.
        assert_eq!(
            shape_of(
                "fn g(x: u8) -> u8 { match x { 0 => a(), 1 => b(), _ => { c(); d() } } }",
                "g"
            ),
            None
        );
    }

    #[test]
    fn the_remainder_and_wrapped_matches() {
        // The closure in the lead `let` is the fn's, not an arm's.
        let src = "fn f(c: u8, x: u32) -> Result<u32, ()> {
    let k = |a: u32| if a > 1 { if a > 2 { 1 } else { 2 } } else { 3 };
    Ok(match c {
        0 => k(x),
        1 => k(x + 1),
        2 => x * 2,
        _ => 0,
    })
}";
        let Some(Shape::Dispatch(d)) = shape_of(src, "f") else {
            panic!("not a dispatch")
        };
        assert_eq!((d.remainder.complexity, d.remainder.nesting), (3, 3));
        assert!(d.arms.iter().all(|a| a.metrics.complexity == 1));
    }

    #[test]
    fn settle_drops_tables_with_a_big_entry() {
        let table = |spans: Vec<(usize, usize)>| {
            Some(Shape::Table(Table {
                entries: spans.len(),
                start_line: 1,
                end_line: 100,
                lines: 0,
                entry_spans: spans,
            }))
        };
        let lines = |s: usize, e: usize| e + 1 - s;
        let mut t = table(vec![(1, 12), (13, 14), (15, 16), (17, 18)]);
        settle(&mut t, lines);
        assert!(t.is_some());
        let mut t = table(vec![(1, 13), (14, 15), (16, 17), (18, 19)]);
        settle(&mut t, lines);
        assert_eq!(t, None);
        // One line per entry is enough.
        let mut t = Some(Shape::Table(Table {
            entries: 4,
            start_line: 1,
            end_line: 4,
            lines: 0,
            entry_spans: vec![(1, 1), (2, 2), (3, 3), (4, 4)],
        }));
        settle(&mut t, lines);
        assert!(t.is_some());
    }

    #[test]
    fn small_arms_are_short_and_simple() {
        let arm = |body_lines, complexity| Arm {
            name: "A".into(),
            line: 1,
            end_line: 1,
            lines: 0,
            body_line: 1,
            body_lines,
            metrics: FnMetrics {
                complexity,
                ..Default::default()
            },
        };
        assert!(arm(12, 3).small());
        assert!(!arm(3, 4).small());
        assert!(!arm(13, 1).small());
    }

    #[test]
    fn settle_keeps_only_dispatches_of_small_arms() {
        let arm = |lines, complexity| Arm {
            name: "A".into(),
            line: 1,
            end_line: lines,
            lines: 0,
            body_line: 1,
            body_lines: 0,
            metrics: FnMetrics {
                complexity,
                ..Default::default()
            },
        };
        let dispatch = |arms: Vec<Arm>| {
            Some(Shape::Dispatch(Dispatch {
                arms,
                start_line: 1,
                end_line: 100,
                lines: 0,
                remainder: FnMetrics::default(),
                repeat: None,
            }))
        };
        let lines = |s: usize, e: usize| e + 1 - s;
        // The upper median arm decides: one big arm is fine...
        let mut s = dispatch(vec![arm(3, 1), arm(12, 3), arm(4, 1), arm(90, 20)]);
        settle(&mut s, lines);
        let Some(Shape::Dispatch(d)) = &s else {
            panic!("dropped")
        };
        assert_eq!((d.lines, d.arms[3].lines), (100, 90));
        // ...medium ones throughout aren't (by lines, or by complexity).
        for arms in [
            vec![arm(3, 1), arm(13, 1), arm(14, 1), arm(15, 1)],
            vec![arm(3, 1), arm(3, 4), arm(3, 4), arm(3, 1)],
        ] {
            let mut s = dispatch(arms);
            settle(&mut s, lines);
            assert_eq!(s, None);
        }
    }

    #[test]
    fn preludes_need_substance() {
        let arms = |body: &str| {
            format!(
                "fn f(m: M) -> u32 {{ match m {{ M::A(p) => {{ {body} 1 }} M::B(p) => {{ {body} 2 }} M::C => 3, M::D => 4 }} }}"
            )
        };
        let repeat = |src: &str| match shape_of(src, "f") {
            Some(Shape::Dispatch(d)) => d.repeat,
            _ => panic!("not a dispatch"),
        };
        // Plain bindings: nothing to extract.
        assert_eq!(repeat(&arms("let a = p; let b = p; let c = 1;")), None);
        // A call with enough tokens, over two statements.
        let r = repeat(&arms("let a = load(p, 1).unwrap_or(0); let b = a + 1;"));
        assert_eq!(r.map(|r| (r.statements, r.count)), Some((2, 2)));
        for (body, expected) in [
            // Enough tokens with a call, `?` or control flow in them...
            ("let a = -f(p, q, r);", Some((1, 2))),
            ("let a = p + q + r + s + t; f();", Some((2, 2))),
            ("let a = p.q?; let b = a.r?;", Some((2, 2))),
            // ...not a short call, nor brackets that aren't a call.
            ("let a = f(p);", None),
            ("let a = (p, q, r, s, t, u, v);", None),
            ("let a = [p, q, r, s, t, u, v, w];", None),
            ("(a, b) = (p, q, r, s, t, u); let c = a;", None),
        ] {
            let r = repeat(&arms(body));
            assert_eq!(r.map(|r| (r.statements, r.count)), expected, "{body}");
        }
        // An arm's only statement is its value, not a prelude.
        let one = "fn f(m: M) -> u32 { match m { M::A(p) => { load(p, 1, 2, 3, 4, 5) } M::B(p) => { load(p, 1, 2, 3, 4, 5) } M::C => 3, M::D => 4 } }";
        assert_eq!(repeat(one), None);
    }

    #[test]
    fn the_largest_group_of_repeated_preludes_wins() {
        let src = "fn f(m: M) -> u32 { match m {
            M::A(p) => { let a = load(p, 1, 2, 3); 1 }
            M::B(p) => { let a = load(p, 1, 2, 3); 2 }
            M::C(p) => { let x = p.get(0).unwrap_or(9); 3 }
            M::D(p) => { let x = p.get(0).unwrap_or(9); 4 }
            M::E(p) => { let x = p.get(0).unwrap_or(9); 5 }
            M::F => 6 } }";
        // Groups come out of a HashMap: repeat so either order is seen.
        for _ in 0..16 {
            let Some(Shape::Dispatch(d)) = shape_of(src, "f") else {
                panic!("not a dispatch")
            };
            assert_eq!(d.repeat.map(|r| r.count), Some(3));
        }
    }

    #[test]
    fn pattern_names() {
        for (pat, name) in [
            ("Call::Donate(request)", "Call::Donate"),
            ("Ok(Reply::Error(Failure::Busy { id }))", "Failure::Busy"),
            ("Some(x)", "Some"),
            ("C::E | C::F", "C::E | C::F"),
            ("_", "_"),
            ("(State::Idle, Ev::Go)", "(State::Idle, Ev::Go)"),
            ("Foo::<u8>::Bar { .. }", "Foo::Bar"),
            ("&Foo::Baz(ref q)", "Foo::Baz"),
            ("x @ \"z\"", "\"z\""),
        ] {
            let p = syn::parse::Parser::parse_str(Pat::parse_multi, pat).unwrap();
            assert_eq!(pat_name(&p), name, "{pat}");
        }
    }

    #[test]
    fn allowed_lints() {
        let f = facts(
            "#[allow(clippy::too_many_lines, dead_code)]\n#[expect(clippy::cognitive_complexity, reason = \"x\")]\nfn f() {}\n#[allow(clippy::needless_pass_by_value)]\nfn g() {}",
        );
        assert_eq!(
            fact(&f, "f").allowed,
            [Lint::TooManyLines, Lint::CognitiveComplexity]
        );
        assert!(fact(&f, "g").allowed.is_empty());
        assert!(!fact(&f, "f").exempt);
        // Inside cfg_attr, whatever the condition; groups don't count.
        let f = facts(
            "#[cfg_attr(feature = \"x\", allow(clippy::too_many_lines))]\nfn f() {}\n#[allow(clippy::all, clippy::pedantic)]\nfn g() {}",
        );
        assert_eq!(fact(&f, "f").allowed, [Lint::TooManyLines]);
        assert!(fact(&f, "g").allowed.is_empty());
        // `any()` never holds.
        let f = facts("#[cfg_attr(any(), allow(clippy::too_many_lines))]\nfn f() {}");
        assert!(fact(&f, "f").allowed.is_empty());
    }
}
