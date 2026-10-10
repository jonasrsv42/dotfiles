//! Walks a parsed file once and records the facts the reports need: which line
//! ranges are test code, module declarations, imports, every function (size,
//! body hashes, whether it just forwards, complexity and panic sites),
//! traits/types, impls and derives, and how often each name is mentioned.

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use syn::ext::IdentExt;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Expr, FnArg, Item, Meta, Pat, Stmt, Token, Type};

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
    /// Trait paths implemented inside `macro_rules!` bodies.
    pub macro_impls: Vec<Vec<String>>,
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
    /// items, if any.
    pub cfg: Option<String>,
    /// Set when the body is a single call that only passes its inputs along.
    pub forwards_to: Option<String>,
    /// For fns spanning a few lines.
    pub body_hash: Option<BodyHash>,
    /// Complexity and fragility measures; `None` for test code.
    pub metrics: Option<FnMetrics>,
    /// Where `metrics.panics` are, in source order (empty for test code).
    pub panic_sites: Vec<PanicSite>,
}

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
    /// Branching complexity, cognitive-style: 1, plus one per `if` (each
    /// `else if` too), `match` (however many arms: a flat match reads as one
    /// decision), `while`/`for`/`loop`, `&&`/`||` and `let .. else`. `?`
    /// adds nothing: early returns on error are the normal path. Macro arguments count when they parse as
    /// comma-separated expressions (`format!`, `vec!`, `assert!`), not
    /// otherwise (`tokio::select!`).
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
        let (metrics, panic_sites) = if in_test {
            (None, Vec::new())
        } else {
            let (m, sites) = fn_metrics(s.sig, s.block);
            (Some(m), sites)
        };
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
            cfg: (!cfgs.is_empty()).then(|| cfgs.join(" ")),
            forwards_to,
            // A lone `Self { a, b, c }` matches every constructor of its shape.
            body_hash: (self.fn_stack.len() < 3
                && end_line >= line + 4
                && !is_struct_literal(s.block))
            .then(|| body_hashes(s.block)),
            metrics,
            panic_sites,
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
            });
            return;
        }
        self.inline_path.push(name);
        visit::visit_item_mod(self, m);
        self.inline_path.pop();
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
        let fact = self.fn_fact(FnSite {
            name,
            attrs: &f.attrs,
            sig: &f.sig,
            block: &f.block,
            vis,
            kind,
            test_only,
        });
        let checked = is_checked_test(&f.attrs, self.ctx.test_features);
        self.visit_fn(fact, checked, |v| visit::visit_impl_item_fn(v, f));
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        let end = t.brace_token.span.close().end().line;
        self.push_item(ItemKind::Trait, &t.ident, &t.vis, &t.attrs, end);
        self.impl_ctx.push(ImplCtx {
            self_ty: t.ident.to_string(),
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
        let owner = self.impl_ctx.last().map_or("?", |c| c.self_ty.as_str());
        let fact = self.fn_fact(FnSite {
            name: format!("{owner}::{}", f.sig.ident),
            attrs: &f.attrs,
            sig: &f.sig,
            block,
            vis: Vis::Public,
            kind: FnKind::TraitDefault,
            test_only,
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
        if m.mac.path.is_ident("macro_rules") {
            self.facts
                .macro_impls
                .extend(macro_trait_impls(m.mac.tokens.clone()));
            return visit::visit_item_macro(self, m);
        }
        // Item-position macros like tokio's `cfg_rt! { pub mod runtime; }`
        // usually wrap plain items: parse and visit them as if unwrapped, so
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
            // `cfg_if! { if #[cfg(unix)] { mod unix; } else { .. } }` isn't
            // valid Rust, but its `mod x;`s are real.
            _ => {
                let mut decls = Vec::new();
                macro_mod_decls(m.mac.tokens.clone(), false, &mut decls);
                for (name, path_attr, test) in decls {
                    self.facts.modules.push(name.clone());
                    self.facts.mod_decls.push(ModDecl {
                        inline_path: self.inline_path.clone(),
                        name,
                        path_attr,
                        test: test || self.in_test(),
                        checked: self.ref_test_depth > 0 || (test && self.unchecked_depth == 0),
                    });
                }
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

/// The identifier-like words of a text: what could name an item.
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with(|c: char| c.is_alphabetic() || c == '_'))
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

/// `#[cfg(..)]` whose predicate can only hold under test (including `loom`
/// and `fuzzing` builds), or a test-function attribute (`#[test]`,
/// `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`).
pub fn is_test_only(attrs: &[Attribute], test_features: &HashSet<String>) -> bool {
    test_attr(attrs, test_features, false)
}

/// Test-only code that `cargo check --all-targets --all-features` builds:
/// like `is_test_only`, but not `loom`/`fuzzing`/other-platform code.
fn is_checked_test(attrs: &[Attribute], test_features: &HashSet<String>) -> bool {
    test_attr(attrs, test_features, true) && always_built(attrs)
}

/// Whether every `#[cfg]` among the attributes holds when the recommended
/// check runs (tests on, all features on), whatever the host: so no
/// `loom`, `fuzzing`, `unix`/`windows`/`target_*`, `not(..)`.
fn always_built(attrs: &[Attribute]) -> bool {
    fn holds(m: &Meta) -> bool {
        match m {
            Meta::Path(p) => p.is_ident("test") || p.is_ident("debug_assertions"),
            Meta::NameValue(nv) => nv.path.is_ident("feature"),
            Meta::List(l) => {
                let Ok(parts) = l.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                else {
                    return false;
                };
                if l.path.is_ident("all") {
                    parts.iter().all(holds)
                } else if l.path.is_ident("any") {
                    parts.iter().any(holds)
                } else {
                    false
                }
            }
        }
    }
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .all(|a| match &a.meta {
            Meta::List(l) => Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(l.tokens.clone())
                .is_ok_and(|p| p.iter().all(holds)),
            _ => false,
        })
}

fn test_attr(attrs: &[Attribute], test_features: &HashSet<String>, checked: bool) -> bool {
    attrs.iter().any(|a| {
        if a.path().is_ident("cfg") {
            return match &a.meta {
                Meta::List(l) => Punctuated::<Meta, Token![,]>::parse_terminated
                    .parse2(l.tokens.clone())
                    .is_ok_and(|p| p.len() == 1 && implies_test(&p[0], test_features, checked)),
                _ => false,
            };
        }
        let last = last_segment(a.path());
        last == "test" || last == "rstest" || last.starts_with("test_case")
    })
}

/// Does this cfg predicate being true imply we're building tests? `test` and
/// test-support features do (and `loom`/`fuzzing`, unless `checked`); `all`
/// if any part does; `any` if every part does.
fn implies_test(m: &Meta, test_features: &HashSet<String>, checked: bool) -> bool {
    match m {
        Meta::Path(p) => {
            p.is_ident("test") || (!checked && (p.is_ident("loom") || p.is_ident("fuzzing")))
        }
        Meta::NameValue(nv) if nv.path.is_ident("feature") => matches!(
            &nv.value,
            Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) if test_features.contains(&s.value())
        ),
        Meta::List(l) => {
            let Ok(parts) = l.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            else {
                return false;
            };
            if l.path.is_ident("all") {
                parts
                    .iter()
                    .any(|p| implies_test(p, test_features, checked))
            } else if l.path.is_ident("any") {
                // `any(test, doc)`: docs builds don't run code, so ignore them.
                let doc = |p: &Meta| p.path().is_ident("doc") || p.path().is_ident("doctest");
                let code: Vec<_> = parts.iter().filter(|p| !doc(p)).collect();
                !code.is_empty() && code.iter().all(|p| implies_test(p, test_features, checked))
            } else {
                false
            }
        }
        _ => false,
    }
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

/// `[#[cfg(test)]] [#[path = ".."]] [pub] mod name;` in macro tokens, as
/// (name, path, under `#[cfg(test)]`). A `#[cfg(test)]` before a `{ .. }`
/// group (`cfg_if!`'s `if #[cfg(test)] { .. }`) covers what's inside.
fn macro_mod_decls(tokens: TokenStream, test: bool, out: &mut Vec<(String, Option<String>, bool)>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let (mut cfg_test, mut path) = (false, None);
    let mut i = 0;
    while i < toks.len() {
        match (&toks[i], toks.get(i + 1), toks.get(i + 2)) {
            (TokenTree::Punct(p), Some(TokenTree::Group(g)), _)
                if p.as_char() == '#' && g.delimiter() == Delimiter::Bracket =>
            {
                let attr = g.stream().to_string().replace(' ', "");
                if attr == "cfg(test)" {
                    cfg_test = true;
                } else if let Some(p) = attr.strip_prefix("path=") {
                    path = Some(p.trim_matches('"').to_string());
                }
                i += 2;
                continue;
            }
            (TokenTree::Ident(kw), Some(TokenTree::Ident(name)), Some(TokenTree::Punct(semi)))
                if kw == "mod" && semi.as_char() == ';' =>
            {
                out.push((name.unraw().to_string(), path.take(), test || cfg_test));
                cfg_test = false;
                i += 3;
                continue;
            }
            (TokenTree::Ident(kw), ..) if kw == "pub" => {}
            (TokenTree::Group(g), ..) => {
                macro_mod_decls(g.stream(), test || cfg_test, out);
                cfg_test = false;
                path = None;
            }
            _ => {
                cfg_test = false;
                path = None;
            }
        }
        i += 1;
    }
}

/// Trait paths in `impl .. Trait for ..` headers inside a macro_rules body.
/// `$crate::Trait` comes back as `crate::Trait`.
fn macro_trait_impls(tokens: TokenStream) -> Vec<Vec<String>> {
    #[derive(PartialEq)]
    enum Tok {
        Ident(String),
        Punct(char),
        Open,
    }
    fn flatten(ts: TokenStream, out: &mut Vec<Tok>) {
        for tt in ts {
            match tt {
                TokenTree::Ident(i) => out.push(Tok::Ident(i.to_string())),
                TokenTree::Punct(p) => out.push(Tok::Punct(p.as_char())),
                TokenTree::Literal(_) => out.push(Tok::Punct('l')),
                TokenTree::Group(g) => {
                    out.push(Tok::Open);
                    flatten(g.stream(), out);
                    out.push(Tok::Punct(')'));
                }
            }
        }
    }
    let mut toks = Vec::new();
    flatten(tokens, &mut toks);
    let mut out = Vec::new();
    for i in 0..toks.len() {
        if toks[i] != Tok::Ident("impl".into()) {
            continue;
        }
        let mut angle = 0i32;
        for j in i + 1..(i + 48).min(toks.len()) {
            match &toks[j] {
                Tok::Open => break,
                Tok::Punct('<') => angle += 1,
                Tok::Punct('>') => angle -= 1,
                Tok::Ident(s) if s == "for" && angle == 0 => {
                    // Walk back over `$crate :: a :: Trait < .. >`.
                    let mut k = j;
                    let mut depth = 0i32;
                    while k > i + 1 {
                        match &toks[k - 1] {
                            Tok::Punct('>') => depth += 1,
                            Tok::Punct('<') => depth -= 1,
                            _ if depth > 0 => {}
                            _ => break,
                        }
                        k -= 1;
                    }
                    let mut path = Vec::new();
                    while k > i + 1 {
                        match &toks[k - 1] {
                            Tok::Ident(s) => path.push(s.clone()),
                            Tok::Punct(':') | Tok::Punct('$') => {}
                            _ => break,
                        }
                        k -= 1;
                    }
                    path.reverse();
                    if !path.is_empty() {
                        out.push(path);
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    out
}

/// If the block is just one call that passes the fn's own inputs along
/// (params, `self`, their fields, references and trivial conversions of
/// those), returns the callee. `Ok(..)`, `Some(..)`, `?`, `.await` and
/// `.into()` around it are looked through. Not forwarding: fns without
/// inputs (`Duration::from_secs(30)` binds a constant), constructors
/// (`Id(raw)`), and turbofish calls (`work_loop::<T>(..)` specialises).
fn forward_target(sig: &syn::Signature, block: &Block) -> Option<String> {
    let params: Vec<String> = sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Receiver(_) => Some("self".to_string()),
            FnArg::Typed(t) => match &*t.pat {
                Pat::Ident(i) => Some(i.ident.to_string()),
                _ => None,
            },
        })
        .collect();
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

fn body_hashes(block: &Block) -> BodyHash {
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
    walk(block.to_token_stream(), &mut h);
    BodyHash {
        exact: h.exact.finish(),
        shape: h.shape.finish(),
        names: h.names.finish(),
    }
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

fn fn_metrics(sig: &syn::Signature, block: &Block) -> (FnMetrics, Vec<PanicSite>) {
    let mut m = MetricsWalk {
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

/// A second, small walk over one fn body: the main visitor's bookkeeping
/// (test depth, refs) doesn't fit counting per fn, and nested fns must
/// start from zero.
struct MetricsWalk {
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
                    let after_dot =
                        i > 0 && matches!(&toks[i - 1], TokenTree::Punct(p) if p.as_char() == '.');
                    // Number of call arguments, if a call follows.
                    let args = match toks.get(i + 1) {
                        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                            Some(top_level_args(g.stream()))
                        }
                        _ => None,
                    };
                    let is_punct = |j: usize, c: char| matches!(toks.get(j), Some(TokenTree::Punct(p)) if p.as_char() == c);
                    let panic = match args {
                        Some(0) => after_dot && id == "unwrap",
                        Some(1) => after_dot && id == "expect" && !is_punct(i + 2, '?'),
                        Some(_) => false,
                        None => {
                            !after_dot
                                && is_punct(i + 1, '!')
                                && PANIC_MACROS.iter().any(|m| id == m)
                        }
                    };
                    if panic {
                        let kind = if after_dot {
                            id.to_string()
                        } else {
                            format!("{id}!")
                        };
                        self.panic_at(id.span(), kind);
                    }
                    self.m.clones += u32::from(after_dot && args == Some(0) && id == "clone");
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

/// A type for display: generics kept so different impls stay apart,
/// lifetimes dropped. Text output shortens long ones; JSON keeps them.
fn display_type(ty: &Type) -> String {
    tidy_generics(compact(ty))
}

/// Drops lifetimes (`'a, ` / `, 'a` / `<'a>` / `&'a ` / ` + 'a`) from a
/// compact type or path string, and keeps char literals.
fn tidy_generics(s: String) -> String {
    let c: Vec<char> = s.chars().collect();
    let is_ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let lifetime = c[i] == '\'' && c.get(i + 1).is_some_and(|&n| n.is_alphabetic() || n == '_');
        if !lifetime {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < c.len() && is_ident(c[j]) {
            j += 1;
        }
        if c.get(j) == Some(&'\'') {
            out.extend(&c[i..=j]); // a char literal `'x'`
            i = j + 1;
            continue;
        }
        i = j;
        let rest: String = c[i..c.len().min(i + 2)].iter().collect();
        if rest == ", " {
            i += 2;
        } else if rest.starts_with(' ') {
            i += 1;
        } else {
            for tail in [", ", " + "] {
                if out.ends_with(tail) {
                    out.truncate(out.len() - tail.len());
                }
            }
        }
    }
    out.replace("<>", "")
}

fn is_struct_literal(block: &Block) -> bool {
    matches!(block.stmts.as_slice(), [Stmt::Expr(Expr::Struct(_), None)])
}

/// The `#[cfg(..)]` predicates among the attributes, as strings.
fn cfg_attrs(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| a.meta.to_token_stream().to_string())
        .collect()
}

fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

/// Token string without the spaces `to_string` puts around punctuation.
fn compact(t: &impl ToTokens) -> String {
    t.to_token_stream()
        .to_string()
        .replace(" :: ", "::")
        .replace(" . ", ".")
        .replace(" < ", "<")
        .replace("::< ", "::<")
        .replace(" >", ">")
        .replace("& ", "&")
        .replace(" ,", ",")
        .replace(",>", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_with(src: &str, features: &[&str]) -> FileFacts {
        let test_features = features.iter().map(|s| s.to_string()).collect();
        let ctx = Ctx {
            test_features: &test_features,
            build_script: false,
        };
        collect(&syn::parse_file(src).unwrap(), &ctx)
    }

    fn facts(src: &str) -> FileFacts {
        facts_with(src, &[])
    }

    fn fact<'a>(f: &'a FileFacts, name: &str) -> &'a FnFact {
        f.fns
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no fn {name}"))
    }

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
    fn evaluates_cfg_predicates() {
        let test = |cfg: &str| {
            let f = facts_with(&format!("#[cfg({cfg})]\nfn f() {{}}"), &["testdb"]);
            fact(&f, "f").in_test
        };
        assert!(test("test"));
        assert!(test("all(test, not(miri))"));
        assert!(test("all(unix, test)"));
        assert!(test("any(test, feature = \"testdb\")"));
        assert!(test("feature = \"testdb\""));
        assert!(!test("not(test)"));
        assert!(!test("any(test, feature = \"esp32\")"));
        assert!(!test("all(feature = \"contest\", unix)"));
        assert!(facts("#![cfg(test)]\nfn f() {}").file_test);
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
    fn nested_items_and_macro_impls() {
        let f = facts(
            "fn outer() {\n    let a = 1;\n    fn inner() {\n        let b = 2;\n    }\n}\nmacro_rules! m { ($t:ty) => { impl $crate::Tr<u8> for $t {} }; }",
        );
        assert_eq!(fact(&f, "outer").nested, vec![(3, 5)]);
        assert_eq!(
            f.macro_impls,
            vec![vec!["crate".to_string(), "Tr".to_string()]]
        );
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

    #[test]
    fn tidy_generics_drops_lifetimes_only() {
        for (input, want) in [
            ("&'static str", "&str"),
            ("Box<dyn Fn() + 'params>", "Box<dyn Fn()>"),
            ("Node<'p, A>", "Node<A>"),
            ("Foo<'x'>", "Foo<'x'>"),
        ] {
            assert_eq!(tidy_generics(input.to_string()), want);
        }
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
    fn test_like_cfgs() {
        let test = |cfg: &str| {
            let f = facts(&format!("#[cfg({cfg})]\nfn f() {{}}"));
            fact(&f, "f").in_test
        };
        assert!(test("any(test, doc)"));
        assert!(test("loom"));
        assert!(test("any(test, fuzzing)"));
        assert!(!test("doc"));
        assert!(!test("any(test, unix)"));
    }

    #[test]
    fn cfg_if_mod_decls_are_seen() {
        let f = facts(
            "cfg_if::cfg_if! {\n    if #[cfg(unix)] {\n        mod unix;\n    } else {\n        #[path = \"win.rs\"]\n        mod windows;\n    }\n}\ncfg_if::cfg_if! { if #[cfg(test)] { mod tests; } }",
        );
        let decls: Vec<_> = f
            .mod_decls
            .iter()
            .map(|d| (d.name.as_str(), d.path_attr.as_deref(), d.test))
            .collect();
        assert_eq!(
            decls,
            vec![
                ("unix", None, false),
                ("windows", Some("win.rs"), false),
                ("tests", None, true)
            ]
        );
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
        // Token scan: one argument and no `?` only.
        assert_eq!(
            m("m!(p.expect(t)?; q.expect(a, b); r.expect(\"x\"))").panics,
            1
        );
    }

    #[test]
    fn statement_item_macros_count_toward_their_fn() {
        // syn sees `m! { .. }` in a body as a statement macro, so `inner`
        // gets no fact of its own: its panic sites land in `outer`.
        let f = facts("fn outer() { my_macro! { fn inner() { x.unwrap(); } } }");
        assert_eq!(fact(&f, "outer").metrics.unwrap().panics, 1);
        assert!(f.fns.iter().all(|f| f.name != "inner"));
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

    #[test]
    fn display_type_keeps_generics_without_lifetimes() {
        let ty: Type = syn::parse_str("Node<'p, Async, Sync>").unwrap();
        assert_eq!(display_type(&ty), "Node<Async, Sync>");
        let ty: Type = syn::parse_str("Wrapper<'a>").unwrap();
        assert_eq!(display_type(&ty), "Wrapper");
    }
}
