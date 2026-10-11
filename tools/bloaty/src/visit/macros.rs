//! Macros are opaque: bloaty doesn't expand them or guess what they expand
//! to. Two exceptions, both text as written: the arguments of the
//! well-known macros in `EXPRESSION_MACROS` are read as expressions, and
//! item-position macros whose whole body parses as Rust items are read as
//! those items (visit.rs). Every other macro's tokens only count as name
//! mentions and, in fn bodies, are scanned for explicit panic calls.

use proc_macro2::{TokenStream, TokenTree};
use std::collections::HashSet;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, Token};

/// Macros whose arguments are comma-separated expressions, by last path
/// segment (names aren't resolved: a workspace macro called `info` counts
/// as tracing's). One table: metrics read only these as code.
pub(crate) const EXPRESSION_MACROS: &[&str] = &[
    // std
    "format",
    "format_args",
    "print",
    "println",
    "eprint",
    "eprintln",
    "write",
    "writeln",
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "vec",
    "matches",
    "dbg",
    "pin",
    // log / tracing
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "event",
];

/// Whether `path!` is in `EXPRESSION_MACROS`.
pub(crate) fn is_expression_macro(path: &syn::Path) -> bool {
    path.segments
        .last()
        .is_some_and(|s| EXPRESSION_MACROS.iter().any(|m| s.ident == m))
}

/// The macro's arguments as comma-separated expressions, if it is in
/// `EXPRESSION_MACROS` and they parse; `None` means opaque (a workspace
/// `info!(B => Codec)` sharing tracing's name is opaque too).
pub(crate) fn expression_args(m: &syn::Macro) -> Option<Punctuated<Expr, Token![,]>> {
    if !is_expression_macro(&m.path) {
        return None;
    }
    Punctuated::<Expr, Token![,]>::parse_terminated
        .parse2(m.tokens.clone())
        .ok()
}

/// Every identifier in the tokens.
pub(super) fn token_words(tokens: TokenStream, out: &mut HashSet<String>) {
    for tt in tokens {
        match tt {
            TokenTree::Ident(i) => {
                out.insert(i.to_string());
            }
            TokenTree::Group(g) => token_words(g.stream(), out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{fact, facts};

    #[test]
    fn macros_declare_no_modules() {
        // `cfg_if!` bodies aren't Rust items: their `mod x;`s stay unseen
        // (the files become orphans, see scan.rs), but the words count.
        let f = facts(
            "cfg_if::cfg_if! {\n    if #[cfg(unix)] {\n        mod unix;\n    } else {\n        #[path = \"win.rs\"]\n        mod windows;\n    }\n}",
        );
        assert!(f.mod_decls.is_empty());
        assert!(f.opaque_mods);
        assert!(f.macro_words.contains("unix") && f.macro_words.contains("windows"));
        // Item macros that parse as items are read as written.
        let f = facts("cfg_rt! { pub mod runtime; }");
        assert_eq!(f.mod_decls.len(), 1);
        assert!(!f.opaque_mods);
    }

    #[test]
    fn nested_items_and_macro_words() {
        let f = facts(
            "fn outer() {\n    let a = 1;\n    fn inner() {\n        let b = 2;\n    }\n}\nmacro_rules! m { ($t:ty) => { impl $crate::Tr<u8> for $t {} }; }\nfn g() { quote!(impl Codec for X); println!(\"{}\", Shown); }",
        );
        assert_eq!(fact(&f, "outer").nested, vec![(3, 5)]);
        for w in ["Tr", "Codec"] {
            assert!(f.macro_words.contains(w), "{w}");
        }
        // Expression macros are read as code, not as opaque tokens; a
        // macro sharing an allow-listed name but not taking expressions is
        // opaque.
        assert!(!f.macro_words.contains("Shown"));
        let f = facts("fn g() { info!(B => Codec); }");
        assert!(f.macro_words.contains("Codec"));
    }
}
