//! The opening of a test body that a setup fn could take
//! (`findings::fixture`): the statements before the first that wouldn't do
//! the same moved into a fn that returns what they bind, each hashed token
//! for token.

use super::body::NEGATING;
use super::macros::{EXPRESSION_MACROS, expression_args};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use syn::visit::{self, Visit};
use syn::{Block, Expr, Pat, Stmt};

/// A setup fn takes at least this many statements.
pub const MIN_FIXTURE_STATEMENTS: usize = 4;

/// A statement of a test's opening: its exact hash (tokens as written),
/// weight and lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TestStmt {
    pub hash: u64,
    /// Tokens, as `shape::weigh` counts them.
    pub tokens: usize,
    /// It does something (`acts`).
    pub active: bool,
    pub first_line: usize,
    pub last_line: usize,
    /// Lines on which one of its tokens starts or a delimiter closes.
    pub lines: usize,
}

/// The statements opening a test body that a fn returning what they bind
/// could take, in order: up to the first that `movable` or `may_borrow`
/// rule out, and never the last statement (something must follow). None
/// if that's fewer than `MIN_FIXTURE_STATEMENTS` (nothing to hash then),
/// or if the body declares items at its top level (an item is visible in
/// the whole block, the opening included, and its name could mean that
/// item there but not in a setup fn).
pub(super) fn test_opening(block: &Block) -> Vec<TestStmt> {
    let stmts = &block.stmts;
    if stmts.len() <= MIN_FIXTURE_STATEMENTS || stmts.iter().any(|s| matches!(s, Stmt::Item(_))) {
        return Vec::new();
    }
    let mut bound = HashSet::new();
    let mut n = 0;
    for s in &stmts[..stmts.len() - 1] {
        if !movable(s) || may_borrow(s, &bound) {
            break;
        }
        n += 1;
        if let Stmt::Local(l) = s {
            pat_names(&l.pat, &mut |n| {
                bound.insert(n);
            });
        }
    }
    if n < MIN_FIXTURE_STATEMENTS {
        return Vec::new();
    }
    stmts[..n]
        .iter()
        .map(|s| {
            let mut k = Key {
                hash: DefaultHasher::new(),
                tokens: 0,
                first_line: 0,
                last_line: 0,
                lines: 0,
            };
            k.add(s.to_token_stream());
            TestStmt {
                hash: k.hash.finish(),
                tokens: k.tokens,
                active: acts(s),
                first_line: k.first_line,
                last_line: k.last_line,
                lines: k.lines,
            }
        })
        .collect()
}

/// A statement's tokens as written, hashed (delimiters, identifiers,
/// punctuation, literals) and counted (as `shape::weigh` does: a group is
/// one token plus its own), and the lines one starts or a delimiter
/// closes on: tokens come in source order, so lines never go back and
/// counting new ones is enough.
struct Key {
    hash: DefaultHasher,
    tokens: usize,
    first_line: usize,
    last_line: usize,
    lines: usize,
}

impl Key {
    fn line(&mut self, line: usize) {
        if self.lines == 0 {
            self.first_line = line;
        }
        if self.lines == 0 || line > self.last_line {
            self.lines += 1;
            self.last_line = line;
        }
    }

    fn add(&mut self, ts: TokenStream) {
        for tt in ts {
            self.tokens += 1;
            match tt {
                TokenTree::Group(g) => {
                    self.line(g.span_open().start().line);
                    (g.delimiter() as u8).hash(&mut self.hash);
                    self.add(g.stream());
                    0xffu8.hash(&mut self.hash);
                    self.line(g.span_close().start().line);
                }
                TokenTree::Ident(i) => {
                    self.line(i.span().start().line);
                    i.to_string().hash(&mut self.hash);
                }
                TokenTree::Punct(p) => {
                    self.line(p.span().start().line);
                    p.as_char().hash(&mut self.hash);
                }
                TokenTree::Literal(l) => {
                    self.line(l.span().start().line);
                    l.to_string().hash(&mut self.hash);
                }
            }
        }
    }
}

/// Whether a statement does something beyond binding fresh values: any
/// statement but a `let`, or a `let` whose value calls with arguments,
/// invokes a macro with some input, awaits, uses `?` or branches.
/// `let mut buf = Vec::new();` doesn't: four of those are no setup.
fn acts(stmt: &Stmt) -> bool {
    struct V(bool);
    impl<'ast> Visit<'ast> for V {
        fn visit_expr(&mut self, e: &'ast Expr) {
            self.0 |= match e {
                Expr::Call(c) => !c.args.is_empty(),
                Expr::MethodCall(m) => !m.args.is_empty(),
                Expr::Macro(m) => !m.mac.tokens.is_empty(),
                Expr::Await(_) | Expr::Try(_) | Expr::If(_) | Expr::Match(_) => true,
                Expr::ForLoop(_) | Expr::While(_) | Expr::Loop(_) => true,
                Expr::Return(_) | Expr::Break(_) | Expr::Continue(_) => true,
                _ => false,
            };
            visit::visit_expr(self, e);
        }
    }
    let Stmt::Local(l) = stmt else { return true };
    let mut v = V(l.init.as_ref().is_some_and(|i| i.diverge.is_some()));
    v.visit_local(l);
    v.0
}

/// The names a pattern binds, in order.
fn pat_names(pat: &Pat, out: &mut dyn FnMut(String)) {
    struct V<'a>(&'a mut dyn FnMut(String));
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
            (self.0)(p.ident.to_string());
            visit::visit_pat_ident(self, p);
        }
    }
    V(out).visit_pat(pat);
}

/// The bound name a place starts from: `a`, `a.b`, `a[i]`, `*a`.
fn root<'a>(e: &'a Expr, bound: &HashSet<String>) -> Option<&'a syn::Ident> {
    match e {
        Expr::Path(p) => p
            .path
            .get_ident()
            .filter(|i| bound.contains(&i.to_string())),
        Expr::Field(f) => root(&f.base, bound),
        Expr::Index(i) => root(&i.expr, bound),
        Expr::Paren(p) => root(&p.expr, bound),
        Expr::Unary(u) => root(&u.expr, bound),
        _ => None,
    }
}

/// A place (path, field, index, deref): `&` of one borrows it, of
/// anything else a temporary.
fn place(e: &Expr) -> bool {
    match e {
        Expr::Path(_) => true,
        Expr::Field(f) => place(&f.base),
        Expr::Index(i) => place(&i.expr),
        Expr::Paren(p) => place(&p.expr),
        Expr::Unary(u) => matches!(u.op, syn::UnOp::Deref(_)) && place(&u.expr),
        _ => false,
    }
}

/// A statement whose value may borrow from what an earlier statement of
/// the opening bound (`bound`), as its syntax shows, so a setup fn
/// couldn't return both: a `ref` binding, or on a `let` value's (or an
/// assignment's right side's) own path, not in call arguments: a
/// reference to or index into such a binding, a reference to a temporary
/// (`&open(1)`), a method call on such a binding (`reserving.pool()`;
/// `clone`, `to_owned`, `to_string` and `to_vec` aside: whether another
/// method's result borrows needs types), a closure or async block that
/// doesn't `move`; looking through blocks, `if`/`match` arms and `break`
/// values.
fn may_borrow(stmt: &Stmt, bound: &HashSet<String>) -> bool {
    match stmt {
        Stmt::Local(l) => {
            let mut by_ref = false;
            struct Ref<'a>(&'a mut bool);
            impl<'ast> Visit<'ast> for Ref<'_> {
                fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
                    *self.0 |= p.by_ref.is_some();
                    visit::visit_pat_ident(self, p);
                }
            }
            Ref(&mut by_ref).visit_pat(&l.pat);
            by_ref || l.init.as_ref().is_some_and(|i| value(&i.expr, bound))
        }
        Stmt::Expr(Expr::Assign(a), _) => value(&a.right, bound),
        _ => false,
    }
}

/// See `may_borrow`.
fn value(e: &Expr, bound: &HashSet<String>) -> bool {
    const OWNED: &[&str] = &["clone", "to_owned", "to_string", "to_vec"];
    let rooted = |e: &Expr| root(e, bound).is_some();
    let tail = |b: &Block| match b.stmts.last() {
        Some(Stmt::Expr(e, None)) => value(e, bound),
        _ => false,
    };
    match e {
        Expr::Reference(r) => rooted(&r.expr) || !place(&r.expr) || value(&r.expr, bound),
        Expr::Index(i) => rooted(&i.expr) || value(&i.expr, bound),
        Expr::MethodCall(m) => {
            let owned = OWNED.iter().any(|o| m.method == o);
            (!owned && rooted(&m.receiver)) || value(&m.receiver, bound)
        }
        Expr::Closure(c) => c.capture.is_none(),
        Expr::Async(a) => a.capture.is_none(),
        Expr::Block(b) => tail(&b.block),
        Expr::Unsafe(u) => tail(&u.block),
        Expr::If(i) => {
            tail(&i.then_branch) || i.else_branch.as_ref().is_some_and(|(_, e)| value(e, bound))
        }
        Expr::Match(m) => m.arms.iter().any(|a| value(&a.body, bound)),
        Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_) => {
            // `break` values, wherever they are inside.
            struct Breaks<'b>(&'b HashSet<String>, bool);
            impl<'ast> Visit<'ast> for Breaks<'_> {
                fn visit_expr_break(&mut self, b: &'ast syn::ExprBreak) {
                    self.1 |= b.expr.as_ref().is_some_and(|e| value(e, self.0));
                    visit::visit_expr_break(self, b);
                }
            }
            let mut v = Breaks(bound, false);
            v.visit_expr(e);
            v.1
        }
        Expr::Field(f) => value(&f.base, bound),
        Expr::Await(a) => value(&a.base, bound),
        Expr::Try(t) => value(&t.expr, bound),
        Expr::Paren(p) => value(&p.expr, bound),
        Expr::Group(g) => value(&g.expr, bound),
        Expr::Cast(c) => value(&c.expr, bound),
        Expr::Unary(u) => value(&u.expr, bound),
        Expr::Tuple(t) => t.elems.iter().any(|e| value(e, bound)),
        Expr::Array(a) => a.elems.iter().any(|e| value(e, bound)),
        Expr::Struct(s) => s.fields.iter().any(|f| value(&f.expr, bound)),
        Expr::Macro(m) => {
            expression_args(&m.mac).is_some_and(|args| args.iter().any(|e| value(e, bound)))
        }
        _ => false,
    }
}

/// Whether a statement does the same moved into a fn called in its
/// place: not an item or a `let` without a value (later statements rely
/// on those being in the test's scope), no `return` outside a closure or
/// async block (it would leave the setup fn, not the test), no macro
/// bloaty doesn't read (opaque: it may hide either, or bind names), no
/// `pin!` (pins a value in place: it can't leave the fn), no `#[cfg]` or
/// `#[cfg_attr]` (a binding that may not exist). Closures, async blocks
/// and nested items keep their returns and macros to themselves.
fn movable(stmt: &Stmt) -> bool {
    struct V(bool);
    impl<'ast> Visit<'ast> for V {
        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
        fn visit_expr_async(&mut self, _: &'ast syn::ExprAsync) {}
        fn visit_item(&mut self, _: &'ast syn::Item) {}
        fn visit_expr_return(&mut self, _: &'ast syn::ExprReturn) {
            self.0 = false;
        }
        fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
            self.0 &= !(a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"));
        }
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let name = m.path.segments.last().map(|s| s.ident.to_string());
            self.0 &= name.is_some_and(|n| plain(&n)) && plain_tokens(m.tokens.clone());
        }
    }
    match stmt {
        Stmt::Item(_) => false,
        Stmt::Local(l) if l.init.is_none() => false,
        _ => {
            let mut v = V(true);
            v.visit_stmt(stmt);
            v.0
        }
    }
}

/// An expression macro a setup fn can take (`pin!` aside: it pins in
/// place).
fn plain(name: &str) -> bool {
    name != "pin" && EXPRESSION_MACROS.contains(&name)
}

/// An expression macro's tokens hold no `return` and no other macro than
/// `plain` ones: read as tokens, closures included (stricter than parsed).
fn plain_tokens(ts: TokenStream) -> bool {
    let tokens: Vec<TokenTree> = ts.into_iter().collect();
    tokens.iter().enumerate().all(|(i, tt)| match tt {
        TokenTree::Ident(id) => {
            let invoked = matches!(tokens.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!')
                && matches!(tokens.get(i + 2), Some(TokenTree::Group(_)));
            let name = id.to_string();
            name != "return" && !(invoked && !NEGATING.contains(&name.as_str()) && !plain(&name))
        }
        TokenTree::Group(g) => plain_tokens(g.stream()),
        _ => true,
    })
}
