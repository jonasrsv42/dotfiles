//! `unused`: fns and types nothing outside themselves mentions (name-based),
//! types with the impl blocks that go with them.

use super::{Category, Cx, Finding, Kind};
use crate::names::Loc;
use crate::scan::FileAnalysis;
use crate::visit::{FnKind, ImplKind, ItemKind, Vis};
use std::collections::HashMap;
use std::fmt::Write;

/// (crate, type name) -> its non-test impl blocks with their code lines.
type TypeImpls<'a> = HashMap<(Option<usize>, &'a str), Vec<(Loc, usize)>>;

/// How many types each crate defines per name, and the impl blocks that
/// go with each type, so deleting it is complete.
fn type_impls<'a>(cx: &Cx<'a>) -> (HashMap<(Option<usize>, &'a str), usize>, TypeImpls<'a>) {
    let mut defs: HashMap<(Option<usize>, &str), usize> = HashMap::new();
    for f in &cx.scan.files {
        let types = f.facts.items.iter().filter(|i| i.kind != ItemKind::Trait);
        for t in types {
            *defs.entry((f.crate_idx, t.name.as_str())).or_default() += 1;
        }
    }
    let mut type_impls: TypeImpls = HashMap::new();
    // Checked test impls go with the tests; `#[cfg(loom)] impl X` goes with X.
    let checked_test_file = |f: &&FileAnalysis| f.whole_test && !f.unchecked_test;
    for f in cx.scan.files.iter().filter(|f| !checked_test_file(f)) {
        let impls = f.facts.impls.iter().filter(|i| {
            !i.checked_test && matches!(i.kind, ImplKind::Inherent | ImplKind::Concrete)
        });
        for i in impls {
            let name = match i.trait_path.last() {
                Some(t) => format!("impl {t} for {}", i.self_ty),
                None => format!("impl {}", i.self_ty),
            };
            let at = Loc::new(cx.scan, f, &name, (i.start_line, i.line, i.end_line));
            type_impls
                .entry((f.crate_idx, i.self_name.as_str()))
                .or_default()
                .push((at, f.code_lines(i.line, i.end_line)));
        }
    }
    (defs, type_impls)
}

pub(super) fn unused(cx: &mut Cx, out: &mut Vec<Finding>) {
    // When a crate defines the name more than once, only same-file impls
    // are surely this type's.
    let (defs, type_impls) = type_impls(cx);
    for f in cx.focus() {
        if f.whole_test {
            continue;
        }
        let fns = f.facts.fns.iter().filter(|func| {
            matches!(func.kind, FnKind::Free | FnKind::Inherent)
                && !func.in_test
                && !func.exempt
                && !func.ident.starts_with('_')
        });
        for func in fns {
            let c = Candidate {
                ident: &func.ident,
                at: Loc::of_fn(cx.scan, f, func),
                self_refs: func.self_refs,
                test_self_refs: 0,
                vis: func.vis,
                lines: f.own_lines(func),
                impls: None,
            };
            push_unused(cx, out, f, c);
        }
        for item in f.facts.items.iter().filter(|i| !i.in_test && !i.exempt) {
            let key = (f.crate_idx, item.name.as_str());
            let ambiguous = defs.get(&key).copied().unwrap_or(0) > 1;
            let impls = (item.kind != ItemKind::Trait).then(|| {
                let all = type_impls.get(&key).cloned().unwrap_or_default();
                all.into_iter()
                    .filter(|(l, _)| !ambiguous || l.file == f.rel)
                    .collect()
            });
            let self_refs =
                |m: &HashMap<&str, u32>| m.get(item.name.as_str()).copied().unwrap_or(0);
            let c = Candidate {
                ident: &item.name,
                at: Loc::new(
                    cx.scan,
                    f,
                    &item.name,
                    (item.start_line, item.line, item.end_line),
                ),
                self_refs: self_refs(&cx.type_self_refs),
                test_self_refs: self_refs(&cx.test_type_self_refs),
                vis: item.vis,
                lines: f.code_lines(item.line, item.end_line),
                impls,
            };
            push_unused(cx, out, f, c);
        }
    }
}

/// An item that may be unused.
struct Candidate<'a> {
    /// Bare name, for reference counts.
    ident: &'a str,
    at: Loc,
    /// Mentions of its own name inside it (outside / inside test code),
    /// not counted as uses.
    self_refs: u32,
    test_self_refs: u32,
    vis: Vis,
    lines: usize,
    /// For types: their impl blocks with code lines.
    impls: Option<Vec<(Loc, usize)>>,
}

fn push_unused(cx: &mut Cx, out: &mut Vec<Finding>, f: &FileAnalysis, c: Candidate) {
    let (r, t) = cx.refs(c.ident);
    let (r, t) = (
        r.saturating_sub(c.self_refs),
        t.saturating_sub(c.test_self_refs),
    );
    if r > 0 || cx.skip_pub(Kind::Unused, f, c.vis) {
        return;
    }
    let uncertain = f.crate_idx.is_some_and(|i| cx.uncertain.contains(&i));
    let mut detail = if t > 0 {
        format!("only referenced from tests ({t})")
    } else {
        "no references".to_string()
    };
    let impl_lines: usize = c.impls.iter().flatten().map(|(_, n)| n).sum();
    if let Some(impls) = c.impls.as_ref().filter(|i| !i.is_empty()) {
        let _ = write!(
            detail,
            "; +{} impl blocks ({impl_lines} lines)",
            impls.len()
        );
    }
    if c.vis == Vis::Public && cx.open_api(f) {
        detail.push_str("; pub in a library, so maybe used outside the repository");
    }
    if uncertain {
        detail.push_str("; crate has code bloaty can't see, so confirm by compiling");
    }
    if f.orphan {
        let stem = f.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let named = f
            .crate_idx
            .is_some_and(|c| cx.crate_modules.contains(&(c, stem)));
        detail.push_str(if named {
            "; the file may not be compiled (bloaty couldn't follow its `mod`)"
        } else {
            "; the file isn't compiled (no `mod` declares it)"
        });
    }
    let lines = c.lines + impl_lines;
    out.push(
        Finding {
            vis: Some(c.vis),
            lines,
            refs: Some(r),
            test_refs: Some(t),
            impls: c.impls.map(|v| v.into_iter().map(|(l, _)| l).collect()),
            uncertain,
            detail,
            ..Finding::new(Kind::Unused, Category::Impl, c.at)
        }
        // Nothing references it before only tests do.
        .rank(u32::from(t > 0), lines),
    );
}

#[cfg(test)]
mod tests {
    use crate::findings::tests::*;

    #[test]
    fn unused_ignores_live_code_reached_indirectly() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/lib.rs",
                "mod plat;\nmod kinds;\nmod py;\nmod gen;\nmod rec;\nmod deleg;\nmod exported;\nuse crate::plat::LinuxThing as Active;\npub fn api() {\n    Active::go();\n    kinds::k();\n    rec::entry();\n    deleg::Outer.forget();\n    deleg::reunite();\n}\n",
            ),
            (
                "src/plat.rs",
                "pub(crate) struct LinuxThing;\nimpl LinuxThing {\n    pub(crate) fn go() {}\n}\npub(crate) fn dead_plat() {}\n",
            ),
            (
                "src/kinds.rs",
                "pub(crate) enum Kind { One, Two }\nuse self::Kind::*;\npub(crate) fn k() -> u8 {\n    match One { One => 1, Two => 2 }\n}\n",
            ),
            (
                "src/py.rs",
                "pub struct Counter;\n#[pymethods]\nimpl Counter {\n    fn incr(&mut self) {}\n}\n",
            ),
            (
                "src/rec.rs",
                "pub(crate) fn entry() {}\nfn dead_recursive(n: u32) -> u32 {\n    if n == 0 { 0 } else { dead_recursive(n - 1) }\n}\nfn test_only() {}\nfn port() -> u16 { 1 }\n#[derive(Deserialize)]\npub struct Cfg {\n    #[builder(default = \"port()\")]\n    p: u16,\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { super::test_only(); }\n}\n",
            ),
            (
                // Same-name delegation: each live fn's only caller is a
                // fn with the same name.
                "src/deleg.rs",
                "pub(crate) struct Outer { inner: Inner }\nstruct Inner;\nimpl Inner {\n    fn forget(&self) {}\n}\nimpl Outer {\n    pub(crate) fn forget(&self) { self.inner.forget() }\n}\nfn reunite_impl() {}\npub(crate) fn reunite() { mod_reunite::reunite() }\nmod mod_reunite {\n    pub(super) fn reunite() { super::reunite_impl() }\n}\nstruct Lexer;\nimpl Lexer {\n    fn next(&mut self) -> Option<u8> { None }\n}\nimpl Iterator for Lexer {\n    type Item = u8;\n    fn next(&mut self) -> Option<u8> { Lexer::next(self) }\n}\n",
            ),
            (
                "src/exported.rs",
                "export_c! {\n    fn plugin_scale(x: u32) -> u32 { x * 2 }\n}\n",
            ),
            (
                "build.rs",
                "fn main() { write(\"impl crate::gen::Embedded {}\"); }\n",
            ),
            (
                "src/gen.rs",
                "pub(crate) struct Embedded;\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n",
            ),
        ]);
        let all = run(&fx, false);
        let unused = names(&all, Kind::Unused);
        for live in [
            "LinuxThing",
            "LinuxThing::go",
            "Kind",
            "k",
            "Counter::incr",
            "Embedded",
            "port",
            "entry",
            "Inner::forget",
            "reunite",
            "reunite_impl",
            "Lexer::next",
            "plugin_scale",
        ] {
            assert!(
                !unused.contains(&live.to_string()),
                "{live} is live: {unused:?}"
            );
        }
        for dead in ["dead_plat", "dead_recursive", "test_only"] {
            assert!(
                unused.contains(&dead.to_string()),
                "{dead} is dead: {unused:?}"
            );
        }
        let test_only = all.all.iter().find(|f| f.at.name == "test_only").unwrap();
        // The OUT_DIR include is covered by the build script's strings.
        assert!(!test_only.uncertain);
        assert_eq!(test_only.detail, "only referenced from tests (1)");
        // A lib nothing depends on: its `pub` items are hidden by default...
        assert!(!unused.contains(&"api".to_string()));
        assert!(all.hidden_pub[&Kind::Unused] > 0);
        // ...and shown on request.
        assert!(names(&run(&fx, true), Kind::Unused).contains(&"api".to_string()));
    }

    #[test]
    fn pub_items_show_only_for_unpublished_libs() {
        let fx = Fixture::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"core\", \"app\", \"private\"]\n",
            ),
            // Publishable, and used by `app`: may still have outside users.
            ("core/Cargo.toml", "[package]\nname = \"core-lib\"\n"),
            (
                "core/src/lib.rs",
                "pub fn used() {}\npub fn maybe_api() {}\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\n[dependencies]\nmycore = { package = \"core-lib\", path = \"../core\" }\n",
            ),
            ("app/src/main.rs", "fn main() { mycore::used(); }\n"),
            (
                "private/Cargo.toml",
                "[package]\nname = \"private\"\npublish = false\n",
            ),
            ("private/src/lib.rs", "pub fn private_dead() {}\n"),
        ]);
        let all = run(&fx, false);
        assert_eq!(names(&all, Kind::Unused), vec!["private_dead"]);
        let with_pub = run(&fx, true);
        let f = with_pub
            .all
            .iter()
            .find(|f| f.at.name == "maybe_api")
            .unwrap();
        assert!(
            f.detail.contains("maybe used outside the repository"),
            "{}",
            f.detail
        );
    }

    #[test]
    fn unresolved_and_generated_modules_make_findings_uncertain() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "mod generated;\nfn main() {}\nfn maybe_used() {}\n",
            ),
        ]);
        let all = run(&fx, false);
        let f = all.all.iter().find(|f| f.at.name == "maybe_used").unwrap();
        assert!(f.uncertain);
    }

    #[test]
    fn unused_types_bring_their_impls_and_docs() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "fn main() {}\n/// A ghost.\n#[derive(Debug)]\nstruct Ghost;\nimpl Ghost {\n    fn a(&self) {}\n}\nimpl Clone for Ghost {\n    fn clone(&self) -> Self { Self }\n}\n",
            ),
        ]);
        let all = run(&fx, false);
        let ghost = all.all.iter().find(|f| f.at.name == "Ghost").unwrap();
        assert_eq!((ghost.at.start_line, ghost.at.line), (2, 4));
        assert_eq!(ghost.impls.as_ref().unwrap().len(), 2);
        assert!(ghost.detail.contains("+2 impl blocks"), "{}", ghost.detail);
    }

    #[test]
    fn self_mentioning_types_can_be_unused() {
        let all = unused_in(
            "fn main() {}\nenum State { A, B }\nimpl State {\n    fn flip(self) -> State { match self { State::A => State::B, State::B => State::A } }\n}\nstruct Dead;\nimpl Default for Dead {\n    fn default() -> Self { Dead }\n}\n",
            &[],
        );
        let unused = names(&all, Kind::Unused);
        assert!(unused.contains(&"State".to_string()), "{unused:?}");
        assert!(unused.contains(&"Dead".to_string()), "{unused:?}");
    }

    #[test]
    fn test_helpers_on_a_type_dont_hide_its_real_uses() {
        let all = unused_in(
            "fn main() { let s = Store { n: 1 }; s.get(); }\nstruct Store { n: u32 }\nimpl Store {\n    fn get(&self) -> u32 { self.n }\n}\n#[cfg(test)]\nimpl Store {\n    fn empty() -> Store { Store { n: 0 } }\n}\n",
            &[],
        );
        assert!(!names(&all, Kind::Unused).contains(&"Store".to_string()));
    }

    #[test]
    fn same_named_types_dont_share_impls() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/main.rs", "mod a;\nmod b;\nfn main() {}\n"),
            (
                "src/a.rs",
                "struct Ghost;\nimpl Ghost {\n    fn x(&self) {}\n}\n",
            ),
            (
                "src/b.rs",
                "struct Ghost;\nimpl Ghost {\n    fn y(&self) {}\n}\n",
            ),
        ]);
        let all = run(&fx, false);
        for g in all.all.iter().filter(|f| f.at.name == "Ghost") {
            let impls = g.impls.as_ref().unwrap();
            assert_eq!(impls.len(), 1);
            assert_eq!(impls[0].file, g.at.file);
        }
    }

    #[test]
    fn deep_expressions_and_includes_keep_their_uses() {
        let deep_or = (0..700)
            .map(|i| format!("x == {i}"))
            .collect::<Vec<_>>()
            .join(" || ");
        let main = format!(
            "fn main() {{ include!(\"glue.inc\"); }}\nfn big(x: u32) -> bool {{ helper_in_big(); {deep_or} }}\nfn helper_in_big() {{}}\nfn only_in_inc() {{}}\n"
        );
        let all = unused_in(&main, &[("src/glue.inc", "only_in_inc();\n")]);
        let unused = names(&all, Kind::Unused);
        for live in ["helper_in_big", "only_in_inc"] {
            assert!(!unused.contains(&live.to_string()), "{live}: {unused:?}");
        }
    }

    #[test]
    fn raw_identifier_modules_resolve() {
        let all = unused_in(
            "mod r#async;\nfn main() { r#async::r(); }\n",
            &[("src/async.rs", "pub(crate) fn r() {}\nfn dead() {}\n")],
        );
        assert_eq!(names(&all, Kind::Unused), vec!["dead"]);
        assert!(all.all.iter().all(|f| !f.uncertain));
    }

    #[test]
    fn loom_and_fuzz_code_uses_count_doctests_count_as_tests() {
        let all = unused_in(
            "fn main() {}\n/// ```\n/// documented();\n/// ```\npub(crate) fn documented() {}\nfn check_invariant() {}\n#[cfg(fuzzing)]\nfn fuzz_entry() { check_invariant(); }\n",
            &[],
        );
        let unused = names(&all, Kind::Unused);
        assert!(
            !unused.contains(&"check_invariant".to_string()),
            "{unused:?}"
        );
        let doc = all.all.iter().find(|f| f.at.name == "documented").unwrap();
        assert_eq!(doc.detail, "only referenced from tests (1)");
    }

    #[test]
    fn files_no_mod_declares_are_flagged() {
        let all = unused_in("fn main() {}\n", &[("src/stray.rs", "fn probe() {}\n")]);
        let f = all.all.iter().find(|f| f.at.name == "probe").unwrap();
        assert!(f.detail.contains("isn't compiled"), "{}", f.detail);
    }

    #[test]
    fn dev_only_users_dont_make_a_published_lib_internal() {
        let fx = Fixture::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"mylib\", \"tool\"]\n",
            ),
            (
                "mylib/Cargo.toml",
                "[package]\nname = \"mylib\"\nversion = \"0.3.0\"\n",
            ),
            ("mylib/src/lib.rs", "pub fn render() {}\n"),
            (
                "tool/Cargo.toml",
                "[package]\nname = \"tool\"\npublish = false\n[dev-dependencies]\nmylib = { path = \"../mylib\" }\n",
            ),
            ("tool/src/main.rs", "fn main() {}\n"),
        ]);
        assert!(!names(&run(&fx, false), Kind::Unused).contains(&"render".to_string()));
    }

    #[test]
    fn uses_from_code_cargo_check_skips_keep_items_alive() {
        let all = unused_in(
            "#[cfg(loom)]\nmod loom_file;\n#[cfg(all(test, loom))]\nmod t {\n    #[test]\n    fn t() { super::snapshot_b(); }\n}\n#[cfg(test)]\nmod u {\n    #[cfg(loom)]\n    #[test]\n    fn t() { super::snapshot_c(); }\n}\n#[cfg(all(test, windows))]\nmod w {\n    #[test]\n    fn t() { super::win_fixture(); }\n}\nfn main() {}\nfn snapshot_a() {}\nfn snapshot_b() {}\nfn snapshot_c() {}\nfn win_fixture() {}\npub(crate) struct Probe;\n#[cfg(loom)]\nimpl Probe {\n    fn p(&self) {}\n}\n",
            &[(
                "src/loom_file.rs",
                "#[test]\nfn t() { super::snapshot_a(); }\n",
            )],
        );
        let unused = names(&all, Kind::Unused);
        for live in ["snapshot_a", "snapshot_b", "snapshot_c", "win_fixture"] {
            assert!(!unused.contains(&live.to_string()), "{live}: {unused:?}");
        }
        // Deleting `Probe` must take its loom-only impl along.
        let probe = all.all.iter().find(|f| f.at.name == "Probe").unwrap();
        assert_eq!(probe.impls.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn inherited_publish_false_shows_pub_items() {
        let fx = Fixture::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"lib\"]\n[workspace.package]\npublish = false\n",
            ),
            (
                "lib/Cargo.toml",
                "[package]\nname = \"lib\"\npublish.workspace = true\n",
            ),
            ("lib/src/lib.rs", "pub fn dead() {}\n"),
        ]);
        assert_eq!(names(&run(&fx, false), Kind::Unused), vec!["dead"]);
    }

    #[test]
    fn cfg_if_modules_are_compiled() {
        let all = unused_in(
            "cfg_if::cfg_if! {\n    if #[cfg(unix)] {\n        mod unix;\n    } else {\n        mod windows;\n    }\n}\nfn main() {}\n",
            &[
                ("src/unix.rs", "fn stray_u() {}\n"),
                ("src/windows.rs", "fn stray_w() {}\n"),
            ],
        );
        for f in all.all.iter().filter(|f| f.kind == Kind::Unused) {
            assert!(!f.detail.contains("compiled"), "{}", f.detail);
        }
    }

    #[test]
    fn type_aliases_dont_make_names_ambiguous() {
        let all = unused_in(
            "mod a;\nmod b;\nfn main() {}\n",
            &[
                ("src/a.rs", "pub(crate) struct Ghost;\n"),
                (
                    "src/b.rs",
                    "impl crate::a::Ghost {\n    fn x(&self) {}\n}\nmod c {\n    type Ghost = u8;\n}\n",
                ),
            ],
        );
        let ghost = all.all.iter().find(|f| f.at.name == "Ghost").unwrap();
        assert_eq!(ghost.impls.as_ref().unwrap().len(), 1);
    }
}
