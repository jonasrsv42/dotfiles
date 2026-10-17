//! Walks a parsed file once and records the facts the reports need: which line
//! ranges are test code, module declarations, imports, every function (size,
//! body hashes, whether it just forwards, complexity and panic sites),
//! traits/types, impls and derives, and how often each name is mentioned.
//!
//! Submodules: `cfg` (what is test code), `body` (forwarding, body hashes),
//! `macros` (which macros are read, the rest opaque), `sig` (signatures as text),
//! `syntax` (paths and types as text). Per-fn measures live in fnmetrics.rs, shapes in shape.rs.

pub(crate) mod body;
mod cfg;
pub(crate) mod macros;
pub(crate) mod opening;
pub(crate) mod sig;
pub(crate) mod syntax;

use crate::fnmetrics::{FnMetrics, PanicSite, fn_metrics};
use crate::shape::{self, Lint, Shape};
use body::{forward_target, is_struct_literal, logic, test_body, token_hashes};
use cfg::{always_built, cfg_attrs, is_checked_test, is_test_fn, is_test_only};
use macros::{expression_args, token_words};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use sig::{Owner, Signature};
use std::collections::{HashMap, HashSet};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Expr, Item, Meta, Type};
use syntax::{compact, display_type, last_segment, path_segments, tidy_generics, words};

/// What the visitor needs to know about the file beyond its syntax.
pub struct Ctx<'a> {
    pub test_features: &'a HashSet<String>,
    /// A build script (build.rs or build/): code it generates is pulled in
    /// with `include!`, so names in its string literals count as uses.
    pub build_script: bool,
}

#[derive(Default)]
pub struct FileFacts {
    /// `#![cfg(test)]` at the top of the file.
    pub file_test: bool,
    /// ...and built by `cargo check --all-targets --all-features` (not
    /// `#![cfg(loom)]`).
    pub file_checked: bool,
    /// `include!` of a non-literal path, usually build-script output in
    /// OUT_DIR. Names in the build script's strings count as uses, so this
    /// only matters for crates without one.
    pub out_dir_include: bool,
    /// Code generated from non-Rust sources (`include_proto!`,
    /// `lalrpop_mod!`): it may use this crate's items invisibly.
    pub generated_code: bool,
    /// Paths of `include!("x")` with a literal path, relative to the file.
    pub literal_includes: Vec<String>,
    /// Mentions of a type's name inside its own definition and impls
    /// (`State::A => State::B`), which don't keep it alive; split like
    /// `refs`/`test_refs`.
    pub type_self_refs: HashMap<String, u32>,
    pub test_type_self_refs: HashMap<String, u32>,
    /// 1-based inclusive line ranges of test-only items.
    pub test_ranges: Vec<(usize, usize)>,
    /// `mod foo;` declarations: the module lives in another file.
    pub mod_decls: Vec<ModDecl>,
    /// Every module name declared here, inline or not (for path resolution).
    pub modules: Vec<String>,
    /// `use` imports as (name in scope, full path).
    pub imports: Vec<(String, Vec<String>)>,
    /// Names of every struct, enum, union and type alias defined here.
    pub type_names: Vec<String>,
    pub fns: Vec<FnFact>,
    pub items: Vec<ItemFact>,
    pub impls: Vec<ImplFact>,
    /// Words in opaque macro tokens (`macro_rules!` bodies, item macros,
    /// `quote!`, anything not in `macros::EXPRESSION_MACROS`): what they
    /// expand to may implement or declare anything named here.
    pub macro_words: HashSet<String>,
    /// An item-position macro bloaty can't read as items mentions `mod`
    /// (`cfg_if! { if #[cfg(unix)] { mod unix; } }`): it may declare
    /// modules bloaty doesn't follow.
    pub opaque_mods: bool,
    /// Name mentions outside definitions and plain imports, split by whether
    /// the mention sits in test code.
    pub refs: HashMap<String, u32>,
    pub test_refs: HashMap<String, u32>,
}

pub struct ModDecl {
    /// Names of the inline `mod x { .. }` blocks the declaration sits in.
    pub inline_path: Vec<String>,
    pub name: String,
    /// `#[path = ".."]`.
    pub path_attr: Option<String>,
    /// Declared from test-only code...
    pub test: bool,
    /// ...that `cargo check --all-targets --all-features` builds (not
    /// `#[cfg(loom)] mod x;`).
    pub checked: bool,
    /// Non-test cfgs on it and the items around it (`cfg (unix)`).
    pub cfgs: Vec<String>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Vis {
    Private,
    /// `pub(crate)`, `pub(super)`, `pub(in ..)`.
    Restricted,
    Public,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FnKind {
    Free,
    Inherent,
    TraitImpl,
    TraitDefault,
}

/// Body hashes for duplicate detection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BodyHash {
    /// Every token.
    pub exact: u64,
    /// Keywords, punctuation and callee/macro names; other names and
    /// literals erased. Copy-pasted code with renamed variables matches.
    pub shape: u64,
    /// Names kept, literals erased. Bodies that only differ here are data
    /// tables (match arms mapping to different strings), not duplicates.
    pub names: u64,
}

pub struct FnFact {
    /// Display name: `f`, `Type::f` or `<Type as Trait>::f`.
    pub name: String,
    /// Bare identifier, for reference counts.
    pub ident: String,
    /// First line of its doc comments and attributes.
    pub start_line: usize,
    /// The `fn` line.
    pub line: usize,
    pub end_line: usize,
    /// Non-test mentions of its own name inside its body. Recursion doesn't
    /// keep a fn alive, so findings subtract these from its uses. (A
    /// mention meaning another fn, like `self.v.len()` inside `fn len`, is
    /// among them but also among the uses, so it cancels out for this fn
    /// and still counts for the other.)
    pub self_refs: u32,
    /// Line ranges of items nested in the body (fns, impls, ...).
    pub nested: Vec<(usize, usize)>,
    pub in_test: bool,
    pub vis: Vis,
    pub kind: FnKind,
    /// An entry point or otherwise externally referenced fn: extern ABI,
    /// `#[no_mangle]`, `main`, proc-macro or framework attributes (on the fn
    /// or its impl block, like `#[pymethods]`), or defined inside an item
    /// macro that may export it.
    pub exempt: bool,
    /// The non-test `#[cfg(..)]` predicates on the fn and its enclosing
    /// items (`cfg (unix)`), outermost first.
    pub cfgs: Vec<String>,
    /// Set when the body is a single call that only passes its inputs along.
    pub forwards_to: Option<body::Forward>,
    /// Its signature, name left out (duplicates compare it).
    pub signature: Signature,
    /// The line of the body's opening brace: lines before it (from `line`)
    /// are signature.
    pub body_line: usize,
    /// Logic in the body (operators, branches, calls with arguments...;
    /// none if it only forwards to `self` or its inputs): see `body::logic`.
    pub logic: u32,
    /// Unless nested deeply or a lone struct literal.
    pub body_hash: Option<BodyHash>,
    /// Complexity and fragility measures; `None` for test code.
    pub metrics: Option<FnMetrics>,
    /// Where `metrics.panics` are, in source order (empty for test code).
    pub panic_sites: Vec<PanicSite>,
    /// A table or dispatch (measured fns only).
    pub shape: Option<Shape>,
    /// Clippy lints allowed on the fn that excuse a smell dimension
    /// (measured fns only).
    pub allowed: Vec<Lint>,
    /// A test fn as written (`TestFn`).
    pub test_fn: Option<TestFn>,
}

/// A `#[test]`-like fn without params (`cfg::is_test_fn`): what tells tests
/// apart that could be one table-driven test (`findings::parametrize`).
pub struct TestFn {
    /// The inline `mod`s it sits in (its scope within the file), each with
    /// its cfgs (`imp #[cfg (unix)]`): cfg-exclusive mods of one name are
    /// different scopes.
    pub scope: Vec<String>,
    /// Its attributes other than docs, as text (`should_panic`,
    /// `tokio::test (flavor = "multi_thread")`).
    pub attrs: Vec<String>,
    /// The body's literals, in order (nested items included).
    pub literals: Vec<body::Literal>,
    /// The body's lines after its opening brace's (`body::TestBody`).
    pub lines: usize,
    /// The statements opening the body a setup fn could take
    /// (`opening::test_opening`).
    pub opening: Vec<opening::TestStmt>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemKind {
    Trait,
    Struct,
    Enum,
    Union,
}

pub struct ItemFact {
    pub kind: ItemKind,
    pub name: String,
    pub vis: Vis,
    /// First line of its doc comments and attributes.
    pub start_line: usize,
    /// The line with its name.
    pub line: usize,
    pub end_line: usize,
    pub in_test: bool,
    /// Defined inside an item macro, which may use or export it.
    pub exempt: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImplKind {
    /// `impl Type { .. }`.
    Inherent,
    Concrete,
    /// `impl<T: Bound> Trait for T`: an extension trait, not a one-off.
    Blanket,
    /// `impl<T: Trait> Trait for Box<T>` / `&T` / `Box<dyn Trait>`: plumbing.
    Forwarding,
    /// `#[derive(Trait)]`.
    Derive,
}

pub struct ImplFact {
    /// Empty for inherent impls.
    pub trait_path: Vec<String>,
    /// Display form, generics included.
    pub self_ty: String,
    /// Bare name of the self type (`Node` for `Node<'a, T>`).
    pub self_name: String,
    /// First line of its attributes.
    pub start_line: usize,
    /// The `impl` line.
    pub line: usize,
    pub end_line: usize,
    pub kind: ImplKind,
    /// In test code (`#[cfg(test)] impl X`, or inside a test module)
    /// that the recommended check builds: a `#[cfg(loom)] impl X` goes
    /// with X when X is deleted.
    pub checked_test: bool,
}

pub fn collect(file: &syn::File, ctx: &Ctx) -> FileFacts {
    let file_test = is_test_only(&file.attrs, ctx.test_features);
    let mut v = Visitor {
        ctx,
        test_depth: usize::from(file_test),
        ref_test_depth: usize::from(is_checked_test(&file.attrs, ctx.test_features)),
        unchecked_depth: usize::from(file_test && !is_checked_test(&file.attrs, ctx.test_features)),
        doc_fence: None,
        inline_path: Vec::new(),
        scope: Vec::new(),
        impl_ctx: Vec::new(),
        fn_stack: Vec::new(),
        fn_idents: HashMap::new(),
        macro_depth: 0,
        cfg_stack: Vec::new(),
        self_types: Vec::new(),
        defs: HashSet::new(),
        facts: FileFacts {
            file_test,
            file_checked: is_checked_test(&file.attrs, ctx.test_features),
            ..Default::default()
        },
    };
    v.visit_file(file);
    v.facts
}

struct ImplCtx {
    self_ty: String,
    /// The impl's (or trait's) generics and where clause, as `sig` writes them.
    generics: String,
    trait_name: Option<String>,
    /// The impl block carries an attribute macro (`#[pymethods]`, ...) that
    /// exports its fns.
    exempt: bool,
}

struct Visitor<'a> {
    ctx: &'a Ctx<'a>,
    /// Inside test-only code: its lines count as test.
    test_depth: usize,
    /// Inside test code that `cargo check --all-targets --all-features`
    /// builds: names mentioned here are test references. Code behind
    /// `cfg(loom)`/`cfg(fuzzing)` is test for line counts but its mentions
    /// count as ordinary uses, since that check never compiles it.
    ref_test_depth: usize,
    /// Inside code `cargo check --all-targets --all-features` doesn't build
    /// on this host: `cfg(loom)`, `cfg(windows)`, `cfg(not(feature = ..))`.
    /// Tests in here are unchecked, so mentions count as ordinary uses.
    unchecked_depth: usize,
    /// Inside a ```code``` block of doc comments (a doctest).
    doc_fence: Option<DocFence>,
    inline_path: Vec<String>,
    /// `inline_path` with each mod's cfgs (`TestFn::scope`).
    scope: Vec<String>,
    impl_ctx: Vec<ImplCtx>,
    /// Indices into `facts.fns` of the fns we're inside.
    fn_stack: Vec<usize>,
    /// Idents of the fns on `fn_stack`, with multiplicity.
    fn_idents: HashMap<String, u32>,
    /// Inside items parsed out of an item macro (`export! { fn f() {} }`).
    macro_depth: usize,
    /// Non-test `#[cfg(..)]`s of the enclosing items.
    cfg_stack: Vec<String>,
    /// Names of the types whose definition or impls we're inside.
    self_types: Vec<String>,
    /// Positions of identifiers that define something; not counted as refs.
    defs: HashSet<(usize, usize)>,
    facts: FileFacts,
}

/// An open code block in doc comments.
struct DocFence {
    /// ```` ``` ```` or `~~~`: what closes it.
    marker: &'static str,
    /// A doctest that compiles: not `text`, `ignore`, `compile_fail`...
    rust: bool,
}

/// Whether a code fence's info string (` ```rust,no_run `) marks a doctest
/// that compiles and so may use items.
fn is_rust_fence(info: &str) -> bool {
    info.split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .all(|t| {
            matches!(t, "rust" | "no_run" | "should_panic" | "test_harness")
                || t.starts_with("edition")
        })
}

/// A fn as found in the syntax tree, before it becomes a `FnFact`.
struct FnSite<'s> {
    name: String,
    attrs: &'s [Attribute],
    sig: &'s syn::Signature,
    block: &'s Block,
    vis: Vis,
    kind: FnKind,
    test_only: bool,
    /// The impl's Self type and trait (for default methods: the trait).
    owner: Owner,
}

impl Visitor<'_> {
    fn in_test(&self) -> bool {
        self.test_depth > 0
    }

    fn define(&mut self, ident: &syn::Ident) {
        let p = ident.span().start();
        self.defs.insert((p.line, p.column));
    }

    fn count_ref(&mut self, name: &str) {
        let test = self.ref_test_depth > 0;
        let innermost = self
            .fn_stack
            .last()
            .map(|&i| self.facts.fns[i].ident.as_str());
        let any_fn = !test
            && (innermost == Some(name)
                || self.fn_stack.len() > 1 && self.fn_idents.contains_key(name));
        for &i in self.fn_stack.iter().filter(|_| any_fn) {
            if self.facts.fns[i].ident == name {
                self.facts.fns[i].self_refs += 1;
            }
        }
        if self.self_types.iter().any(|t| t == name) {
            let map = if test {
                &mut self.facts.test_type_self_refs
            } else {
                &mut self.facts.type_self_refs
            };
            *map.entry(name.to_string()).or_default() += 1;
        }
        let map = if test {
            &mut self.facts.test_refs
        } else {
            &mut self.facts.refs
        };
        match map.get_mut(name) {
            Some(n) => *n += 1,
            None => {
                map.insert(name.to_string(), 1);
            }
        }
    }

    /// Counts every word that could be a name inside a string literal token.
    fn count_string_words(&mut self, literal: &str) {
        let Some(inner) = literal.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return;
        };
        for word in words(inner) {
            self.count_ref(word);
        }
    }

    /// Doctests (```` ``` ```` blocks in doc comments) use names too: as
    /// tests, since `cargo test --doc` runs them.
    fn doc_line(&mut self, text: &str) {
        for line in text.lines() {
            let t = line.trim_start();
            let marker = ["```", "~~~"].into_iter().find(|m| t.starts_with(m));
            match (&self.doc_fence, marker) {
                (Some(f), Some(m)) if f.marker == m => self.doc_fence = None,
                (None, Some(m)) => {
                    self.doc_fence = Some(DocFence {
                        marker: m,
                        rust: is_rust_fence(&t[m.len()..]),
                    })
                }
                (Some(DocFence { rust: true, .. }), _) => {
                    self.ref_test_depth += 1;
                    for word in words(line) {
                        self.count_ref(word);
                    }
                    self.ref_test_depth -= 1;
                }
                _ => {}
            }
        }
    }

    /// Names in opaque tokens (macro arguments, attribute arguments).
    /// `strings`: also count words in string literals, as for
    /// `#[serde(default = "path::f")]` or build-script output.
    fn count_token_refs(&mut self, tokens: TokenStream, strings: bool) {
        let mut prev: [Option<String>; 2] = [None, None];
        for tt in tokens {
            match &tt {
                TokenTree::Ident(i) => self.count_ref(&i.to_string()),
                TokenTree::Group(g) => self.count_token_refs(g.stream(), strings),
                TokenTree::Literal(l) if strings => {
                    // serde's `with = "m"` uses `m::serialize`/`deserialize`.
                    if prev[0].as_deref() == Some("with") && prev[1].as_deref() == Some("=") {
                        self.count_ref("serialize");
                        self.count_ref("deserialize");
                    }
                    self.count_string_words(&l.to_string());
                }
                _ => {}
            }
            prev = [
                prev[1].take(),
                match &tt {
                    TokenTree::Ident(i) => Some(i.to_string()),
                    TokenTree::Punct(p) => Some(p.as_char().to_string()),
                    _ => None,
                },
            ];
        }
    }

    /// Records a fn and visits its body with it on the fn stack. `checked`:
    /// a test-only fn that cargo check builds (see `ref_test_depth`).
    fn visit_fn(&mut self, f: FnFact, checked: bool, visit_body: impl FnOnce(&mut Self)) {
        let test_only = f.in_test && !self.in_test();
        let checked = checked && self.ref_test_depth == 0 && self.unchecked_depth == 0;
        *self.fn_idents.entry(f.ident.clone()).or_default() += 1;
        self.facts.fns.push(f);
        self.fn_stack.push(self.facts.fns.len() - 1);
        self.test_depth += usize::from(test_only);
        self.ref_test_depth += usize::from(checked);
        visit_body(self);
        self.ref_test_depth -= usize::from(checked);
        self.test_depth -= usize::from(test_only);
        if let Some(i) = self.fn_stack.pop() {
            let ident = &self.facts.fns[i].ident;
            if let Some(n) = self.fn_idents.get_mut(ident) {
                *n -= 1;
                if *n == 0 {
                    self.fn_idents.remove(ident.as_str());
                }
            }
        }
    }

    fn fn_fact(&self, s: FnSite) -> FnFact {
        let line = s.sig.fn_token.span.start().line;
        let end_line = s.block.brace_token.span.close().end().line;
        let in_test = self.in_test() || s.test_only;
        let in_exempt_impl = self.impl_ctx.last().is_some_and(|c| c.exempt);
        let exempt = in_exempt_impl
            || self.macro_depth > 0
            || s.sig.abi.is_some()
            || s.sig.ident == "main"
            || s.attrs.iter().any(|a| !is_plain_attr(a));
        let mut cfgs = self.cfg_stack.clone();
        cfgs.extend(cfg_attrs(s.attrs));
        // Allowed lints for test fns too: long_fn honours them.
        let allowed = shape::allowed(s.attrs);
        let (metrics, panic_sites, shape) = if in_test {
            (None, Vec::new(), None)
        } else {
            let (m, sites) = fn_metrics(s.sig, s.block);
            (Some(m), sites, shape::shape(s.block))
        };
        let test = s.sig.inputs.is_empty() && is_test_fn(s.attrs);
        // A lone `Self { a, b, c }` matches every constructor of its shape.
        let hashed = self.fn_stack.len() < 3 && !is_struct_literal(s.block);
        // Tokenized once, for the body hash and a test's literals.
        let tokens = (hashed || test).then(|| s.block.to_token_stream());
        let body_line = s.block.brace_token.span.open().start().line;
        let test_fn = test.then(|| {
            let b = test_body(tokens.clone().unwrap_or_default(), body_line);
            TestFn {
                scope: self.scope.clone(),
                attrs: s
                    .attrs
                    .iter()
                    .filter(|a| !a.path().is_ident("doc"))
                    .map(|a| compact(&a.meta))
                    .collect(),
                literals: b.literals,
                lines: b.lines,
                opening: opening::test_opening(s.block),
            }
        });
        let forwards_to = match s.kind {
            // Trait impls often *must* delegate (Display, From, Deref...).
            FnKind::Free | FnKind::Inherent if !in_test && !exempt && cfgs.is_empty() => {
                forward_target(s.sig, s.block)
            }
            _ => None,
        };
        FnFact {
            name: s.name,
            ident: s.sig.ident.to_string(),
            start_line: attrs_start(s.attrs, || line),
            line,
            end_line,
            self_refs: 0,
            nested: Vec::new(),
            in_test,
            vis: s.vis,
            kind: s.kind,
            exempt,
            cfgs,
            signature: Signature::new(s.sig, s.block, s.owner),
            body_line,
            logic: logic(s.sig, s.block),
            forwards_to,
            body_hash: hashed.then(|| token_hashes(tokens.unwrap_or_default())),
            metrics,
            panic_sites,
            shape,
            allowed,
            test_fn,
        }
    }

    fn push_item(
        &mut self,
        kind: ItemKind,
        ident: &syn::Ident,
        vis: &syn::Visibility,
        attrs: &[Attribute],
        end: usize,
    ) {
        self.define(ident);
        let line = ident.span().start().line;
        self.facts.items.push(ItemFact {
            kind,
            name: ident.to_string(),
            vis: vis_of(vis),
            start_line: attrs_start(attrs, || line),
            line,
            end_line: end,
            in_test: self.in_test(),
            exempt: self.macro_depth > 0,
        });
    }

    fn type_item(
        &mut self,
        kind: ItemKind,
        ident: &syn::Ident,
        vis: &syn::Visibility,
        attrs: &[Attribute],
        end: proc_macro2::Span,
    ) {
        self.facts.type_names.push(ident.to_string());
        self.record_derives(attrs, ident);
        self.push_item(kind, ident, vis, attrs, end.end().line);
    }

    fn record_derives(&mut self, attrs: &[Attribute], ident: &syn::Ident) {
        for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
            let line = attr.pound_token.span.start().line;
            let _ = attr.parse_nested_meta(|meta| {
                self.facts.impls.push(ImplFact {
                    trait_path: path_segments(&meta.path),
                    self_ty: ident.to_string(),
                    self_name: ident.to_string(),
                    start_line: line,
                    line,
                    end_line: line,
                    kind: ImplKind::Derive,
                    checked_test: self.ref_test_depth > 0,
                });
                Ok(())
            });
        }
    }

    /// Imports aren't uses, but some names in them are: the original of a
    /// `use a::X as Y` (only reachable as `Y` afterwards), modules and enums
    /// a path goes through (`use self::Kind::*`), and `pub use` re-exports.
    fn visit_use_tree(&mut self, tree: &syn::UseTree, public: bool) {
        match tree {
            syn::UseTree::Path(p) => {
                self.count_ref(&p.ident.to_string());
                self.visit_use_tree(&p.tree, public);
            }
            syn::UseTree::Name(n) if public && n.ident != "self" => {
                self.count_ref(&n.ident.to_string());
            }
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
            syn::UseTree::Rename(r) => self.count_ref(&r.ident.to_string()),
            syn::UseTree::Group(g) => g.items.iter().for_each(|t| self.visit_use_tree(t, public)),
        }
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if let Some(&top) = self.fn_stack.last() {
            let r = item_range(item);
            self.facts.fns[top].nested.push(r);
        }
        self.doc_fence = None;
        let attrs = item_attrs(item);
        // Not built by the recommended check on this host: references in
        // here are ordinary uses, even from tests.
        let unchecked = !always_built(attrs);
        if is_test_only(attrs, self.ctx.test_features) {
            let checked = !unchecked && self.unchecked_depth == 0;
            self.facts.test_ranges.push(item_range(item));
            let saved = self.ref_test_depth;
            self.test_depth += 1;
            if checked {
                self.ref_test_depth += 1;
            } else {
                self.ref_test_depth = 0;
                self.unchecked_depth += 1;
            }
            visit::visit_item(self, item);
            self.ref_test_depth = saved;
            self.unchecked_depth -= usize::from(!checked);
            self.test_depth -= 1;
            return;
        }
        let saved = self.ref_test_depth;
        if unchecked {
            self.ref_test_depth = 0;
            self.unchecked_depth += 1;
        }
        // Fns get their own cfgs in `fn_fact`; other items pass theirs down
        // (`#[cfg(unix)] mod sys { fn f() {} }`).
        let cfgs = match item {
            Item::Fn(_) => Vec::new(),
            _ => cfg_attrs(attrs),
        };
        let pushed = cfgs.len();
        self.cfg_stack.extend(cfgs);
        visit::visit_item(self, item);
        self.cfg_stack.truncate(self.cfg_stack.len() - pushed);
        self.unchecked_depth -= usize::from(unchecked);
        self.ref_test_depth = saved;
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        // `mod r#async;` lives in `async.rs`.
        let name = m.ident.unraw().to_string();
        self.facts.modules.push(name.clone());
        // `#[cfg_attr(unix, path = "sys/unix.rs")]`: a file bloaty can't
        // pick, so the crate has code it may not see.
        let cfg_path = m.attrs.iter().any(|a| {
            a.path().is_ident("cfg_attr")
                && matches!(&a.meta, Meta::List(l) if words(&l.tokens.to_string()).any(|w| w == "path"))
        });
        self.facts.generated_code |= cfg_path;
        if m.content.is_none() {
            let path_attr = m.attrs.iter().find_map(|a| match &a.meta {
                Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
                    Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) => Some(s.value()),
                    _ => None,
                },
                _ => None,
            });
            self.facts.mod_decls.push(ModDecl {
                inline_path: self.inline_path.clone(),
                name,
                path_attr,
                test: self.in_test(),
                checked: self.ref_test_depth > 0,
                cfgs: self.cfg_stack.clone(),
            });
            return;
        }
        let mut cfgs = cfg_attrs(&m.attrs);
        cfgs.retain(|c| c != "cfg (test)");
        self.scope.push(if cfgs.is_empty() {
            name.clone()
        } else {
            format!("{name} #[{}]", cfgs.join(", "))
        });
        self.inline_path.push(name);
        visit::visit_item_mod(self, m);
        self.inline_path.pop();
        self.scope.pop();
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        collect_use(&u.tree, &mut Vec::new(), &mut self.facts.imports);
        self.visit_use_tree(&u.tree, matches!(u.vis, syn::Visibility::Public(_)));
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.define(&f.sig.ident);
        let fact = self.fn_fact(FnSite {
            name: f.sig.ident.to_string(),
            attrs: &f.attrs,
            sig: &f.sig,
            block: &f.block,
            vis: vis_of(&f.vis),
            kind: FnKind::Free,
            test_only: false,
            owner: Owner::default(),
        });
        self.visit_fn(fact, false, |v| visit::visit_item_fn(v, f));
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let self_ty = display_type(&i.self_ty);
        let self_name = match &*i.self_ty {
            Type::Path(p) => p.path.segments.last().map(|seg| {
                self.define(&seg.ident);
                seg.ident.to_string()
            }),
            _ => None,
        }
        .unwrap_or_else(|| self_ty.clone());
        let (trait_path, kind) = match &i.trait_ {
            Some((_, path, _)) => (path_segments(path), impl_kind(&i.self_ty, &i.generics)),
            None => (Vec::new(), ImplKind::Inherent),
        };
        self.facts.impls.push(ImplFact {
            trait_path,
            self_ty: self_ty.clone(),
            self_name,
            start_line: attrs_start(&i.attrs, || i.impl_token.span.start().line),
            line: i.impl_token.span.start().line,
            end_line: i.brace_token.span.close().end().line,
            kind,
            checked_test: self.ref_test_depth > 0,
        });
        self.impl_ctx.push(ImplCtx {
            self_ty,
            generics: sig::generics_text(&i.generics),
            trait_name: i
                .trait_
                .as_ref()
                .map(|(_, path, _)| tidy_generics(compact(path))),
            exempt: i.attrs.iter().any(|a| !is_plain_attr(a)),
        });
        let self_name = self.facts.impls.last().map(|i| i.self_name.clone());
        self.self_types.extend(self_name);
        visit::visit_item_impl(self, i);
        self.self_types.pop();
        self.impl_ctx.pop();
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.define(&f.sig.ident);
        let test_only = is_test_only(&f.attrs, self.ctx.test_features);
        if test_only {
            let start = attrs_start(&f.attrs, || f.sig.fn_token.span.start().line);
            let end = f.block.brace_token.span.close().end().line;
            self.facts.test_ranges.push((start, end));
        }
        let (name, kind, vis) = match self.impl_ctx.last() {
            Some(ImplCtx {
                self_ty,
                trait_name: Some(t),
                ..
            }) => (
                format!("<{self_ty} as {t}>::{}", f.sig.ident),
                FnKind::TraitImpl,
                Vis::Public,
            ),
            Some(ImplCtx { self_ty, .. }) => (
                format!("{self_ty}::{}", f.sig.ident),
                FnKind::Inherent,
                vis_of(&f.vis),
            ),
            None => (f.sig.ident.to_string(), FnKind::Inherent, vis_of(&f.vis)),
        };
        let owner = self.impl_ctx.last().map_or_else(Owner::default, |c| Owner {
            self_ty: Some(c.self_ty.clone()),
            generics: c.generics.clone(),
            trait_name: c.trait_name.clone(),
        });
        let fact = self.fn_fact(FnSite {
            name,
            attrs: &f.attrs,
            sig: &f.sig,
            block: &f.block,
            vis,
            kind,
            test_only,
            owner,
        });
        let checked = is_checked_test(&f.attrs, self.ctx.test_features);
        self.visit_fn(fact, checked, |v| visit::visit_impl_item_fn(v, f));
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        let end = t.brace_token.span.close().end().line;
        self.push_item(ItemKind::Trait, &t.ident, &t.vis, &t.attrs, end);
        self.impl_ctx.push(ImplCtx {
            self_ty: t.ident.to_string(),
            generics: sig::generics_text(&t.generics),
            trait_name: None,
            exempt: false,
        });
        self.self_types.push(t.ident.to_string());
        visit::visit_item_trait(self, t);
        self.self_types.pop();
        self.impl_ctx.pop();
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.define(&f.sig.ident);
        let Some(block) = &f.default else {
            return visit::visit_trait_item_fn(self, f);
        };
        let test_only = is_test_only(&f.attrs, self.ctx.test_features);
        if test_only {
            let start = attrs_start(&f.attrs, || f.sig.fn_token.span.start().line);
            let end = block.brace_token.span.close().end().line;
            self.facts.test_ranges.push((start, end));
        }
        let (owner, generics) = self
            .impl_ctx
            .last()
            .map_or(("?", ""), |c| (c.self_ty.as_str(), c.generics.as_str()));
        let fact = self.fn_fact(FnSite {
            name: format!("{owner}::{}", f.sig.ident),
            attrs: &f.attrs,
            sig: &f.sig,
            block,
            vis: Vis::Public,
            kind: FnKind::TraitDefault,
            test_only,
            owner: Owner {
                self_ty: None,
                generics: generics.to_string(),
                trait_name: Some(owner.to_string()),
            },
        });
        let checked = is_checked_test(&f.attrs, self.ctx.test_features);
        self.visit_fn(fact, checked, |v| visit::visit_trait_item_fn(v, f));
    }

    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        let end = match &s.fields {
            syn::Fields::Named(f) => f.brace_token.span.close(),
            syn::Fields::Unnamed(f) => f.paren_token.span.close(),
            syn::Fields::Unit => s.ident.span(),
        };
        self.type_item(ItemKind::Struct, &s.ident, &s.vis, &s.attrs, end);
        self.self_types.push(s.ident.to_string());
        visit::visit_item_struct(self, s);
        self.self_types.pop();
    }

    fn visit_item_enum(&mut self, e: &'ast syn::ItemEnum) {
        let end = e.brace_token.span.close();
        self.type_item(ItemKind::Enum, &e.ident, &e.vis, &e.attrs, end);
        self.self_types.push(e.ident.to_string());
        visit::visit_item_enum(self, e);
        self.self_types.pop();
    }

    fn visit_item_union(&mut self, u: &'ast syn::ItemUnion) {
        let end = u.fields.brace_token.span.close();
        self.type_item(ItemKind::Union, &u.ident, &u.vis, &u.attrs, end);
        self.self_types.push(u.ident.to_string());
        visit::visit_item_union(self, u);
        self.self_types.pop();
    }

    fn visit_item_type(&mut self, t: &'ast syn::ItemType) {
        self.define(&t.ident);
        self.facts.type_names.push(t.ident.to_string());
        visit::visit_item_type(self, t);
    }

    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        self.define(&c.ident);
        visit::visit_item_const(self, c);
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        self.define(&s.ident);
        visit::visit_item_static(self, s);
    }

    fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
        token_words(m.mac.tokens.clone(), &mut self.facts.macro_words);
        if m.mac.path.is_ident("macro_rules") {
            // A template with `mod` may declare modules where it's invoked.
            let mut words = HashSet::new();
            token_words(m.mac.tokens.clone(), &mut words);
            self.facts.opaque_mods |= words.contains("mod");
            return visit::visit_item_macro(self, m);
        }
        // Item-position macros like tokio's `cfg_rt! { pub mod runtime; }`
        // usually wrap plain items: when the whole body parses as Rust
        // items, visit them as written (no guess at the expansion), so
        // their modules and the names they use are seen. What they define
        // is exempt from `unused`: the macro may export it (`#[no_mangle]`)
        // or turn it into a test.
        // Each level re-parses everything inside it: stop after a few.
        let parsed = (self.macro_depth < 4)
            .then(|| syn::parse2::<syn::File>(m.mac.tokens.clone()).ok())
            .flatten();
        match parsed {
            Some(file) if !file.items.is_empty() => {
                for a in &m.attrs {
                    self.visit_attribute(a);
                }
                self.visit_path(&m.mac.path);
                self.macro_depth += 1;
                for item in &file.items {
                    self.visit_item(item);
                }
                self.macro_depth -= 1;
            }
            // Opaque (`cfg_if! { if #[cfg(unix)] { mod unix; } .. }` isn't
            // Rust): its words count as mentions, and a `mod` in it means
            // modules bloaty may not see.
            _ => {
                let mut words = HashSet::new();
                token_words(m.mac.tokens.clone(), &mut words);
                self.facts.opaque_mods |= words.contains("mod");
                visit::visit_item_macro(self, m)
            }
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        // Code pulled in from elsewhere: `include!(concat!(env!("OUT_DIR"),
        // ..))`, `tonic::include_proto!`, `lalrpop_mod!`.
        match last_segment(&m.path).as_str() {
            "include" => match syn::parse2::<syn::LitStr>(m.tokens.clone()) {
                Ok(path) => self.facts.literal_includes.push(path.value()),
                Err(_) => self.facts.out_dir_include = true,
            },
            "include_str" | "include_bytes" => {}
            n => self.facts.generated_code |= n.contains("include") || n == "lalrpop_mod",
        }
        // Macro arguments are opaque tokens to syn; names used in
        // `assert_eq!(f(x), 1)` or `vec![T::new()]` still count as uses.
        self.count_token_refs(m.tokens.clone(), self.ctx.build_script);
        if expression_args(m).is_none() {
            token_words(m.tokens.clone(), &mut self.facts.macro_words);
        }
        visit::visit_macro(self, m);
    }

    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
        if self.ctx.build_script {
            self.count_string_words(&format!("{:?}", s.value()));
        }
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        match &attr.meta {
            Meta::NameValue(nv) if attr.path().is_ident("doc") => {
                if let Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    self.doc_line(&s.value());
                }
            }
            _ if attr.path().is_ident("doc") => {}
            Meta::List(l) => self.count_token_refs(l.tokens.clone(), true),
            Meta::NameValue(nv) => self.count_token_refs(nv.value.to_token_stream(), true),
            Meta::Path(_) => {}
        }
    }

    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        let p = i.span().start();
        if !self.defs.contains(&(p.line, p.column)) {
            self.count_ref(&i.to_string());
        }
    }
}

fn vis_of(v: &syn::Visibility) -> Vis {
    match v {
        syn::Visibility::Public(_) => Vis::Public,
        syn::Visibility::Restricted(_) => Vis::Restricted,
        syn::Visibility::Inherited => Vis::Private,
    }
}

/// Attributes that don't make a fn an entry point or externally referenced.
fn is_plain_attr(a: &Attribute) -> bool {
    const PLAIN: &[&str] = &[
        "doc",
        "inline",
        "must_use",
        "allow",
        "expect",
        "warn",
        "deny",
        "forbid",
        "cfg",
        "cfg_attr",
        "track_caller",
        "cold",
        "deprecated",
        "rustfmt",
        "clippy",
        "instrument",
        "async_trait",
    ];
    let first = a.path().segments.first().map(|s| s.ident.to_string());
    let last = last_segment(a.path());
    first.is_some_and(|f| PLAIN.contains(&f.as_str())) || PLAIN.contains(&last.as_str())
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn attrs_start(attrs: &[Attribute], fallback: impl FnOnce() -> usize) -> usize {
    attrs
        .first()
        .map_or_else(fallback, |a| a.pound_token.span.start().line)
}

/// 1-based inclusive line range of an item, attributes included. Reads
/// keyword and brace spans where there are some: `Spanned::span()`
/// re-tokenizes the whole item.
fn item_range(item: &Item) -> (usize, usize) {
    let close = |d: &proc_macro2::extra::DelimSpan| d.close().end().line;
    let end = match item {
        Item::Fn(f) => close(&f.block.brace_token.span),
        Item::Impl(i) => close(&i.brace_token.span),
        Item::Trait(t) => close(&t.brace_token.span),
        Item::Mod(syn::ItemMod {
            content: Some((brace, _)),
            ..
        }) => close(&brace.span),
        _ => item.span().end().line,
    };
    let start = attrs_start(item_attrs(item), || match item {
        Item::Fn(f) => f.sig.fn_token.span.start().line,
        Item::Impl(i) => i.impl_token.span.start().line,
        Item::Mod(m) => m.mod_token.span.start().line,
        _ => item.span().start().line,
    });
    (start, end)
}

fn collect_use(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    out: &mut Vec<(String, Vec<String>)>,
) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            collect_use(&p.tree, prefix, out);
            prefix.pop();
        }
        syn::UseTree::Name(n) if n.ident == "self" => {
            if let Some(last) = prefix.last() {
                out.push((last.clone(), prefix.clone()));
            }
        }
        syn::UseTree::Name(n) => {
            let mut path = prefix.clone();
            path.push(n.ident.to_string());
            out.push((n.ident.to_string(), path));
        }
        syn::UseTree::Rename(r) => {
            let mut path = prefix.clone();
            path.push(r.ident.to_string());
            out.push((r.rename.to_string(), path));
        }
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| collect_use(t, prefix, out)),
        syn::UseTree::Glob(_) => {}
    }
}

fn impl_kind(self_ty: &Type, generics: &syn::Generics) -> ImplKind {
    let is_param = |t: &Type| match t {
        Type::Path(p) => {
            p.qself.is_none() && generics.type_params().any(|g| p.path.is_ident(&g.ident))
        }
        _ => false,
    };
    let is_param_or_dyn = |t: &Type| is_param(t) || matches!(t, Type::TraitObject(_));
    match self_ty {
        t if is_param(t) => ImplKind::Blanket,
        Type::Reference(r) if is_param_or_dyn(&r.elem) => ImplKind::Forwarding,
        Type::Path(p) if p.qself.is_none() => {
            let Some(seg) = p.path.segments.last() else {
                return ImplKind::Concrete;
            };
            let wraps = matches!(seg.ident.to_string().as_str(), "Box" | "Rc" | "Arc");
            let inner = match &seg.arguments {
                syn::PathArguments::AngleBracketed(a) => a.args.first(),
                _ => None,
            };
            match inner {
                Some(syn::GenericArgument::Type(t)) if wraps && is_param_or_dyn(t) => {
                    ImplKind::Forwarding
                }
                _ => ImplKind::Concrete,
            }
        }
        _ => ImplKind::Concrete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fact, facts};

    fn refs(f: &FileFacts, name: &str) -> (u32, u32) {
        let get = |m: &HashMap<String, u32>| m.get(name).copied().unwrap_or(0);
        (get(&f.refs), get(&f.test_refs))
    }

    #[test]
    fn finds_test_ranges_and_decls() {
        let f = facts(
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n#[cfg(test)]\nmod more;\n#[cfg(not(test))]\nfn b() {}\n",
        );
        assert_eq!(f.test_ranges, vec![(2, 6), (4, 5), (7, 8)]);
        assert_eq!(f.mod_decls.len(), 1);
        assert!(f.mod_decls[0].test);
        assert!(fact(&f, "t").in_test);
        assert!(!fact(&f, "b").in_test);
    }

    #[test]
    fn records_decls_with_inline_path_and_path_attr() {
        let f = facts("mod inner { #[cfg(test)] mod tests; }\n#[path = \"x_tests.rs\"]\nmod x;");
        assert_eq!(f.mod_decls[0].inline_path, vec!["inner"]);
        assert!(f.mod_decls[0].test);
        assert_eq!(f.mod_decls[1].path_attr.as_deref(), Some("x_tests.rs"));
    }

    #[test]
    fn sees_items_inside_item_macros() {
        let f = facts("cfg_rt! {\n    #[cfg(test)]\n    mod tests;\n    pub fn spawn() {}\n}");
        assert_eq!(f.mod_decls.len(), 1);
        assert!(f.mod_decls[0].test);
        assert_eq!(fact(&f, "spawn").line, 4);
        assert_eq!(refs(&f, "spawn"), (0, 0));
    }

    #[test]
    fn classifies_impls_and_derives() {
        let f = facts(
            "trait A {}\nimpl A for X {}\nimpl<T: Clone> A for T {}\nimpl<T: A + ?Sized> A for Box<T> {}\nimpl A for &dyn B {}\nimpl X {}\n#[derive(Debug, serde::Serialize)]\nstruct Y;",
        );
        let kinds: Vec<_> = f.impls.iter().map(|i| i.kind).collect();
        use ImplKind::*;
        assert_eq!(
            kinds,
            vec![
                Concrete, Blanket, Forwarding, Forwarding, Inherent, Derive, Derive
            ]
        );
        assert_eq!(f.impls[6].trait_path, vec!["serde", "Serialize"]);
    }

    #[test]
    fn counts_refs_outside_definitions_and_imports() {
        let f = facts(
            r#"
            use crate::helper;
            use crate::plat::LinuxThing as Active;
            use self::Kind::*;
            pub use crate::api::Exported;
            fn helper() {}
            fn used_in_macro() {}
            fn by_serde() -> u32 { 1 }
            fn port() -> u16 { 1 }
            #[derive(Deserialize)]
            struct C { #[serde(default = "by_serde")] x: u32, #[builder(default = "port()")] p: u16, #[serde(with = "hex")] h: u8 }
            fn main() { assert_eq!(used_in_macro(), ()); }
            fn rec(n: u32) -> u32 { if n == 0 { 0 } else { rec(n - 1) } }
            #[cfg(test)]
            mod tests { fn t() { super::helper(); } }
            "#,
        );
        assert_eq!(refs(&f, "helper"), (0, 1));
        assert_eq!(refs(&f, "used_in_macro").0, 1);
        assert_eq!(refs(&f, "by_serde").0, 1);
        assert_eq!(refs(&f, "port").0, 1);
        assert_eq!(refs(&f, "serialize").0, 1);
        assert_eq!(refs(&f, "LinuxThing").0, 1);
        assert_eq!(refs(&f, "Kind").0, 1);
        assert_eq!(refs(&f, "Exported").0, 1);
        // Recursion is still a mention; `self_refs` lets findings discount it.
        assert_eq!(refs(&f, "rec"), (1, 0));
        assert_eq!(fact(&f, "rec").self_refs, 1);
    }

    #[test]
    fn records_test_fns_as_written() {
        let f = facts(
            r#"#[cfg(test)]
mod tests {
    mod inner {
        /// Doc.
        #[tokio::test(flavor = "multi_thread")]
        #[should_panic]
        async fn a() {
            let x = load(1, "p");
            check(x, 2);
        }
        #[test]
        fn b() {
            let x = load(1, "q");
            check(x, 3);
        }
    }
    #[rstest]
    fn r() {}
    #[test]
    fn p(x: u8) {}
}"#,
        );
        let t = |name: &str| fact(&f, name).test_fn.as_ref();
        let (a, b) = (t("a").unwrap(), t("b").unwrap());
        assert_eq!(a.scope, ["tests", "inner"]);
        assert_eq!(
            a.attrs,
            ["tokio::test (flavor = \"multi_thread\")", "should_panic"]
        );
        assert_eq!(b.attrs, ["test"]);
        // Only literals differ: the same names hash, literals apart where
        // they differ.
        assert_eq!(
            fact(&f, "a").body_hash.unwrap().names,
            fact(&f, "b").body_hash.unwrap().names
        );
        let same: Vec<bool> = a
            .literals
            .iter()
            .zip(&b.literals)
            .map(|(x, y)| x == y)
            .collect();
        assert_eq!(same, [true, false, false]);
        // Called with arguments: not a test as written.
        assert!(t("r").is_none() && t("p").is_none());
    }

    #[test]
    fn test_only_impl_fn_bodies_count_as_test() {
        let f = facts(
            "impl Foo {\n    #[cfg(test)]\n    fn helper(&self) -> u32 { only_for_tests() }\n}",
        );
        assert!(fact(&f, "Foo::helper").in_test);
        assert_eq!(refs(&f, "only_for_tests"), (0, 1));
    }

    #[test]
    fn notices_dynamic_includes() {
        assert!(facts("include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));").out_dir_include);
        assert!(!facts("include!(\"gen.rs\");").out_dir_include);
        assert!(facts("tonic::include_proto!(\"api\");").generated_code);
        assert!(!facts("include_str!(\"x.txt\");").generated_code);
    }

    #[test]
    fn type_self_mentions_are_tracked_apart() {
        let f = facts(
            "enum State { A, B }\nimpl State {\n    fn next(self) -> State { match self { State::A => State::B, _ => State::A } }\n}\nfn user() -> State { State::A }\nstruct List { next: Option<Box<List>> }",
        );
        assert_eq!(f.type_self_refs.get("State"), Some(&4));
        assert_eq!(refs(&f, "State").0, 6);
        assert_eq!(f.type_self_refs.get("List"), Some(&1));
    }

    #[test]
    fn doctests_only_count_compiling_fences() {
        let f = facts(
            "/// ```\n/// a();\n/// ```\n/// ~~~rust,no_run\n/// b();\n/// ~~~\n/// ```compile_fail\n/// c();\n/// ```\n/// ```text\n/// d();\n/// ```\nfn f() {}",
        );
        for (name, want) in [("a", 1), ("b", 1), ("c", 0), ("d", 0)] {
            assert_eq!(refs(&f, name).1, want, "{name}");
        }
    }

    #[test]
    fn unchecked_test_code_counts_as_use() {
        let f = facts(
            "#[cfg(all(test, loom))]\nmod a { #[test] fn t() { x(); } }\n#[cfg(loom)]\nmod b { #[test] fn t() { y(); } }\n#[cfg(test)]\nmod c { #[cfg(windows)] fn t() { z(); } }\n#[cfg(test)]\nmod d { #[test] fn t() { w(); } }",
        );
        for name in ["x", "y", "z"] {
            assert_eq!(refs(&f, name), (1, 0), "{name}");
        }
        assert_eq!(refs(&f, "w"), (0, 1));
    }

    #[test]
    fn raw_identifier_modules_resolve_unraw() {
        let f = facts("mod r#async;");
        assert_eq!(f.mod_decls[0].name, "async");
    }

    #[test]
    fn test_code_is_not_measured() {
        let f = facts(
            "fn a() { x.unwrap(); }\n#[cfg(test)]\nmod tests { fn helper() { x.unwrap(); } #[test] fn t() {} }\nimpl S { #[cfg(test)] fn only(&self) {} }",
        );
        assert!(fact(&f, "a").metrics.is_some());
        for n in ["helper", "t", "S::only"] {
            assert!(fact(&f, n).metrics.is_none(), "{n}");
        }
    }
}
