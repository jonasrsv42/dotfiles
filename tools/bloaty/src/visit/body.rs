//! What a fn body is: a call that only forwards its inputs, its hashes for
//! duplicate detection, a struct literal.

use super::BodyHash;
use super::syntax::compact;
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use syn::{Block, Expr, FnArg, Pat, Stmt};

/// If the block is just one call that passes the fn's own inputs along
/// (params, `self`, their fields, references and trivial conversions of
/// those), returns the callee. `Ok(..)`, `Some(..)`, `?`, `.await` and
/// `.into()` around it are looked through. Not forwarding: fns without
/// inputs (`Duration::from_secs(30)` binds a constant), constructors
/// (`Id(raw)`), and turbofish calls (`work_loop::<T>(..)` specialises).
pub(super) fn forward_target(sig: &syn::Signature, block: &Block) -> Option<String> {
    let params = inputs(sig);
    if params.is_empty() {
        return None;
    }
    let [Stmt::Expr(expr, _)] = block.stmts.as_slice() else {
        return None;
    };
    let rooted = |e: &Expr| is_rooted(e, &params);
    match peel(expr) {
        Expr::Call(c) => {
            let Expr::Path(p) = &*c.func else { return None };
            let last = p.path.segments.last()?;
            let constructor = last.ident.to_string().starts_with(char::is_uppercase);
            let turbofish = p.path.segments.iter().any(|s| !s.arguments.is_empty());
            (!constructor && !turbofish && !c.args.is_empty() && c.args.iter().all(rooted))
                .then(|| compact(&c.func))
        }
        Expr::MethodCall(m) => {
            (m.turbofish.is_none() && rooted(&m.receiver) && m.args.iter().all(rooted))
                .then(|| format!("{}.{}", compact(&m.receiver), m.method))
        }
        _ => None,
    }
}

/// Whether the block only hands the work to a field or method of `self`:
/// one expression that is a call whose first argument is rooted in `self`
/// (`Get::get(&self.input)`), a method call on `self` or a field of it
/// (`self.inner.close()`, `self.len()`), or a field itself (`&self.inner`),
/// with every argument an input of the fn. Looks through what `peel` does.
pub(super) fn forwards_to_self(sig: &syn::Signature, block: &Block) -> bool {
    if sig.receiver().is_none() {
        return false;
    }
    let params = inputs(sig);
    let [Stmt::Expr(expr, _)] = block.stmts.as_slice() else {
        return false;
    };
    let rooted = |e: &Expr| is_rooted(e, &params);
    let on_self = |e: &Expr| is_rooted(e, &["self".to_string()]);
    match peel(expr) {
        Expr::Call(c) => {
            matches!(&*c.func, Expr::Path(_))
                && c.args.first().is_some_and(on_self)
                && c.args.iter().all(rooted)
        }
        Expr::MethodCall(m) => on_self(&m.receiver) && m.args.iter().all(rooted),
        e @ Expr::Field(_) => on_self(e),
        _ => false,
    }
}

/// How much logic the body holds, as a duplicate would share it: operators
/// (`a + b`, `!x`, `-x`, assignments), branches and loops, closures, and
/// calls (not constructors like `Self(x)`) or macros given arguments. A body that only forwards (to `self`,
/// or its inputs to another fn) holds none, however long its signature;
/// neither do `{}`, `{ x }`, `Default::default()` or `Vec::new()`. Items
/// nested in the body are their own.
pub(super) fn logic(sig: &syn::Signature, block: &Block) -> u32 {
    use syn::visit::Visit;
    struct Count(u32);
    impl<'ast> Visit<'ast> for Count {
        fn visit_expr(&mut self, e: &'ast Expr) {
            let logic = match e {
                Expr::Binary(_)
                | Expr::Assign(_)
                | Expr::If(_)
                | Expr::Match(_)
                | Expr::While(_)
                | Expr::ForLoop(_)
                | Expr::Loop(_)
                | Expr::Closure(_) => true,
                Expr::Unary(u) => !matches!(u.op, syn::UnOp::Deref(_)),
                // Constructors (`Self(x)`, `Some(x)`) only wrap data.
                Expr::Call(c) => !c.args.is_empty() && !constructor(&c.func),
                Expr::MethodCall(m) => !m.args.is_empty(),
                _ => false,
            };
            self.0 += u32::from(logic);
            syn::visit::visit_expr(self, e);
        }
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            self.0 += u32::from(!m.tokens.is_empty());
        }
        fn visit_item(&mut self, _: &'ast syn::Item) {}
    }
    fn constructor(func: &Expr) -> bool {
        let Expr::Path(p) = func else { return false };
        p.path
            .segments
            .last()
            .is_some_and(|s| s.ident.to_string().starts_with(char::is_uppercase))
    }
    if forwards_to_self(sig, block) || forward_target(sig, block).is_some() {
        return 0;
    }
    let mut count = Count(0);
    count.visit_block(block);
    count.0
}

/// The names of the fn's inputs: `self` and plain identifier params.
fn inputs(sig: &syn::Signature) -> Vec<String> {
    sig.inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Receiver(_) => Some("self".to_string()),
            FnArg::Typed(t) => match &*t.pat {
                Pat::Ident(i) => Some(i.ident.to_string()),
                _ => None,
            },
        })
        .collect()
}

const CONVERSIONS: &[&str] = &[
    "into",
    "clone",
    "to_owned",
    "to_string",
    "as_ref",
    "as_mut",
    "unwrap",
    "borrow",
];
const WRAPPERS: &[&str] = &[
    "Ok", "Some", "Err", "Box::new", "Arc::new", "Rc::new", "Self",
];

fn is_conversion(m: &syn::ExprMethodCall) -> bool {
    m.args.is_empty() && CONVERSIONS.contains(&m.method.to_string().as_str())
}

fn peel(mut e: &Expr) -> &Expr {
    loop {
        e = match e {
            Expr::Paren(p) => &p.expr,
            Expr::Try(t) => &t.expr,
            Expr::Await(a) => &a.base,
            Expr::Reference(r) => &r.expr,
            Expr::MethodCall(m) if is_conversion(m) => &m.receiver,
            Expr::Call(c) if c.args.len() == 1 && WRAPPERS.contains(&compact(&c.func).as_str()) => {
                &c.args[0]
            }
            _ => return e,
        };
    }
}

/// An input of the fn: a param or `self`, or a field/reference/deref/trivial
/// conversion of one.
fn is_rooted(e: &Expr, params: &[String]) -> bool {
    match e {
        Expr::Path(p) => p
            .path
            .get_ident()
            .is_some_and(|i| params.iter().any(|p| i == p)),
        Expr::Field(f) => is_rooted(&f.base, params),
        Expr::Reference(r) => is_rooted(&r.expr, params),
        Expr::Paren(p) => is_rooted(&p.expr, params),
        Expr::Unary(u) => matches!(u.op, syn::UnOp::Deref(_)) && is_rooted(&u.expr, params),
        Expr::MethodCall(m) => is_conversion(m) && is_rooted(&m.receiver, params),
        _ => false,
    }
}

pub(super) fn body_hashes(block: &Block) -> BodyHash {
    token_hashes(block.to_token_stream())
}

/// `body_hashes` of any tokens.
pub(crate) fn token_hashes(tokens: TokenStream) -> BodyHash {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "else", "enum", "fn", "for", "if",
        "impl", "in", "let", "loop", "match", "move", "mut", "ref", "return", "self", "Self",
        "static", "struct", "unsafe", "where", "while", "dyn",
    ];
    struct H {
        exact: DefaultHasher,
        shape: DefaultHasher,
        names: DefaultHasher,
    }
    impl H {
        fn all(&mut self, v: impl Hash) {
            v.hash(&mut self.exact);
            v.hash(&mut self.shape);
            v.hash(&mut self.names);
        }
    }
    fn walk(ts: TokenStream, h: &mut H) {
        let tokens: Vec<TokenTree> = ts.into_iter().collect();
        for (i, tt) in tokens.iter().enumerate() {
            match tt {
                TokenTree::Group(g) => {
                    h.all(match g.delimiter() {
                        Delimiter::Parenthesis => 1u8,
                        Delimiter::Brace => 2,
                        Delimiter::Bracket => 3,
                        Delimiter::None => 4,
                    });
                    walk(g.stream(), h);
                    h.all(0u8);
                }
                TokenTree::Ident(ident) => {
                    let s = ident.to_string();
                    s.hash(&mut h.exact);
                    s.hash(&mut h.names);
                    // Keep what is called: `write!` vs `writeln!`, `push` vs
                    // `insert` are different code, not renamed variables.
                    let called = match tokens.get(i + 1) {
                        Some(TokenTree::Punct(p)) => p.as_char() == '!',
                        Some(TokenTree::Group(g)) => g.delimiter() == Delimiter::Parenthesis,
                        _ => false,
                    };
                    if called || KEYWORDS.contains(&s.as_str()) {
                        s.hash(&mut h.shape);
                    } else {
                        'i'.hash(&mut h.shape);
                    }
                }
                TokenTree::Punct(p) => h.all(p.as_char()),
                TokenTree::Literal(l) => {
                    l.to_string().hash(&mut h.exact);
                    'l'.hash(&mut h.shape);
                    'l'.hash(&mut h.names);
                }
            }
        }
    }
    let mut h = H {
        exact: DefaultHasher::new(),
        shape: DefaultHasher::new(),
        names: DefaultHasher::new(),
    };
    walk(tokens, &mut h);
    BodyHash {
        exact: h.exact.finish(),
        shape: h.shape.finish(),
        names: h.names.finish(),
    }
}

pub(super) fn is_struct_literal(block: &Block) -> bool {
    matches!(block.stmts.as_slice(), [Stmt::Expr(Expr::Struct(_), None)])
}

#[cfg(test)]
mod tests {
    use crate::testutil::{fact, facts};

    #[test]
    fn detects_forwarding() {
        let f = facts(
            r#"
            struct S { inner: Inner }
            impl S {
                fn get(&self, k: &str) -> Option<u32> { self.inner.get(k) }
                async fn load(&self, id: u64) -> Result<Thing> { Ok(self.inner.load(id).await?) }
                fn real(&self, k: &str) -> u32 { self.inner.get(k).map(|v| v + 1).unwrap_or(0) }
                fn field(&self) -> &Inner { &self.inner }
                fn magic(&self) -> u32 { helper(MAGIC) }
                pub fn new() -> Self { Self::default() }
                fn wrap(raw: u64) -> Self { Id(raw) }
                fn special(&self, x: u8) -> bool { self.inner.is::<X>(x) }
            }
            impl Display for S { fn fmt(&self, f: &mut Formatter) -> fmt::Result { self.inner.fmt(f) } }
            fn make(a: u32) -> Foo { Foo::new(a) }
            fn run(a: u32) { work_loop::<T>(a) }
            fn timeout() -> Duration { Duration::from_secs(30) }
            pub extern "C" fn ffi(x: u32) -> u32 { helper(x) }
            #[cfg(unix)]
            fn stub(x: u32) -> u32 { helper(x) }
            #[pymethods]
            impl S { fn exported(&self, k: &str) -> u32 { self.inner.get(k) } }
            "#,
        );
        let fwd = |n: &str| fact(&f, n).forwards_to.clone();
        assert_eq!(fwd("S::get").as_deref(), Some("self.inner.get"));
        assert_eq!(fwd("S::load").as_deref(), Some("self.inner.load"));
        assert_eq!(fwd("make").as_deref(), Some("Foo::new"));
        for n in [
            "S::real",
            "S::field",
            "S::magic",
            "S::new",
            "S::wrap",
            "S::special",
            "<S as Display>::fmt",
            "run",
            "timeout",
            "ffi",
            "stub",
            "S::exported",
        ] {
            assert_eq!(fwd(n), None, "{n}");
        }
        assert!(fact(&f, "S::exported").exempt);
    }

    /// `f(sig, block)` for each fn of `src` (free and in impls), by name.
    fn per_fn<T>(src: &str, f: impl Fn(&syn::Signature, &syn::Block) -> T) -> Vec<(String, T)> {
        let file = syn::parse_file(src).unwrap();
        let mut out = Vec::new();
        for item in &file.items {
            match item {
                syn::Item::Fn(i) => out.push((i.sig.ident.to_string(), f(&i.sig, &i.block))),
                syn::Item::Impl(i) => {
                    for it in &i.items {
                        if let syn::ImplItem::Fn(m) = it {
                            out.push((m.sig.ident.to_string(), f(&m.sig, &m.block)));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn detects_forwarding_to_self() {
        let src = r#"
            impl Get<dyn Sink> for S { fn get(&self) -> Result<Box<dyn Sink>, Error> { Get::get(&self.input) } }
            impl Close for S { fn close(&mut self) -> Result<()> { self.inner.close() } }
            impl S {
                fn len(&self) -> usize { self.x.len() }
                fn find(&self, k: &str) -> Option<u32> { Ok(self.inner.find(k)?) }
                fn me(&self) -> u32 { self.other() }
                fn field(&self) -> &Inner { &self.inner }
                fn owned(&self) -> Vec<u8> { self.x.clone() }
                fn no_logic(&self) -> usize { self.x.len() + 1 }
                fn constant(&self) -> u32 { helper(MAGIC) }
                fn no_arg_first(&self, k: &str) -> u32 { lookup(k, &self.inner) }
                fn no_literal_arg(&self) -> u32 { self.inner.get("k") }
                fn no_two(&self) -> usize { let n = self.x.len(); n }
                fn no_bare(&self) -> &Self { self }
                fn no_assoc(k: &str) -> u32 { Self::lookup(k) }
            }
            "#;
        let got = per_fn(src, super::forwards_to_self);
        assert_eq!(got.len(), 14);
        for (name, fwd) in got {
            let want = !(name.starts_with("no_") || name == "constant");
            assert_eq!(fwd, want, "{name}");
        }
    }

    #[test]
    fn counts_logic() {
        let src = r#"
            fn add(a: u32, b: u32) -> u32 { a + b }
            fn not(a: bool) -> bool { !a }
            fn timeout() -> Duration { Duration::from_secs(30) }
            fn fmt(x: u32) -> String { format!("{x}") }
            fn branchy(x: u32) -> u32 { if x > 1 { x * 2 } else { x } }
            fn empty() {}
            fn ident(x: u32) -> u32 { x }
            fn default() -> Config { Default::default() }
            fn new() -> Vec<u8> { Vec::new() }
            fn deref(x: &u32) -> u32 { *x }
            fn forward(x: u32) -> u32 { helper(x) }
            fn len(x: &[u8]) -> usize { x.len() }
            fn nested() { fn inner(a: u32) -> u32 { a + 1 } }
            fn wrap(x: u32) -> W { Self(x) }
            fn wrap_into(x: &str) -> W { W(x.into()) }
            fn some(a: u32, b: u32) -> Option<u32> { Some(a + b) }
            impl S { fn get(&self, k: &str) -> u32 { self.inner.get(k) } }
            "#;
        let got: Vec<(String, u32)> = per_fn(src, super::logic);
        let n = |name: &str| got.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(n("add"), 1);
        assert_eq!(n("not"), 1);
        assert_eq!(n("timeout"), 1);
        assert_eq!(n("fmt"), 1);
        assert_eq!(n("branchy"), 3);
        assert_eq!(n("some"), 1);
        for name in [
            "empty",
            "ident",
            "default",
            "new",
            "deref",
            "forward",
            "len",
            "nested",
            "get",
            "wrap",
            "wrap_into",
        ] {
            assert_eq!(n(name), 0, "{name}");
        }
    }

    #[test]
    fn body_hashes_separate_renames_from_literals_and_callees() {
        let f = facts(
            r#"
            fn a(x: u32) -> u32 {
                let y = x + 1;
                y * 2

            }
            fn b(q: u32) -> u32 {
                let z = q + 7;
                z * 2

            }
            fn c(x: u32) -> u32 {
                let y = x + 1;
                y * 2

            }
            fn d(x: u32) -> u32 {
                let y = x + 9;
                y * 2

            }
            fn e(x: u32) -> u32 {
                let y = other(x);
                y * 2

            }
            fn g(x: u32) -> u32 {
                let y = thing(x);
                y * 2

            }
            "#,
        );
        let h = |n: &str| fact(&f, n).body_hash.unwrap();
        assert_eq!(h("a"), h("c"));
        assert_ne!(h("a").exact, h("b").exact);
        assert_eq!(h("a").shape, h("b").shape);
        assert_ne!(h("a").names, h("b").names);
        // Only a literal differs: same names hash.
        assert_eq!(h("a").names, h("d").names);
        // Different callee: different shape.
        assert_ne!(h("e").shape, h("g").shape);
    }
}
