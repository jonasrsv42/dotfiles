//! `duplicate`: the same code more than once. A claim of exact, syntactic
//! sameness: identical bodies (or, for long ones, the same shape) with the
//! same written signature, each copy with some logic of its own. Equal
//! bodies under different signatures, or of different shape-only cfg
//! variants, are reported as notes. Names aren't resolved (a `CODES` or
//! `helper` meaning different items in two files still matches), and
//! whether a copy could call another (visibility, re-exports, types) is
//! never judged: that needs rust-analyzer.
//!
//! Each group is reported once with all its copies and the facts about
//! them (crate, test code, cfgs, signature); `policy` picks its tier and
//! hint from those facts.

mod policy;

pub use policy::{Rule, Tier};

use super::{Criterion, Cx, Finding, Kind, Reason, category};
use crate::names::{Loc, short_name};
use crate::scan::FileAnalysis;
use crate::visit::sig::{Part, Signature};
use crate::visit::{BodyHash, FnFact, FnKind};
use policy::Facts;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// The least logic (see `visit::body::logic`) a body needs to count as a
/// duplicate: forwarding bodies and `{}` / `{ x }` / `Default::default()`
/// have none.
pub(crate) const MIN_LOGIC: u32 = 1;

/// A duplicate's facts (flattened into its finding's JSON).
#[derive(Serialize, Debug)]
pub struct Duplicate {
    pub tier: Tier,
    pub rule: Rule,
    /// Identical bodies, or only the same shape.
    pub identical: bool,
    /// Code lines removed by keeping one copy.
    pub lines_saved: usize,
    /// Signature parts that differ between copies (`Signatures` notes).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differs: Vec<Part>,
    /// The crates its copies are in, sorted.
    pub crates: Vec<String>,
    /// `[a, b]`: crate a depends on crate b directly (per Cargo.toml).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<[String; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
    /// Every copy, the finding's own first.
    pub copies: Vec<Copy>,
}

/// One copy of a duplicate and where it is built.
#[derive(Serialize, Debug)]
pub struct Copy {
    #[serde(flatten)]
    pub at: Loc,
    pub test: bool,
    /// Its non-test cfgs (its file's `mod` declarations', then its own), as
    /// written.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cfgs: Vec<String>,
    pub signature: Signature,
    /// Its signature class within the group (0 for the first copy's
    /// signature, 1 for the next different one, ...).
    pub class: usize,
}

/// A fn compared for duplicates.
struct Member<'a> {
    file: &'a FileAnalysis,
    func: &'a FnFact,
    /// Own code lines, signature included: the text size.
    lines: usize,
    /// Own code lines after the signature: what a shared helper would hold.
    substance: usize,
    test: bool,
    cfgs: Vec<String>,
}

impl Member<'_> {
    fn hash(&self) -> BodyHash {
        self.func.body_hash.expect("members have body hashes")
    }

    /// Identical free fns and inherent methods of impl code count however
    /// short (`fn add(a, b) { a + b }`).
    fn short_candidate(&self) -> bool {
        let callable = matches!(self.func.kind, FnKind::Free | FnKind::Inherent);
        callable && !self.test && self.func.logic >= MIN_LOGIC
    }

    /// Enough of a body to be a duplicate: `min_dup` lines after the
    /// signature and some logic, or a short candidate's identical copy.
    fn substantive(&self, identical: bool, min_dup: usize) -> bool {
        let any_size = identical && self.short_candidate();
        self.func.logic >= MIN_LOGIC && (self.substance >= min_dup || any_size)
    }

    /// Code lines shown and saved: the body after the signature (a one-line
    /// fn counts 1).
    fn shown(&self) -> usize {
        self.substance.max(1)
    }
}

pub(super) fn duplicates(cx: &mut Cx, out: &mut Vec<Finding>) {
    let min_dup = cx.opts.min_dup;
    let members: Vec<Member> = cx
        .scan
        .files
        .iter()
        .flat_map(|f| f.facts.fns.iter().map(move |func| (f, func)))
        .filter(|(_, func)| func.body_hash.is_some())
        .map(|(file, func)| {
            let lines = file.own_lines(func);
            let signature = file.code_lines(func.line, func.body_line.min(func.end_line));
            Member {
                file,
                func,
                lines,
                substance: lines.saturating_sub(signature),
                test: func.in_test || file.whole_test,
                cfgs: file.cfgs.iter().chain(&func.cfgs).cloned().collect(),
            }
        })
        // Long enough by text, or possibly by substance.
        .filter(|m| m.lines >= min_dup || m.short_candidate())
        .collect();
    for (group, identical) in duplicate_groups(&members, min_dup) {
        let mut ms: Vec<&Member> = group.iter().map(|&i| &members[i]).collect();
        // `duplicate_groups` iterates a HashMap: report in a fixed order.
        ms.sort_by_key(|m| (&m.file.rel, m.func.line));
        // Focus scopes the report: groups outside the requested paths are
        // neither reported nor counted.
        if !ms.iter().any(|m| m.file.in_focus) {
            continue;
        }
        // A copy in a file a macro may declare is test or not by path only:
        // the group's tier and category would rest on that.
        if ms.iter().any(|m| m.file.macro_declared) {
            cx.exclude(Kind::Duplicate, Reason::NamedInMacro);
            continue;
        }
        // A duplicate by text size alone (signatures counted, any body).
        let by_text = ms.iter().filter(|m| m.lines >= min_dup).count() > 1;
        ms.retain(|m| m.substantive(identical, min_dup));
        if ms.len() < 2 {
            if by_text {
                *cx.excluded.entry(Criterion::Substance).or_default() += 1;
            }
            continue;
        }
        for ms in by_signature(ms) {
            if ms.iter().any(|m| m.file.in_focus) {
                out.push(duplicate_finding(cx, ms, identical));
            }
        }
    }
}

/// Groups of member indices with identical bodies (`true`), then groups with
/// the same shape (`false`). Same shape only counts for longer bodies (short
/// ones match by accident: every builder setter looks alike) and not for
/// bodies differing only in literals: those are data tables (`match` arms
/// mapping to different strings, queries with different SQL).
fn duplicate_groups(members: &[Member], min_dup: usize) -> Vec<(Vec<usize>, bool)> {
    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, m) in members.iter().enumerate() {
        exact.entry(m.hash().exact).or_default().push(i);
    }
    let mut groups: Vec<(Vec<usize>, bool)> = Vec::new();
    let mut grouped = HashSet::new();
    for g in exact.into_values().filter(|g| g.len() > 1) {
        grouped.extend(g.iter().copied());
        groups.push((g, true));
    }
    let mut shape: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, m) in members.iter().enumerate() {
        if !grouped.contains(&i) && m.lines >= min_dup.max(15) {
            shape.entry(m.hash().shape).or_default().push(i);
        }
    }
    let names_differ = |g: &[usize]| {
        g.iter()
            .any(|&i| members[i].hash().names != members[g[0]].hash().names)
    };
    groups.extend(
        shape
            .into_values()
            .filter(|g| g.len() > 1 && names_differ(g))
            .map(|g| (g, false)),
    );
    groups
}

/// Copies of one body to report: those sharing a signature (the same code),
/// and, if the signatures aren't all equal, the whole group once more (a
/// note naming what differs), so no copy goes unmentioned.
fn by_signature<'m, 'a>(ms: Vec<&'m Member<'a>>) -> Vec<Vec<&'m Member<'a>>> {
    let mut same: Vec<Vec<&Member>> = Vec::new();
    for m in &ms {
        match same
            .iter_mut()
            .find(|c| c[0].func.signature == m.func.signature)
        {
            Some(c) => c.push(m),
            None => same.push(vec![m]),
        }
    }
    let differ = same.len() > 1;
    let mut out: Vec<_> = same.into_iter().filter(|c| c.len() > 1).collect();
    if differ {
        out.push(ms);
    }
    out
}

fn duplicate_finding(cx: &Cx, mut ms: Vec<&Member>, identical: bool) -> Finding {
    // Lead with a copy inside the requested paths.
    ms.sort_by_key(|m| (!m.file.in_focus, &m.file.rel, m.func.line));
    let crate_name = |m: &Member| cx.scan.crate_name(m.file).to_string();
    let crates: BTreeSet<String> = ms.iter().map(|m| crate_name(m)).collect();
    let cfg_sets: HashSet<BTreeSet<&String>> = ms.iter().map(|m| m.cfgs.iter().collect()).collect();
    let facts = Facts {
        identical,
        differs: Signature::differs(ms.iter().map(|m| &m.func.signature)),
        // Cfgs compare only within a crate: across crates they're other
        // crates' features.
        cfgs_differ: cfg_sets.len() > 1 && crates.len() == 1,
        crates: crates.len(),
    };
    let rule = Rule::of(&facts);
    let hint = if facts.differs.is_empty() {
        rule.hint()
    } else {
        facts.differs.iter().find_map(|&p| policy::part_hint(p))
    };
    let depends = depends(cx, &ms);
    let max = ms.iter().map(|m| m.shown()).max().unwrap_or(0);
    let saved = ms.iter().map(|m| m.shown()).sum::<usize>() - max;
    let classes = classes(&ms);
    let detail = detail(&ms, &classes, rule, &facts, &crates, &depends, hint, saved);
    let first = ms[0];
    let dup = Duplicate {
        tier: rule.tier(),
        rule,
        identical,
        lines_saved: saved,
        differs: facts.differs,
        crates: crates.into_iter().collect(),
        depends,
        hint,
        copies: ms
            .iter()
            .zip(classes)
            .map(|(m, class)| Copy {
                at: Loc::of_fn(cx.scan, m.file, m.func),
                test: m.test,
                cfgs: m.cfgs.clone(),
                signature: m.func.signature.clone(),
                class,
            })
            .collect(),
    };
    let note = dup.tier == Tier::Note;
    Finding {
        lines: max,
        dup: Some(dup),
        detail,
        ..Finding::new(
            Kind::Duplicate,
            category(ms.iter().all(|m| m.test)),
            Loc::of_fn(cx.scan, first.file, first.func),
        )
    }
    // Notes last; identical groups first: same-shape ones are harder to merge.
    .rank(u32::from(note) * 2 + u32::from(!identical), saved)
}

/// Each copy's signature class: the index of its signature among the
/// group's distinct ones, in order of appearance.
fn classes(ms: &[&Member]) -> Vec<usize> {
    let mut seen: Vec<&Signature> = Vec::new();
    ms.iter()
        .map(|m| {
            let s = &m.func.signature;
            seen.iter().position(|&x| x == s).unwrap_or_else(|| {
                seen.push(s);
                seen.len() - 1
            })
        })
        .collect()
}

/// `[a, b]` for each pair of the copies' crates where a depends on b.
fn depends(cx: &Cx, ms: &[&Member]) -> Vec<[String; 2]> {
    let idx: BTreeSet<usize> = ms.iter().filter_map(|m| m.file.crate_idx).collect();
    let name = |c: usize| cx.scan.crates[c].name.clone();
    idx.iter()
        .flat_map(|&a| {
            cx.scan.crates[a]
                .links
                .iter()
                .filter(|b| idx.contains(b))
                .map(move |&b| [name(a), name(b)])
        })
        .collect()
}

/// `2 identical fns; in a, b; a depends on b; <hint>; up to ~10 lines
/// saved`; notes name the copies per signature (`A::f, A::g | B::f`).
#[allow(clippy::too_many_arguments)]
fn detail(
    ms: &[&Member],
    classes: &[usize],
    rule: Rule,
    facts: &Facts,
    crates: &BTreeSet<String>,
    depends: &[[String; 2]],
    hint: Option<&str>,
    saved: usize,
) -> String {
    let mut parts = vec![format!("{} {}", ms.len(), rule.text(facts.identical))];
    if !facts.differs.is_empty() {
        let differs: Vec<_> = facts.differs.iter().map(|p| p.text()).collect();
        let n = classes.iter().max().map_or(0, |m| m + 1);
        let by_class: Vec<String> = (0..n)
            .map(|c| {
                let names = ms.iter().zip(classes).filter(|(_, k)| **k == c);
                let names: Vec<_> = names.map(|(m, _)| short_name(&m.func.name)).collect();
                names.join(", ")
            })
            .collect();
        parts[0].push_str(&format!(
            " (differing: {}): {}",
            differs.join(", "),
            by_class.join(" | ")
        ));
    }
    if crates.len() > 1 {
        let crates: Vec<_> = crates.iter().map(String::as_str).collect();
        parts.push(format!("in {}", crates.join(", ")));
    }
    parts.extend(depends.iter().map(|[a, b]| format!("{a} depends on {b}")));
    parts.extend(hint.map(str::to_string));
    if rule.tier() == Tier::Finding {
        parts.push(format!("up to ~{saved} lines saved"));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::{Rule, Tier};
    use crate::findings::tests::*;
    use crate::findings::{Criterion, Finding};

    const PKG: &str = "[package]\nname = \"m\"\n";

    fn dups(files: &[(&str, &str)]) -> (Vec<Finding>, Findings) {
        let mut all = run(&Fixture::new(files), true);
        let dups = std::mem::take(&mut all.all)
            .into_iter()
            .filter(|f| f.kind == Kind::Duplicate)
            .collect();
        (dups, all)
    }

    /// (copy names, rule) per duplicate.
    fn summary(dups: &[Finding]) -> Vec<(Vec<&str>, Rule)> {
        dups.iter()
            .map(|f| {
                let d = f.dup.as_ref().unwrap();
                let names = d.copies.iter().map(|c| c.at.name.as_str()).collect();
                (names, d.rule)
            })
            .collect()
    }

    /// A 5-line body (min_dup is 4 in tests) using `v`, `w`, `z`.
    fn body(lit: &str, v: &str) -> String {
        format!(
            "{{\n    let {v} = x + {lit};\n    let w = {v} * 2;\n    let z = w - {v};\n    z + {v}\n}}\n"
        )
    }

    #[test]
    fn duplicates_skip_data_tables() {
        let table = |name: &str, a: &str, b: &str| {
            let arms: String = (0..16)
                .map(|i| format!("        {i} => \"{a}{i}{b}\",\n"))
                .collect();
            format!(
                "fn {name}(k: u8) -> &'static str {{\n    match k {{\n{arms}        _ => \"\",\n    }}\n}}\n"
            )
        };
        let src = format!(
            "fn a(x: u32) -> u32 {}fn b(x: u32) -> u32 {}{}{}",
            body("1", "v"),
            body("1", "v"),
            table("name", "n", ""),
            table("as_stored", "s", "_x"),
        );
        let (dups, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", &src)]);
        assert_eq!(summary(&dups), [(vec!["a", "b"], Rule::Identical)]);
        let d = dups[0].dup.as_ref().unwrap();
        assert_eq!((d.lines_saved, d.tier), (5, Tier::Finding));
    }

    /// Platform variants that differ in names are notes; identical ones are
    /// still the same code twice.
    #[test]
    fn cfg_variants() {
        let long = |v: &str| {
            let lines: String = (0..14)
                .map(|i| format!("    let {v}{i} = x + {i};\n"))
                .collect();
            format!("{{\n{lines}    {v}0\n}}\n")
        };
        let src = format!(
            "#[cfg(unix)]\nmod sys {{\n    fn join(x: u32) -> u32 {}    fn sep(x: u32) -> u32 {}}}\n#[cfg(windows)]\nmod sys {{\n    fn join(x: u32) -> u32 {}    fn sep(x: u32) -> u32 {}}}\n",
            long("a"),
            body("2", "v"),
            long("b"),
            body("2", "v"),
        );
        let (dups, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", &src)]);
        let mut got = summary(&dups);
        got.sort_by_key(|(n, _)| n[0]);
        assert_eq!(
            got,
            [
                (vec!["join", "join"], Rule::CfgVariants),
                (vec!["sep", "sep"], Rule::CfgIdentical),
            ]
        );
        let mut tiers: Vec<_> = dups
            .iter()
            .map(|f| (f.at.name.as_str(), f.dup.as_ref().unwrap().tier))
            .collect();
        tiers.sort_by_key(|t| t.0);
        assert_eq!(tiers, [("join", Tier::Note), ("sep", Tier::Finding)]);
    }

    /// Cfgs compare only within a crate; across crates the copies are
    /// compared as if ungated, and a same-shape pair says so.
    #[test]
    fn cfgs_and_shapes_across_crates() {
        let long = |name: &str, v: &str| {
            let lines: String = (0..14)
                .map(|i| format!("    let {v}{i} = x + {i};\n"))
                .collect();
            format!("fn {name}(x: u32) -> u32 {{\n{lines}    {v}0\n}}\n")
        };
        let ws = "[workspace]\nmembers = [\"a\", \"b\"]\n";
        let a = format!(
            "#[cfg(unix)]\nfn f(x: u32) -> u32 {}{}",
            body("1", "v"),
            long("j", "a")
        );
        let b = format!(
            "#[cfg(windows)]\nfn g(x: u32) -> u32 {}{}",
            body("1", "v"),
            long("k", "b")
        );
        let (dups, _) = dups(&[
            ("Cargo.toml", ws),
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("a/src/lib.rs", &a),
            ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            ("b/src/lib.rs", &b),
        ]);
        let mut got = summary(&dups);
        got.sort_by_key(|(n, _)| n[0]);
        assert_eq!(
            got,
            [
                (vec!["f", "g"], Rule::Crates),
                (vec!["j", "k"], Rule::Crates)
            ]
        );
        let shape = dups.iter().find(|f| f.at.name == "j").unwrap();
        assert!(
            shape.detail.starts_with("2 fns with the same shape"),
            "{}",
            shape.detail
        );
    }

    #[test]
    fn short_bodies_count_by_logic() {
        let src = "struct A;\nstruct B;\ntrait T { fn t(&self, a: u32, b: u32) -> u32; }\n\
            fn add(a: u32, b: u32) -> u32 { a + b }\n\
            fn plus(a: u32, b: u32) -> u32 { a + b }\n\
            impl T for A { fn t(&self, a: u32, b: u32) -> u32 { a * b } }\n\
            impl T for B { fn t(&self, a: u32, b: u32) -> u32 { a * b } }\n\
            fn d1() -> Vec<u8> { Default::default() }\n\
            fn d2() -> Vec<u8> { Default::default() }\n\
            fn i1(x: u32) -> u32 { x }\n\
            fn i2(x: u32) -> u32 { x }\n";
        let (dups, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", src)]);
        assert_eq!(summary(&dups), [(vec!["add", "plus"], Rule::Identical)]);
        assert_eq!(dups[0].dup.as_ref().unwrap().lines_saved, 1);
    }

    /// A long signature around a body that only forwards to `self` (areamy's
    /// `Get::get(&self.input)`) is no duplicate.
    #[test]
    fn forwarding_bodies_are_no_duplicates() {
        let get = |ty: &str| {
            format!(
                "impl Get for {ty} {{\n    fn get(\n        &self,\n        key: &str,\n        default: u32,\n    ) -> Result<u32, Error> {{\n        Get::get(&self.input, key, default)\n    }}\n}}\n"
            )
        };
        let src = format!("{}{}", get("A"), get("B"));
        let (dups, all) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", &src)]);
        assert!(dups.is_empty(), "{dups:#?}");
        assert_eq!(all.excluded.get(&Criterion::Substance), Some(&1));
    }

    /// Test code repeating a real fn: one group with all copies, impl since
    /// one copy is.
    #[test]
    fn test_copies_of_real_fns() {
        let src = format!(
            "pub fn real(x: u32) -> u32 {}#[cfg(test)]\nmod tests {{\n    fn copy(x: u32) -> u32 {}    fn again(x: u32) -> u32 {}}}\n",
            body("1", "v"),
            body("1", "v"),
            body("1", "v"),
        );
        let (dups, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", &src)]);
        assert_eq!(
            summary(&dups),
            [(vec!["real", "copy", "again"], Rule::Identical)]
        );
        assert_eq!(dups[0].category, Category::Impl);
        let tests: Vec<_> = dups[0]
            .dup
            .as_ref()
            .unwrap()
            .copies
            .iter()
            .map(|c| c.test)
            .collect();
        assert_eq!(tests, [false, true, true]);
        assert_eq!(dups[0].dup.as_ref().unwrap().lines_saved, 10);
    }

    /// Copies across crates are one finding stating the dependency, never
    /// that one could call the other.
    #[test]
    fn copies_across_crates() {
        let f = |name: &str| format!("pub fn {name}(x: u32) -> u32 {}", body("1", "v"));
        let ws = "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n";
        let (dups, _) = dups(&[
            ("Cargo.toml", ws),
            ("a/Cargo.toml", "[package]\nname = \"a\"\npublish = false\n"),
            ("a/src/lib.rs", &f("fa")),
            ("b/Cargo.toml", "[package]\nname = \"b\"\npublish = false\n"),
            ("b/src/lib.rs", &f("fb")),
            (
                "c/Cargo.toml",
                "[package]\nname = \"c\"\npublish = false\n[dev-dependencies]\na = { path = \"../a\" }\n",
            ),
            ("c/src/lib.rs", &format!("{}{}", f("fc"), f("fc2"))),
        ]);
        assert_eq!(
            summary(&dups),
            [(vec!["fa", "fb", "fc", "fc2"], Rule::Crates)]
        );
        let d = dups[0].dup.as_ref().unwrap();
        assert_eq!(d.crates, ["a", "b", "c"]);
        assert_eq!(d.depends, [["c".to_string(), "a".to_string()]]);
        assert!(
            dups[0]
                .detail
                .starts_with("4 identical fns; in a, b, c; c depends on a; "),
            "{}",
            dups[0].detail
        );
    }

    /// Short identical methods: the same code on one type; a note on two.
    #[test]
    fn short_methods_on_other_types_are_notes() {
        let src = "struct A { n: u32 }\nstruct B { n: u32 }\n\
            impl A { fn is_empty(&self) -> bool { self.n == 0 } }\n\
            impl B { fn is_empty(&self) -> bool { self.n == 0 } }\n\
            impl A { fn none(&self) -> bool { self.n == 0 } }\n";
        let (dups, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", src)]);
        assert_eq!(
            summary(&dups),
            [
                (vec!["A::is_empty", "A::none"], Rule::Identical),
                (
                    vec!["A::is_empty", "B::is_empty", "A::none"],
                    Rule::Signatures
                ),
            ]
        );
        assert!(
            dups[1]
                .detail
                .contains("(differing: Self type): A::is_empty, A::none | B::is_empty"),
            "{}",
            dups[1].detail
        );
    }

    /// An owned/borrowed pair: receiver and return type differ.
    #[test]
    fn owned_and_borrowed_is_a_note() {
        let src = "enum M { D(Vec<u8>), E }\nimpl M {\n\
            fn data(self) -> Vec<u8> {\n        match self {\n            M::D(d) => d,\n            M::E => panic!(),\n        }\n    }\n\
            fn as_data(&self) -> &Vec<u8> {\n        match self {\n            M::D(d) => d,\n            M::E => panic!(),\n        }\n    }\n}\n";
        let (dups, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", src)]);
        assert_eq!(
            summary(&dups),
            [(vec!["M::data", "M::as_data"], Rule::Signatures)]
        );
        let d = dups[0].dup.as_ref().unwrap();
        assert_eq!(d.tier, Tier::Note);
        assert!(d.hint.unwrap().contains("owned/borrowed"), "{d:?}");
    }

    #[test]
    fn same_type_different_traits() {
        let body = "{\n        let a = 1;\n        let b = a + 2;\n        let c = b * 3;\n        write!(f, \"{}\", c)\n    }\n";
        let src = format!(
            "use std::fmt;\nstruct A;\nimpl fmt::Display for A {{\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {body}}}\nimpl fmt::Debug for A {{\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {body}}}\nfn main() {{}}\n"
        );
        let all = unused_in(&src, &[]);
        let dup = all.all.iter().find(|f| f.kind == Kind::Duplicate).unwrap();
        assert!(dup.detail.contains("(differing: trait)"), "{}", dup.detail);
        assert!(dup.detail.contains("delegating"), "{}", dup.detail);
    }
}
