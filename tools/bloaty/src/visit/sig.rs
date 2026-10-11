//! A fn's signature as written, its name left out: with an equal body,
//! what makes two copies the same code. Token-normalized (spacing, trailing
//! commas, lifetimes dropped outside `for<..>`), never resolved: `Vec<u8>`
//! and an alias of it differ, `<T>` and `<U>` differ, so do `<T: X>` and
//! `where T: X`.

use super::syntax::{compact, tidy_generics};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;

/// How a method takes `self`.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Receiver {
    /// A free fn or associated fn without `self`.
    None,
    /// `self` (also `mut self`).
    Value,
    /// `&self`.
    Ref,
    /// `&mut self`.
    RefMut,
    /// `self: Pin<&mut Self>` and other typed receivers, with the type.
    Typed(String),
}

/// What a fn inside an impl or trait belongs to.
#[derive(Clone, Default, Debug)]
pub struct Owner {
    /// The impl's Self type (none for trait default methods).
    pub self_ty: Option<String>,
    /// The impl's (or trait's) generic parameters and where clause.
    pub generics: String,
    /// The trait implemented or declared.
    pub trait_name: Option<String>,
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct Signature {
    /// `const async unsafe extern "C"`, as written.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub qualifiers: String,
    /// Generic parameters with their bounds, then the where clause.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub generics: String,
    pub receiver: Receiver,
    /// Parameter types, `self` and names left out.
    pub params: Vec<String>,
    /// Empty for `()`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ret: String,
    /// The impl's Self type, for fns in impls (left out for associated fns
    /// that never mention `Self` or `self`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_ty: Option<String>,
    /// The impl's generics and where clause.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub impl_generics: String,
    /// The trait implemented (or, for default methods, declared).
    #[serde(rename = "trait", skip_serializing_if = "Option::is_none")]
    pub trait_name: Option<String>,
}

/// A part of a signature (see `Signature::differs`).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    SelfType,
    ImplGenerics,
    Trait,
    Receiver,
    Generics,
    Params,
    Return,
    Qualifiers,
}

impl Part {
    pub const ALL: [Part; 8] = [
        Part::SelfType,
        Part::ImplGenerics,
        Part::Trait,
        Part::Receiver,
        Part::Generics,
        Part::Params,
        Part::Return,
        Part::Qualifiers,
    ];

    pub fn text(self) -> &'static str {
        match self {
            Part::SelfType => "Self type",
            Part::ImplGenerics => "impl generics",
            Part::Trait => "trait",
            Part::Receiver => "receiver",
            Part::Generics => "generics",
            Part::Params => "parameters",
            Part::Return => "return type",
            Part::Qualifiers => "qualifiers",
        }
    }
}

/// Normalized text: spacing, trailing commas; lifetimes dropped unless
/// higher-ranked (`for<'de> Deserialize<'de>` stays as written).
pub(super) fn text(t: &impl ToTokens) -> String {
    // Generics compact to `< 'a>` and `<T : Read>`.
    let s = compact(t).replace("< ", "<").replace(" : ", ": ");
    let s = s.trim_end_matches([',', ' ']).to_string();
    if s.contains("for<") {
        s
    } else {
        tidy_generics(s)
    }
}

/// Generic parameters, then the where clause: `<T: X> where T: Y`.
pub(super) fn generics_text(g: &syn::Generics) -> String {
    let where_clause = g.where_clause.as_ref().map(text);
    [Some(text(g)), where_clause]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `Self` or `self` appears in the tokens.
fn mentions_self(ts: TokenStream) -> bool {
    ts.into_iter().any(|t| match t {
        TokenTree::Ident(i) => i == "Self" || i == "self",
        TokenTree::Group(g) => mentions_self(g.stream()),
        _ => false,
    })
}

impl Signature {
    /// `block` decides whether an associated fn without `self` is tied to
    /// its Self type at all.
    pub fn new(sig: &syn::Signature, block: &syn::Block, owner: Owner) -> Self {
        let receiver = match sig.receiver() {
            None => Receiver::None,
            Some(r) if r.colon_token.is_some() => Receiver::Typed(text(&r.ty)),
            Some(r) if r.reference.is_none() => Receiver::Value,
            Some(r) if r.mutability.is_some() => Receiver::RefMut,
            Some(_) => Receiver::Ref,
        };
        let detached = receiver == Receiver::None
            && !mentions_self(sig.to_token_stream())
            && !mentions_self(block.to_token_stream());
        let (self_ty, impl_generics) = if detached {
            (None, String::new())
        } else {
            (owner.self_ty, owner.generics)
        };
        let params = sig
            .inputs
            .iter()
            .filter_map(|a| match a {
                syn::FnArg::Typed(p) => Some(text(&p.ty)),
                syn::FnArg::Receiver(_) => None,
            })
            .collect();
        let ret = match &sig.output {
            syn::ReturnType::Default => String::new(),
            syn::ReturnType::Type(_, ty) => text(ty),
        };
        let quals = [
            sig.constness.map(|_| "const".to_string()),
            sig.asyncness.map(|_| "async".to_string()),
            sig.unsafety.map(|_| "unsafe".to_string()),
            sig.abi.as_ref().map(text),
        ];
        Signature {
            qualifiers: quals.into_iter().flatten().collect::<Vec<_>>().join(" "),
            generics: generics_text(&sig.generics),
            receiver,
            params,
            ret,
            self_ty,
            impl_generics,
            trait_name: owner.trait_name,
        }
    }

    fn same(&self, other: &Signature, part: Part) -> bool {
        match part {
            Part::SelfType => self.self_ty == other.self_ty,
            Part::ImplGenerics => self.impl_generics == other.impl_generics,
            Part::Trait => self.trait_name == other.trait_name,
            Part::Receiver => self.receiver == other.receiver,
            Part::Generics => self.generics == other.generics,
            Part::Params => self.params == other.params,
            Part::Return => self.ret == other.ret,
            Part::Qualifiers => self.qualifiers == other.qualifiers,
        }
    }

    /// The parts in which any of `sigs` differs from the first.
    pub fn differs<'a>(sigs: impl Iterator<Item = &'a Signature> + Clone) -> Vec<Part> {
        let mut first = sigs.clone();
        let Some(first) = first.next() else {
            return Vec::new();
        };
        Part::ALL
            .into_iter()
            .filter(|&p| sigs.clone().any(|s| !s.same(first, p)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(src: &str) -> Signature {
        let f: syn::ItemFn = syn::parse_str(&format!("{src} {{}}")).unwrap();
        Signature::new(&f.sig, &f.block, Owner::default())
    }

    /// A method `src { body }` on `ty` in `impl<generics>`.
    fn method(src: &str, body: &str, ty: &str, generics: &str) -> Signature {
        let i: syn::ImplItemFn = syn::parse_str(&format!("{src} {{ {body} }}")).unwrap();
        let owner = Owner {
            self_ty: Some(ty.into()),
            generics: generics.into(),
            trait_name: None,
        };
        Signature::new(&i.sig, &i.block, owner)
    }

    #[test]
    fn names_spacing_and_lifetimes_dont_count() {
        assert_eq!(
            sig("fn same(left: &[u8], right: &[u8]) -> bool"),
            sig("fn other<'a>(a: &'a [u8], b :&[ u8 ]) -> bool")
        );
        let s = sig("pub async unsafe fn f<T: Read>(x: T) -> Vec<T> where T: Send");
        assert_eq!(s.qualifiers, "async unsafe");
        assert_eq!(s.generics, "<T: Read> where T: Send");
        assert_eq!((s.params, s.ret), (vec!["T".to_string()], "Vec<T>".into()));
        assert_eq!(
            sig("fn w<T>(x: &T) where T: Debug + Clone,"),
            sig("fn w<T,>(x: &T) where T: Debug + Clone")
        );
    }

    #[test]
    fn higher_ranked_bounds_keep_their_lifetimes() {
        let h = sig("fn h<T>(x: &T) where T: for<'de> De<'de>");
        assert_eq!(h.generics, "<T> where T: for<'de> De<'de>");
        assert_ne!(h, sig("fn h<T>(x: &T) where T: De<'static>"));
    }

    #[test]
    fn differing_parts() {
        let a = method("fn data(self) -> Vec<u8>", "", "Message", "");
        let b = method("fn as_data(&self) -> &[u8]", "", "Message", "");
        let c = method("fn data(self) -> Vec<u8>", "", "Other", "");
        assert_eq!(
            Signature::differs([&a, &b].into_iter()),
            [Part::Receiver, Part::Return]
        );
        assert_eq!(Signature::differs([&a, &c].into_iter()), [Part::SelfType]);
        assert!(Signature::differs([&a, &a].into_iter()).is_empty());
        assert_eq!(
            method("fn f(&mut self)", "", "A", "").receiver,
            Receiver::RefMut
        );
        let pinned = method("fn f(self: Pin<&mut Self>)", "", "A", "");
        assert_eq!(pinned.receiver, Receiver::Typed("Pin<&mut Self>".into()));
        // The impl's generics count too.
        let ta = method("fn run(&self) -> u32", "", "Foo<T>", "<T: Ta>");
        let tb = method("fn run(&self) -> u32", "", "Foo<T>", "<T: Tb>");
        assert_eq!(
            Signature::differs([&ta, &tb].into_iter()),
            [Part::ImplGenerics]
        );
    }

    /// An associated fn that never mentions `Self`/`self` isn't tied to
    /// its type; one that does is.
    #[test]
    fn detached_associated_fns() {
        let a = method("fn cols(c: &[u8]) -> bool", "c.is_empty()", "A", "");
        let b = method("fn cols(c: &[u8]) -> bool", "c.is_empty()", "B", "<T>");
        assert_eq!(a, b);
        assert_eq!(a.self_ty, None);
        let a = method("fn new() -> Self", "Self::default()", "A", "");
        let b = method("fn new() -> Self", "Self::default()", "B", "");
        assert_ne!(a, b);
    }
}
