//! Small syntax-to-text helpers: paths, types and words as names.

use quote::ToTokens;
use syn::Type;

/// The identifier-like words of a text: what could name an item.
pub(crate) fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with(|c: char| c.is_alphabetic() || c == '_'))
}

/// A type for display: generics kept so different impls stay apart,
/// lifetimes dropped. Text output shortens long ones; JSON keeps them.
pub(super) fn display_type(ty: &Type) -> String {
    tidy_generics(compact(ty))
}

/// Drops lifetimes (`'a, ` / `, 'a` / `<'a>` / `&'a ` / ` + 'a`) from a
/// compact type or path string, and keeps char literals.
pub(super) fn tidy_generics(s: String) -> String {
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

pub(super) fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

pub(crate) fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

/// Token string without the spaces `to_string` puts around punctuation.
pub(crate) fn compact(t: &impl ToTokens) -> String {
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
    fn display_type_keeps_generics_without_lifetimes() {
        let ty: Type = syn::parse_str("Node<'p, Async, Sync>").unwrap();
        assert_eq!(display_type(&ty), "Node<Async, Sync>");
        let ty: Type = syn::parse_str("Wrapper<'a>").unwrap();
        assert_eq!(display_type(&ty), "Wrapper");
    }
}
