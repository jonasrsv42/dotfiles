//! Which code is test code: `#[cfg]` predicates and test attributes.

use super::syntax::last_segment;
use quote::ToTokens;
use std::collections::HashSet;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, Meta, Token};

/// `#[cfg(..)]` whose predicate can only hold under test (including `loom`
/// and `fuzzing` builds), or a test-function attribute (`#[test]`,
/// `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`).
pub(super) fn is_test_only(attrs: &[Attribute], test_features: &HashSet<String>) -> bool {
    test_attr(attrs, test_features, false)
}

/// Test-only code that `cargo check --all-targets --all-features` builds:
/// like `is_test_only`, but not `loom`/`fuzzing`/other-platform code.
pub(super) fn is_checked_test(attrs: &[Attribute], test_features: &HashSet<String>) -> bool {
    test_attr(attrs, test_features, true) && always_built(attrs)
}

/// Whether every `#[cfg]` among the attributes holds when the recommended
/// check runs (tests on, all features on), whatever the host: so no
/// `loom`, `fuzzing`, `unix`/`windows`/`target_*`, `not(..)`.
pub(super) fn always_built(attrs: &[Attribute]) -> bool {
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

/// A test attribute that runs the fn as written: last path segment `test`
/// (`#[test]`, `#[tokio::test]`), not `#[rstest]` / `#[test_case(..)]`.
pub(super) fn is_test_fn(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| last_segment(a.path()) == "test")
}

/// The `#[cfg(..)]` predicates among the attributes, as strings.
pub(super) fn cfg_attrs(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| a.meta.to_token_stream().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::testutil::{fact, facts, facts_with};

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
}
