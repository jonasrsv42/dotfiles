//! `duplicate`: the same code more than once. A claim of exact, syntactic
//! sameness: identical bodies (or, for long ones, the same shape) with the
//! same written signature, each copy with some logic of its own. Equal
//! bodies under different signatures, shape-only cfg variants, same-shape
//! groups of test code with at most one impl copy, and short test-only
//! groups with one copy per crate are reported as notes. Names aren't resolved (a `CODES` or
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
use std::path::Path;

/// The least logic (see `visit::body::logic`) a body needs to count as a
/// duplicate: forwarding bodies and `{}` / `{ x }` / `Default::default()`
/// have none.
pub(crate) const MIN_LOGIC: u32 = 1;

/// Lines after the signature an identical free fn or inherent method of
/// impl code needs below --min-dup: shorter ones (`would_block`, a
/// one-line sum) are idioms, not helpers worth sharing (audit: every
/// marked one under 7 was a no, every yes at least 7).
pub(crate) const MIN_CALLABLE_DUP: usize = 7;

/// Own code lines (signature included) a body needs to match by shape;
/// shorter ones match by accident (every builder setter looks alike).
/// A test-only group with one copy per crate needs as many lines after
/// the signature to be a finding: shorter, they're small helpers each
/// crate's tests keep (bumbum's keypairs, `body_json`).
pub(crate) const MIN_SHAPE_LINES: usize = 15;

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
    /// Code lines after the signature (at least 1), as the finding's `lines`.
    pub lines: usize,
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

    /// Identical free fns and inherent methods of impl code count from
    /// `MIN_CALLABLE_DUP` lines after the signature, even below `min_dup`.
    fn short_candidate(&self) -> bool {
        let callable = matches!(self.func.kind, FnKind::Free | FnKind::Inherent);
        callable && !self.test && self.func.logic >= MIN_LOGIC && self.substance >= MIN_CALLABLE_DUP
    }

    /// Enough of a body to be a duplicate: `min_dup` lines after the
    /// signature and some logic, or a short candidate's identical copy.
    fn substantive(&self, identical: bool, min_dup: usize) -> bool {
        let short = identical && self.short_candidate();
        self.func.logic >= MIN_LOGIC && (self.substance >= min_dup || short)
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
/// the same shape (`false`). Same shape only counts for longer bodies
/// (`MIN_SHAPE_LINES`) and not for
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
        if !grouped.contains(&i) && m.lines >= min_dup.max(MIN_SHAPE_LINES) {
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
        copies: ms.len(),
        impl_copies: ms.iter().filter(|m| !m.test).count(),
        crowded: {
            let idx: HashSet<Option<usize>> = ms.iter().map(|m| m.file.crate_idx).collect();
            idx.len() < ms.len()
        },
        lines: ms.iter().map(|m| m.shown()).min().unwrap_or(0),
        test_targets: ms
            .iter()
            .map(|m| (m.file.crate_idx, test_target(cx, m.file)))
            .collect::<HashSet<_>>()
            .len(),
    };
    let rule = Rule::of(&facts);
    let test = facts.test();
    let hint = if facts.differs.is_empty() {
        rule.hint(&facts)
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
                lines: m.shown(),
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
            category(test),
            Loc::of_fn(cx.scan, first.file, first.func),
        )
    }
    // Notes last; identical groups first: same-shape ones are harder to merge.
    .rank(u32::from(note) * 2 + u32::from(!identical), saved)
}

/// The test target a file is in, by path: `tests/x` or `benches/x` for
/// files under its crate's `tests/x.rs`, `tests/x/` (and benches), else
/// `src` (the lib's or a bin's `cfg(test)` code). A hint's fact only:
/// `tests/common/mod.rs` reads as its own target.
fn test_target(cx: &Cx, file: &FileAnalysis) -> String {
    let dir = file.crate_idx.map(|c| &cx.scan.crates[c].dir);
    let rel = dir.and_then(|d| file.path.strip_prefix(d).ok());
    let mut parts = rel.into_iter().flat_map(|r| r.components());
    match (parts.next(), parts.next()) {
        (Some(top), Some(entry)) if top.as_os_str() == "tests" || top.as_os_str() == "benches" => {
            let entry = Path::new(entry.as_os_str());
            let stem = entry.file_stem().unwrap_or(entry.as_os_str());
            format!(
                "{}/{}",
                top.as_os_str().to_string_lossy(),
                stem.to_string_lossy()
            )
        }
        _ => "src".to_string(),
    }
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

    /// One-line bodies are under --min-dup and `MIN_CALLABLE_DUP` alike.
    #[test]
    fn one_line_bodies_are_no_duplicates() {
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
        assert!(dups.is_empty(), "{dups:#?}");
    }

    /// Below --min-dup, identical free fns and inherent methods count from
    /// `MIN_CALLABLE_DUP` lines after the signature, if they have logic;
    /// trait methods and test code don't.
    #[test]
    fn short_callables_need_min_callable_dup_lines() {
        // `n` statements and the tail: `n + 2` lines after the signature
        // (the closing brace counts); `k` keeps pairs apart.
        let f = |sig: &str, n: usize, k: usize| {
            let lines: String = (0..n)
                .map(|i| format!("    let x = x + {k}{i};\n"))
                .collect();
            format!("{sig} {{\n{lines}    x\n}}\n")
        };
        assert_eq!(super::MIN_CALLABLE_DUP, 7);
        // 8 lines after the signature each, no logic: forwarding, defaults.
        let fw = |name: &str| {
            format!(
                "fn {name}(a: u32, b: u32, c: u32, d: u32, e: u32) -> u32 {{\n    target(\n        a,\n        b,\n        c,\n        d,\n        e,\n    )\n}}\n"
            )
        };
        let df = |name: &str| {
            let fields: String = "abcde"
                .chars()
                .map(|c| format!("        {c}: Default::default(),\n"))
                .collect();
            format!("fn {name}() -> Cfg {{\n    Cfg {{\n{fields}    }}\n}}\n")
        };
        let no_logic = [fw("fw1"), fw("fw2"), df("df1"), df("df2")].concat();
        let src = format!(
            "struct S;\ntrait T {{ fn t(&self, x: u32) -> u32; }}\n{}{}{}{}impl S {{\n{}{}}}\n\
             struct A;\nstruct B;\nimpl T for A {{\n{}}}\nimpl T for B {{\n{}}}\n\
             #[cfg(test)]\nmod tests {{\n{}{}}}\n{no_logic}",
            f("fn six(x: u32) -> u32", 4, 1),
            f("fn six2(x: u32) -> u32", 4, 1),
            f("fn seven(x: u32) -> u32", 5, 2),
            f("fn seven2(x: u32) -> u32", 5, 2),
            f("fn m(&self, x: u32) -> u32", 5, 3),
            f("fn m2(&self, x: u32) -> u32", 5, 3),
            f("fn t(&self, x: u32) -> u32", 5, 4),
            f("fn t(&self, x: u32) -> u32", 5, 4),
            f("fn tc(x: u32) -> u32", 5, 5),
            f("fn tc2(x: u32) -> u32", 5, 5),
        );
        let fx = Fixture::new(&[("Cargo.toml", PKG), ("src/lib.rs", &src)]);
        let o = crate::findings::Options {
            min_dup: 8,
            ..opts(true)
        };
        let all = crate::findings::findings(&fx.scan(), &o);
        let dups: Vec<_> = all
            .all
            .into_iter()
            .filter(|f| f.kind == Kind::Duplicate)
            .collect();
        assert_eq!(
            summary(&dups),
            [
                (vec!["seven", "seven2"], Rule::Identical),
                (vec!["S::m", "S::m2"], Rule::Identical),
            ]
        );
        assert_eq!(dups[0].lines, 7);
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

    /// Short test code: one copy per crate is a note; a crate with two
    /// copies keeps the group a finding whatever other crates hold, and
    /// test-only hints don't promise a call between targets.
    #[test]
    fn short_test_code_in_crates_and_targets() {
        let h = |name: &str| format!("fn {name}(x: u32) -> u32 {}", body("1", "v"));
        let ws = "[workspace]\nmembers = [\"a\", \"b\"]\n";
        let lib = format!("#[cfg(test)]\nmod t {{\n{}}}\n", h("h1"));
        let (x, y) = (h("h2"), h("h3"));
        let files = |with: &[&str]| -> Vec<Finding> {
            let mut fs = vec![
                ("Cargo.toml", ws),
                ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
                ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            ];
            for (path, text) in [
                ("a/src/lib.rs", lib.as_str()),
                ("a/tests/x.rs", x.as_str()),
                ("b/tests/y.rs", y.as_str()),
            ] {
                if with.contains(&path) {
                    fs.push((path, text));
                }
            }
            dups(&fs).0
        };
        let all = files(&["a/src/lib.rs", "a/tests/x.rs", "b/tests/y.rs"]);
        assert_eq!(summary(&all), [(vec!["h1", "h2", "h3"], Rule::Crates)]);
        let hint = all[0].dup.as_ref().unwrap().hint.unwrap();
        assert!(hint.contains("dev-dependency"), "{hint}");
        let one_crate = files(&["a/src/lib.rs", "a/tests/x.rs"]);
        assert_eq!(summary(&one_crate), [(vec!["h1", "h2"], Rule::Identical)]);
        let hint = one_crate[0].dup.as_ref().unwrap().hint.unwrap();
        assert!(hint.contains("mod common"), "{hint}");
        let spread = files(&["a/tests/x.rs", "b/tests/y.rs"]);
        assert_eq!(summary(&spread), [(vec!["h2", "h3"], Rule::TestCrates)]);
        assert_eq!(spread[0].dup.as_ref().unwrap().tier, Tier::Note);
    }

    /// Same shape with a test copy: a note while at most one copy is impl
    /// code (a test's reference copy), a finding from two.
    #[test]
    fn same_shape_with_test_copies() {
        let long = |name: &str, v: &str| {
            let lines: String = (0..14)
                .map(|i| format!("    let {v}{i} = x + {i};\n"))
                .collect();
            format!("fn {name}(x: u32) -> u32 {{\n{lines}    {v}0\n}}\n")
        };
        let test = format!("#[cfg(test)]\nmod t {{\n{}}}\n", long("k", "b"));
        let one = format!("{}{test}", long("j", "a"));
        let (dups1, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", &one)]);
        assert_eq!(summary(&dups1), [(vec!["j", "k"], Rule::TestShape)]);
        assert_eq!(dups1[0].category, Category::Impl);
        let two = format!("{}{}{test}", long("j", "a"), long("l", "c"));
        let (dups2, _) = dups(&[("Cargo.toml", PKG), ("src/lib.rs", &two)]);
        assert_eq!(summary(&dups2), [(vec!["j", "l", "k"], Rule::SameShape)]);
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

    /// Identical methods: the same code on one type; a note on two.
    #[test]
    fn methods_on_other_types_are_notes() {
        let m = |ty: &str, name: &str| {
            format!(
                "impl {ty} {{\n    fn {name}(&self) -> bool {{\n        let m = self.n;\n        let k = m % 2;\n        k + m == 0\n    }}\n}}\n"
            )
        };
        let src = format!(
            "struct A {{ n: u32 }}\nstruct B {{ n: u32 }}\n{}{}{}",
            m("A", "is_empty"),
            m("B", "is_empty"),
            m("A", "none")
        );
        let src = src.as_str();
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
