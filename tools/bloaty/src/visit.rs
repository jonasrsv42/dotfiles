//! Walks a parsed file once and records the facts the reports need: which line
//! ranges are test code, module declarations, imports, every function (size,
//! body hashes, whether it just forwards), traits/types, impls and derives, and
//! how often each name is mentioned.

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
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
    /// `include!` of a non-literal path (usually build-script output): code
    /// we can't see may use this crate's items.
    pub dynamic_include: bool,
    /// 1-based inclusive line ranges of test-only items.
    pub test_ranges: Vec<(usize, usize)>,
    /// `mod foo;` declarations: the module lives in another file.
    pub mod_decls: Vec<ModDecl>,
    /// Every module name declared here, inline or not (for path resolution).
    pub modules: Vec<String>,
    /// `use` imports as (name in scope, full path).
    pub imports: Vec<(String, Vec<String>)>,
    /// Glob imports' paths (`use a::prelude::*` -> `["a", "prelude"]`).
    pub glob_imports: Vec<Vec<String>>,
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
    pub test: bool,
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
    pub line: usize,
    pub end_line: usize,
    /// Line ranges of items nested in the body (fns, impls, ...).
    pub nested: Vec<(usize, usize)>,
    pub in_test: bool,
    pub vis: Vis,
    pub kind: FnKind,
    /// An entry point or otherwise externally referenced fn: extern ABI,
    /// `#[no_mangle]`, `main`, proc-macro or framework attributes (on the fn
    /// or its impl block, like `#[pymethods]`)...
    pub exempt: bool,
    /// The non-test `#[cfg(..)]` predicates on the fn, if any.
    pub cfg: Option<String>,
    /// Set when the body is a single call that only passes its inputs along.
    pub forwards_to: Option<String>,
    /// For fns spanning a few lines.
    pub body_hash: Option<BodyHash>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemKind {
    Trait,
    Struct,
    Enum,
}

pub struct ItemFact {
    pub kind: ItemKind,
    pub name: String,
    pub vis: Vis,
    pub line: usize,
    pub end_line: usize,
    pub in_test: bool,
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
    pub line: usize,
    pub end_line: usize,
    pub kind: ImplKind,
}

pub fn collect(file: &syn::File, ctx: &Ctx) -> FileFacts {
    let file_test = is_test_only(&file.attrs, ctx.test_features);
    let mut v = Visitor {
        ctx,
        test_depth: usize::from(file_test),
        inline_path: Vec::new(),
        impl_ctx: Vec::new(),
        fn_stack: Vec::new(),
        defs: HashSet::new(),
        facts: FileFacts {
            file_test,
            ..Default::default()
        },
    };
    v.visit_file(file);
    v.facts
}

/// For files syn can't parse: every identifier in the token stream counts as
/// a reference, so items they use aren't reported unused.
pub fn token_refs(tokens: TokenStream) -> HashMap<String, u32> {
    fn walk(ts: TokenStream, out: &mut HashMap<String, u32>) {
        for tt in ts {
            match tt {
                TokenTree::Ident(i) => *out.entry(i.to_string()).or_default() += 1,
                TokenTree::Group(g) => walk(g.stream(), out),
                _ => {}
            }
        }
    }
    let mut out = HashMap::new();
    walk(tokens, &mut out);
    out
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
    test_depth: usize,
    inline_path: Vec<String>,
    impl_ctx: Vec<ImplCtx>,
    /// Indices into `facts.fns` of the fns we're inside.
    fn_stack: Vec<usize>,
    /// Positions of identifiers that define something; not counted as refs.
    defs: HashSet<(usize, usize)>,
    facts: FileFacts,
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
        // A fn calling itself (recursion, or `fn len(&self) { self.v.len() }`)
        // doesn't keep it alive.
        if self
            .fn_stack
            .iter()
            .any(|&i| self.facts.fns[i].ident == name)
        {
            return;
        }
        let map = if self.in_test() {
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
        for word in inner.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if word.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                self.count_ref(word);
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

    /// Records a fn and visits its body with it on the fn stack.
    fn visit_fn(&mut self, f: FnFact, visit_body: impl FnOnce(&mut Self)) {
        let test_only = f.in_test && !self.in_test();
        self.facts.fns.push(f);
        self.fn_stack.push(self.facts.fns.len() - 1);
        self.test_depth += usize::from(test_only);
        visit_body(self);
        self.test_depth -= usize::from(test_only);
        self.fn_stack.pop();
    }

    fn fn_fact(&self, s: FnSite) -> FnFact {
        let line = s.sig.fn_token.span.start().line;
        let end_line = s.block.brace_token.span.close().end().line;
        let in_test = self.in_test() || s.test_only;
        let in_exempt_impl = self.impl_ctx.last().is_some_and(|c| c.exempt);
        let exempt = in_exempt_impl
            || s.sig.abi.is_some()
            || s.sig.ident == "main"
            || s.attrs.iter().any(|a| !is_plain_attr(a));
        let cfgs: Vec<String> = s
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("cfg"))
            .map(|a| a.meta.to_token_stream().to_string())
            .collect();
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
            line,
            end_line,
            nested: Vec::new(),
            in_test,
            vis: s.vis,
            kind: s.kind,
            exempt,
            cfg: (!cfgs.is_empty()).then(|| cfgs.join(" ")),
            forwards_to,
            body_hash: (end_line >= line + 4).then(|| body_hashes(s.block)),
        }
    }

    fn push_item(&mut self, kind: ItemKind, ident: &syn::Ident, vis: &syn::Visibility, end: usize) {
        self.define(ident);
        self.facts.items.push(ItemFact {
            kind,
            name: ident.to_string(),
            vis: vis_of(vis),
            line: ident.span().start().line,
            end_line: end,
            in_test: self.in_test(),
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
        self.push_item(kind, ident, vis, end.end().line);
    }

    fn record_derives(&mut self, attrs: &[Attribute], ident: &syn::Ident) {
        for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
            let line = attr.pound_token.span.start().line;
            let _ = attr.parse_nested_meta(|meta| {
                self.facts.impls.push(ImplFact {
                    trait_path: path_segments(&meta.path),
                    self_ty: ident.to_string(),
                    self_name: ident.to_string(),
                    line,
                    end_line: line,
                    kind: ImplKind::Derive,
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
        if is_test_only(item_attrs(item), self.ctx.test_features) {
            self.facts.test_ranges.push(item_range(item));
            self.test_depth += 1;
            visit::visit_item(self, item);
            self.test_depth -= 1;
        } else {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let name = m.ident.to_string();
        self.facts.modules.push(name.clone());
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
            });
            return;
        }
        self.inline_path.push(name);
        visit::visit_item_mod(self, m);
        self.inline_path.pop();
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        collect_use(
            &u.tree,
            &mut Vec::new(),
            &mut self.facts.imports,
            &mut self.facts.glob_imports,
        );
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
        self.visit_fn(fact, |v| visit::visit_item_fn(v, f));
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
            line: i.impl_token.span.start().line,
            end_line: i.brace_token.span.close().end().line,
            kind,
        });
        self.impl_ctx.push(ImplCtx {
            self_ty,
            trait_name: i.trait_.as_ref().map(|(_, path, _)| compact(path)),
            exempt: i.attrs.iter().any(|a| !is_plain_attr(a)),
        });
        visit::visit_item_impl(self, i);
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
        self.visit_fn(fact, |v| visit::visit_impl_item_fn(v, f));
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        let end = t.brace_token.span.close().end().line;
        self.push_item(ItemKind::Trait, &t.ident, &t.vis, end);
        self.impl_ctx.push(ImplCtx {
            self_ty: t.ident.to_string(),
            trait_name: None,
            exempt: false,
        });
        visit::visit_item_trait(self, t);
        self.impl_ctx.pop();
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.define(&f.sig.ident);
        let Some(block) = &f.default else {
            return visit::visit_trait_item_fn(self, f);
        };
        let owner = self.impl_ctx.last().map_or("?", |c| c.self_ty.as_str());
        let fact = self.fn_fact(FnSite {
            name: format!("{owner}::{}", f.sig.ident),
            attrs: &f.attrs,
            sig: &f.sig,
            block,
            vis: Vis::Public,
            kind: FnKind::TraitDefault,
            test_only: false,
        });
        self.visit_fn(fact, |v| visit::visit_trait_item_fn(v, f));
    }

    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        let end = match &s.fields {
            syn::Fields::Named(f) => f.brace_token.span.close(),
            syn::Fields::Unnamed(f) => f.paren_token.span.close(),
            syn::Fields::Unit => s.ident.span(),
        };
        self.type_item(ItemKind::Struct, &s.ident, &s.vis, &s.attrs, end);
        visit::visit_item_struct(self, s);
    }

    fn visit_item_enum(&mut self, e: &'ast syn::ItemEnum) {
        let end = e.brace_token.span.close();
        self.type_item(ItemKind::Enum, &e.ident, &e.vis, &e.attrs, end);
        visit::visit_item_enum(self, e);
    }

    fn visit_item_union(&mut self, u: &'ast syn::ItemUnion) {
        self.define(&u.ident);
        self.facts.type_names.push(u.ident.to_string());
        self.record_derives(&u.attrs, &u.ident);
        visit::visit_item_union(self, u);
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
        // their modules, fns and definitions are seen.
        match syn::parse2::<syn::File>(m.mac.tokens.clone()) {
            Ok(file) if !file.items.is_empty() => {
                for a in &m.attrs {
                    self.visit_attribute(a);
                }
                self.visit_path(&m.mac.path);
                for item in &file.items {
                    self.visit_item(item);
                }
            }
            _ => visit::visit_item_macro(self, m),
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if last_segment(&m.path) == "include"
            && syn::parse2::<syn::LitStr>(m.tokens.clone()).is_err()
        {
            self.facts.dynamic_include = true;
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

/// `#[cfg(..)]` whose predicate can only hold under test, or a test-function
/// attribute (`#[test]`, `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`).
pub fn is_test_only(attrs: &[Attribute], test_features: &HashSet<String>) -> bool {
    attrs.iter().any(|a| {
        if a.path().is_ident("cfg") {
            return match &a.meta {
                Meta::List(l) => Punctuated::<Meta, Token![,]>::parse_terminated
                    .parse2(l.tokens.clone())
                    .is_ok_and(|p| p.len() == 1 && implies_test(&p[0], test_features)),
                _ => false,
            };
        }
        let last = last_segment(a.path());
        last == "test" || last == "rstest" || last.starts_with("test_case")
    })
}

/// Does this cfg predicate being true imply we're building tests? `test` and
/// test-support features do; `all` if any part does; `any` if every part does.
fn implies_test(m: &Meta, test_features: &HashSet<String>) -> bool {
    match m {
        Meta::Path(p) => p.is_ident("test"),
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
                parts.iter().any(|p| implies_test(p, test_features))
            } else if l.path.is_ident("any") {
                !parts.is_empty() && parts.iter().all(|p| implies_test(p, test_features))
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
    globs: &mut Vec<Vec<String>>,
) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            collect_use(&p.tree, prefix, out, globs);
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
        syn::UseTree::Group(g) => g
            .items
            .iter()
            .for_each(|t| collect_use(t, prefix, out, globs)),
        syn::UseTree::Glob(_) => globs.push(prefix.clone()),
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

/// A type for display: generics kept so different impls stay apart,
/// lifetimes dropped, very long names cut.
fn display_type(ty: &Type) -> String {
    let mut s = compact(ty);
    // Drop lifetime arguments: `'a, ` / `, 'a` / `<'a>`.
    while let Some(i) = s.find('\'') {
        let end = s[i + 1..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(s.len(), |e| i + 1 + e);
        let (mut a, mut b) = (i, end);
        if s[b..].starts_with(", ") {
            b += 2;
        } else if s[..a].ends_with(", ") {
            a -= 2;
        }
        s.replace_range(a..b, "");
    }
    let s = s.replace("<>", "");
    if s.chars().count() > 100 {
        format!("{}…", s.chars().take(99).collect::<String>())
    } else {
        s
    }
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
        assert_eq!(refs(&f, "rec"), (0, 0));
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
        assert!(facts("include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));").dynamic_include);
        assert!(!facts("include!(\"gen.rs\");").dynamic_include);
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
    fn display_type_keeps_generics_without_lifetimes() {
        let ty: Type = syn::parse_str("Node<'p, Async, Sync>").unwrap();
        assert_eq!(display_type(&ty), "Node<Async, Sync>");
        let ty: Type = syn::parse_str("Wrapper<'a>").unwrap();
        assert_eq!(display_type(&ty), "Wrapper");
    }
}
