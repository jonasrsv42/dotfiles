//! Items inside macro invocations bloaty only sees as tokens: `mod x;`
//! declarations (`cfg_if!`) and trait impls in `macro_rules!` bodies.

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::ext::IdentExt;

/// `[#[cfg(test)]] [#[path = ".."]] [pub] mod name;` in macro tokens, as
/// (name, path, under `#[cfg(test)]`). A `#[cfg(test)]` before a `{ .. }`
/// group (`cfg_if!`'s `if #[cfg(test)] { .. }`) covers what's inside.
pub(super) fn macro_mod_decls(
    tokens: TokenStream,
    test: bool,
    out: &mut Vec<(String, Option<String>, bool)>,
) {
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
pub(super) fn macro_trait_impls(tokens: TokenStream) -> Vec<Vec<String>> {
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

#[cfg(test)]
mod tests {
    use crate::testutil::{fact, facts};

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
}
