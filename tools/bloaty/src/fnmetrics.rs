//! Per-fn measures (complexity, nesting, params, panic sites, clones): a
//! second, small walk over each non-test fn body, separate from the main
//! visitor in `visit`.

use crate::visit::syntax::last_segment;
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde::Serialize;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Block, Expr, FnArg, Item, Token};

/// One panic site counted in `FnMetrics::panics`.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct PanicSite {
    pub line: usize,
    /// 1-based, in characters.
    pub col: usize,
    /// `unwrap`, `expect`, or the macro with its `!` (`panic!`, `assert_eq!`).
    pub kind: String,
}

/// Per-fn measures over the body, closures included and nested items
/// (fns, impls, mods) left out: those are measured on their own.
#[derive(Serialize, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct FnMetrics {
    /// Cyclomatic complexity, counted like this: 1, plus one per `if` (each
    /// `else if` too), `match` (one, however many arms: a flat match reads
    /// as one decision), `while`/`for`/`loop`, `&&`/`||` and `let .. else`.
    /// `?` isn't counted: early returns on error are the normal path. Macro
    /// arguments count when they parse as comma-separated expressions
    /// (`format!`, `vec!`, `assert!`), not otherwise (`tokio::select!`).
    #[serde(rename = "cyclomatic_complexity")]
    pub complexity: u32,
    /// Deepest nesting of `if`/`match`/loops/closures/`let .. else`/block
    /// expressions (`{ }`, `unsafe`, `async`, `const`) in the body: 0 for
    /// straight-line code, 1 inside one `if`. An `else if` and match guards
    /// sit at their construct's depth, and a block that is a construct's own
    /// body (a match arm's or closure's `{ }`) doesn't add a level.
    pub nesting: u32,
    /// Parameters, `self` excluded.
    pub params: u32,
    /// `.unwrap()`, `.expect(x)` (not `.expect(x)?`), `panic!`,
    /// `unreachable!`, `todo!`, `unimplemented!`, `assert!`, `assert_eq!`,
    /// `assert_ne!`, also inside macro arguments.
    pub panics: u32,
    /// `.clone()` calls, also inside macro arguments.
    pub clones: u32,
}

/// Macros that panic in release builds (`debug_assert*` compile out).
const PANIC_MACROS: &[&str] = &[
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "assert",
    "assert_eq",
    "assert_ne",
];

pub fn fn_metrics(sig: &syn::Signature, block: &Block) -> (FnMetrics, Vec<PanicSite>) {
    let mut m = MetricsWalk {
        skip: None,
        m: FnMetrics {
            complexity: 1,
            params: sig
                .inputs
                .iter()
                .filter(|a| matches!(a, FnArg::Typed(_)))
                .count() as u32,
            ..Default::default()
        },
        depth: 0,
        sites: Vec::new(),
    };
    m.visit_block(block);
    m.sites.sort_by_key(|s| (s.line, s.col));
    m.m.panics = m.sites.len() as u32;
    (m.m, m.sites)
}

/// A match arm measured like a fn: complexity from 1 (its guard counts),
/// nesting from inside the arm. Panics and clones are the fn's.
pub fn arm_metrics(arm: &syn::Arm) -> FnMetrics {
    let mut m = MetricsWalk {
        skip: None,
        m: FnMetrics {
            complexity: 1,
            ..Default::default()
        },
        depth: 0,
        sites: Vec::new(),
    };
    if let Some((_, guard)) = &arm.guard {
        m.visit_expr(guard);
    }
    match &*arm.body {
        // The arm's own `{ }` adds no level, as a fn body's doesn't.
        Expr::Block(b) if b.label.is_none() => m.visit_block(&b.block),
        body => m.visit_expr(body),
    }
    m.m.panics = m.sites.len() as u32;
    m.m
}

/// The cyclomatic complexity of one expression, counted from 1.
pub fn expr_complexity(e: &Expr) -> u32 {
    let mut m = MetricsWalk {
        skip: None,
        m: FnMetrics {
            complexity: 1,
            ..Default::default()
        },
        depth: 0,
        sites: Vec::new(),
    };
    m.visit_expr(e);
    m.m.complexity
}

/// A dispatch fn's body measured without its match's arms (they are
/// measured on their own): the lets before the match, its scrutinee and the
/// fn's base complexity of 1. The match itself adds nothing.
pub fn remainder_metrics(block: &Block, dispatch: &syn::ExprMatch) -> FnMetrics {
    let mut m = MetricsWalk {
        skip: Some(dispatch as *const syn::ExprMatch),
        m: FnMetrics {
            complexity: 1,
            ..Default::default()
        },
        depth: 0,
        sites: Vec::new(),
    };
    m.visit_block(block);
    m.m.panics = m.sites.len() as u32;
    m.m
}

/// A second, small walk over one fn body: the main visitor's bookkeeping
/// (test depth, refs) doesn't fit counting per fn, and nested fns must
/// start from zero.
struct MetricsWalk {
    /// A match (by address) whose arms are left out: `remainder_metrics`.
    skip: Option<*const syn::ExprMatch>,
    m: FnMetrics,
    depth: u32,
    /// The panic sites; their count becomes `m.panics`.
    sites: Vec<PanicSite>,
}

impl MetricsWalk {
    fn panic_at(&mut self, span: proc_macro2::Span, kind: String) {
        let at = span.start();
        self.sites.push(PanicSite {
            line: at.line,
            col: at.column + 1,
            kind,
        });
    }

    fn nested(&mut self, f: impl FnOnce(&mut Self)) {
        self.depth += 1;
        self.m.nesting = self.m.nesting.max(self.depth);
        f(self);
        self.depth -= 1;
    }

    /// A construct's body: a `{ }` here is the construct's, not a level.
    fn body(&mut self, e: &Expr) {
        match e {
            Expr::Block(b) if b.label.is_none() => self.visit_block(&b.block),
            _ => self.visit_expr(e),
        }
    }

    /// Panic sites and clones in macro tokens that aren't expressions
    /// (`tokio::select!`, `vec![x; n]`, items): `. unwrap ( )`,
    /// `. expect ( x )` not followed by `?`, `. clone ( )` and `panic ! ..`.
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let toks: Vec<TokenTree> = tokens.into_iter().collect();
        for (i, tt) in toks.iter().enumerate() {
            match tt {
                TokenTree::Group(g) => self.scan_tokens(g.stream()),
                TokenTree::Ident(id) => {
                    if let Some(kind) = token_panic(&toks, i, id) {
                        self.panic_at(id.span(), kind);
                    }
                    let clone = id == "clone" && call_args(&toks, i) == Some(0);
                    self.m.clones += u32::from(clone && punct_at(&toks, i.wrapping_sub(1), '.'));
                }
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for MetricsWalk {
    fn visit_item(&mut self, _: &'ast Item) {}

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        self.m.complexity += 1;
        self.visit_expr(&e.cond);
        self.nested(|v| v.visit_block(&e.then_branch));
        match e.else_branch.as_ref().map(|(_, e)| &**e) {
            Some(Expr::If(elif)) => self.visit_expr_if(elif),
            Some(other) => self.nested(|v| v.body(other)),
            None => {}
        }
    }

    fn visit_expr_match(&mut self, e: &'ast syn::ExprMatch) {
        if self.skip.is_some_and(|p| std::ptr::eq(p, e)) {
            return self.visit_expr(&e.expr);
        }
        self.m.complexity += 1;
        self.visit_expr(&e.expr);
        // Guards sit at the match's own depth, like its scrutinee.
        for (_, guard) in e.arms.iter().filter_map(|a| a.guard.as_ref()) {
            self.visit_expr(guard);
        }
        self.nested(|v| e.arms.iter().for_each(|a| v.body(&a.body)));
    }

    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.m.complexity += 1;
        self.visit_expr(&e.cond);
        self.nested(|v| v.visit_block(&e.body));
    }

    fn visit_expr_for_loop(&mut self, e: &'ast syn::ExprForLoop) {
        self.m.complexity += 1;
        self.visit_expr(&e.expr);
        self.nested(|v| v.visit_block(&e.body));
    }

    fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
        self.m.complexity += 1;
        self.nested(|v| v.visit_block(&e.body));
    }

    fn visit_expr_closure(&mut self, e: &'ast syn::ExprClosure) {
        self.nested(|v| v.body(&e.body));
    }

    fn visit_expr_block(&mut self, e: &'ast syn::ExprBlock) {
        self.nested(|v| v.visit_block(&e.block));
    }

    fn visit_expr_unsafe(&mut self, e: &'ast syn::ExprUnsafe) {
        self.nested(|v| v.visit_block(&e.block));
    }

    fn visit_expr_async(&mut self, e: &'ast syn::ExprAsync) {
        self.nested(|v| v.visit_block(&e.block));
    }

    fn visit_expr_const(&mut self, e: &'ast syn::ExprConst) {
        self.nested(|v| v.visit_block(&e.block));
    }

    fn visit_local_init(&mut self, l: &'ast syn::LocalInit) {
        self.visit_expr(&l.expr);
        if let Some((_, diverge)) = &l.diverge {
            self.m.complexity += 1;
            self.nested(|v| v.body(diverge));
        }
    }

    fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
        if matches!(e.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.m.complexity += 1;
        }
        visit::visit_expr_binary(self, e);
    }

    fn visit_expr_try(&mut self, e: &'ast syn::ExprTry) {
        // `p.expect(tok)?` is a parser's fallible method, not
        // `Option::expect`.
        match &*e.expr {
            Expr::MethodCall(m) if m.method == "expect" && m.args.len() == 1 => {
                visit::visit_expr_method_call(self, m)
            }
            _ => visit::visit_expr_try(self, e),
        }
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        let (m, n) = (&e.method, e.args.len());
        if (m == "unwrap" && n == 0) || (m == "expect" && n == 1) {
            self.panic_at(m.span(), m.to_string());
        } else if m == "clone" && n == 0 {
            self.m.clones += 1;
        }
        visit::visit_expr_method_call(self, e);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let name = last_segment(&m.path);
        if PANIC_MACROS.contains(&name.as_str()) {
            let span = m
                .path
                .segments
                .last()
                .map_or(m.path.span(), |s| s.ident.span());
            self.panic_at(span, format!("{name}!"));
        }
        // Most macros take expressions (`format!`, `vec!`, `assert!`): walk
        // them like code so their branches and closures count.
        match Punctuated::<Expr, Token![,]>::parse_terminated.parse2(m.tokens.clone()) {
            Ok(args) => args.iter().for_each(|a| self.visit_expr(a)),
            Err(_) => self.scan_tokens(m.tokens.clone()),
        }
    }
}

/// Comma-separated arguments in a call's parentheses (a trailing comma
/// doesn't add one).
/// Whether `toks[j]` is the punctuation `c`.
fn punct_at(toks: &[TokenTree], j: usize, c: char) -> bool {
    matches!(toks.get(j), Some(TokenTree::Punct(p)) if p.as_char() == c)
}

/// The number of arguments of a call right after `toks[i]`, if one follows.
fn call_args(toks: &[TokenTree], i: usize) -> Option<usize> {
    match toks.get(i + 1) {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
            Some(top_level_args(g.stream()))
        }
        _ => None,
    }
}

/// The panic kind if the ident `toks[i]` starts a panic site in raw tokens:
/// `. unwrap ( )`, `. expect ( x )` not followed by `?`, `panic ! ..`.
fn token_panic(toks: &[TokenTree], i: usize, id: &proc_macro2::Ident) -> Option<String> {
    let after_dot = punct_at(toks, i.wrapping_sub(1), '.');
    let panic = match call_args(toks, i) {
        Some(0) => after_dot && id == "unwrap",
        Some(1) => after_dot && id == "expect" && !punct_at(toks, i + 2, '?'),
        Some(_) => false,
        None => !after_dot && punct_at(toks, i + 1, '!') && PANIC_MACROS.iter().any(|m| id == m),
    };
    panic.then(|| {
        if after_dot {
            id.to_string()
        } else {
            format!("{id}!")
        }
    })
}

fn top_level_args(ts: TokenStream) -> usize {
    let mut n = 0;
    let mut pending = false;
    for tt in ts {
        match tt {
            TokenTree::Punct(p) if p.as_char() == ',' => {
                n += usize::from(pending);
                pending = false;
            }
            _ => pending = true,
        }
    }
    n + usize::from(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fact, facts};

    fn metrics(src: &str, name: &str) -> FnMetrics {
        fact(&facts(src), name)
            .metrics
            .unwrap_or_else(|| panic!("{name} not measured"))
    }

    #[test]
    fn complexity_counts_branches() {
        let cc = |body: &str| metrics(&format!("fn f(x: u32) {{ {body} }}"), "f").complexity;
        assert_eq!(cc("let y = x + 1;"), 1);
        assert_eq!(cc("if x > 1 { a() }"), 2);
        assert_eq!(cc("if x > 1 { a() } else if x > 2 { b() } else { c() }"), 3);
        assert_eq!(cc("match x { 0 => a(), 1 => b(), _ => c() }"), 2);
        assert_eq!(cc("match x { _ => a() }"), 2);
        assert_eq!(cc("while x > 0 {} for i in v {} loop { break }"), 4);
        assert_eq!(cc("if x > 1 && x < 5 || x == 9 { a() }"), 4);
        assert_eq!(cc("let y = g()?; h(y?)?;"), 1);
        assert_eq!(cc("let Some(y) = g() else { return };"), 2);
        // Guards add nothing themselves; their `&&` do count.
        assert_eq!(cc("match x { n if n > 1 && n < 3 => a(), _ => b() }"), 3);
    }

    #[test]
    fn closures_count_toward_their_fn_and_nested_fns_dont() {
        let src = "fn outer(v: Vec<u32>) {\n    v.iter().for_each(|x| if *x > 1 { a() });\n    fn inner(x: u32) { if x > 1 { b() } if x > 2 { c() } }\n    impl S { fn m(&self) { if t { d() } } }\n}";
        let f = facts(src);
        let m = |n: &str| fact(&f, n).metrics.unwrap();
        assert_eq!(m("outer").complexity, 2);
        assert_eq!(m("outer").nesting, 2);
        assert_eq!(m("inner").complexity, 3);
        assert_eq!(m("S::m").complexity, 2);
    }

    #[test]
    fn nesting_depth() {
        let depth = |body: &str| metrics(&format!("fn f() {{ {body} }}"), "f").nesting;
        assert_eq!(depth("a(); b();"), 0);
        assert_eq!(depth("if x { a() }"), 1);
        // else-if chains stay at one level.
        assert_eq!(depth("if x { a() } else if y { b() } else { c() }"), 1);
        assert_eq!(depth("for i in v { if x { a() } }"), 2);
        // A match arm's own `{ }` is not a level; a block inside it is.
        assert_eq!(depth("match x { A => { if y { a() } } _ => {} }"), 2);
        assert_eq!(depth("match x { A => { { a() } } _ => {} }"), 2);
        // A labelled block is a level, even as an arm's body.
        assert_eq!(depth("match x { A => 'l: { a() } _ => {} }"), 2);
        let arm: syn::Arm = syn::parse_str("A => 'l: { if x { 1 } else { 2 } }").unwrap();
        assert_eq!(arm_metrics(&arm).nesting, 2);
        assert_eq!(depth("let g = || { if x { a() } };"), 2);
        assert_eq!(depth("unsafe { a() }"), 1);
        assert_eq!(depth("let y = async { loop { if z { a() } } };"), 3);
        assert_eq!(depth("let Some(y) = o else { if x { a() } return };"), 2);
        assert_eq!(depth("let y = const { 1 };"), 1);
        // Guards sit at the match's depth.
        assert_eq!(
            depth("match x { n if (|| { if n > 1 { true } else { false } })() => 1, _ => 0 }"),
            2
        );
    }

    #[test]
    fn params_exclude_self() {
        let src = "struct S;\nimpl S { fn m(&self, a: u8, (b, c): (u8, u8)) {} }\nfn f() {}";
        assert_eq!(metrics(src, "S::m").params, 2);
        assert_eq!(metrics(src, "f").params, 0);
    }

    #[test]
    fn panic_sites_and_clones_incl_macro_arguments() {
        let src = r#"
            fn f(x: Option<u8>, r: Result<u8, E>, s: String) {
                x.unwrap();
                r.expect("ok");
                x.unwrap_or(0);
                x.expect_none_ish(1, 2);
                println!("{} {}", x.unwrap(), r.clone().expect("e"));
                vec![s.clone(), s.clone()];
                assert!(x.map(|v| { if v > 1 { panic!("big") } v }).is_some());
                std::panic!("boom");
                if false { unreachable!() } else { todo!() }
                unimplemented!();
                let t = s.clone();
                let c = Clone::clone(&s);
                let _ = "x.unwrap() in a string";
            }
            "#;
        let m = metrics(src, "f");
        assert_eq!(m.panics, 10);
        assert_eq!(fact(&facts(src), "f").panic_sites.len(), 10);
        assert_eq!(m.clones, 4);
    }

    #[test]
    fn panic_sites_say_where_and_what() {
        let src = "fn f(x: Option<u8>, r: R) {\n    let a = x.unwrap();\n    println!(\"{}\", r.expect(\"e\"));\n    p.expect(t)?;\n    std::assert_eq!(a, 1);\n    tokio::select! { v = rx => v.unwrap(), }\n    if a > 1 { unreachable!() }\n    debug_assert!(a > 0);\n}\n#[cfg(test)]\nfn t() { None::<u8>.unwrap(); }";
        let f = facts(src);
        let sites: Vec<_> = fact(&f, "f")
            .panic_sites
            .iter()
            .map(|s| (s.line, s.col, s.kind.as_str()))
            .collect();
        assert_eq!(
            sites,
            vec![
                (2, 15, "unwrap"),
                (3, 22, "expect"),
                (5, 10, "assert_eq!"),
                (6, 34, "unwrap"),
                (7, 16, "unreachable!"),
            ]
        );
        assert_eq!(fact(&f, "f").metrics.unwrap().panics, 5);
        assert!(fact(&f, "t").panic_sites.is_empty());
    }

    #[test]
    fn asserts_panic_in_release_debug_asserts_dont() {
        let src = "fn f(x: u8) {\n    assert!(x > 1);\n    assert_eq!(x, 2, \"msg\");\n    std::assert_ne!(x, 3);\n    debug_assert!(x > 1);\n    debug_assert_eq!(x, 2);\n}";
        assert_eq!(metrics(src, "f").panics, 3);
    }

    #[test]
    fn expression_macro_arguments_count_like_code() {
        let m = |body: &str| metrics(&format!("fn f(x: u8) {{ {body} }}"), "f");
        let a = m("println!(\"{}\", if x > 1 { 1 } else { 2 });");
        assert_eq!((a.complexity, a.nesting), (2, 1));
        let a = m("assert!(v.iter().any(|y| *y > 1 && *y < 3), \"{}\", g()?);");
        assert_eq!((a.complexity, a.nesting, a.panics), (2, 1, 1));
        // Nested macros are walked once, not also token-scanned.
        let a = m("vec![format!(\"{}\", x.unwrap()), s.clone()];");
        assert_eq!((a.panics, a.clones), (1, 1));
        // Not expressions: only panic sites and clones are seen.
        let a = m("tokio::select! { v = rx.recv() => if v { v.unwrap() } else { w.clone() }, }");
        assert_eq!((a.complexity, a.nesting, a.panics, a.clones), (1, 0, 1, 1));
    }

    #[test]
    fn fallible_expect_is_not_a_panic() {
        let m = |body: &str| metrics(&format!("fn f() -> R {{ {body} }}"), "f");
        assert_eq!(m("p.expect(Token::Comma)?; Ok(())").panics, 0);
        assert_eq!(m("p.expect(Token::Comma)?; Ok(())").complexity, 1);
        // Other `?` operands are walked as usual.
        assert_eq!(
            m("let x = h(if a { 1 } else { 2 })?; Some(x)").complexity,
            2
        );
        assert_eq!(m("let x = o.unwrap()?; Some(x)").panics, 1);
    }

    #[test]
    fn token_scan_finds_calls_by_their_arguments() {
        // `a;` first keeps `p.expect` off token 2, where i + 2 == i * 2.
        let src = "fn f() -> R { m!(a; p.expect(t)?; q.expect(a, b); r.expect(\"x\"); s.expect(c::MSG); t.expect[i]; panic!(\"y\"); unreachable; format!(\"z\")) }";
        let f = facts(src);
        let kinds: Vec<_> = fact(&f, "f")
            .panic_sites
            .iter()
            .map(|s| s.kind.as_str())
            .collect();
        assert_eq!(kinds, ["expect", "expect", "panic!"]);
    }

    #[test]
    fn expr_complexity_counts_from_one() {
        let cc = |src: &str| expr_complexity(&syn::parse_str(src).unwrap());
        assert_eq!(cc("x"), 1);
        assert_eq!(cc("if c { x } else { 0 }"), 2);
    }

    #[test]
    fn statement_item_macros_count_toward_their_fn() {
        // syn sees `m! { .. }` in a body as a statement macro, so `inner`
        // gets no fact of its own: its panic sites land in `outer`.
        let f = facts("fn outer() { my_macro! { fn inner() { x.unwrap(); } } }");
        assert_eq!(fact(&f, "outer").metrics.unwrap().panics, 1);
        assert!(f.fns.iter().all(|f| f.name != "inner"));
    }
}
