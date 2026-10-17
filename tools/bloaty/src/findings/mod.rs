//! Ranked hints of bloat. Each finding says where to look and why; none of
//! them is proof that code can go.
//!
//! This module holds the shared types, the per-run context (`Cx`), the
//! orchestration (`findings`), `report` and `render`; each kind lives in
//! its own submodule.

pub(crate) mod duplicate;
pub(crate) mod fixture;
mod long_fn;
pub(crate) mod parametrize;
mod pass_through;
mod single_impl_trait;
mod test_heavy;
mod unused;

pub use duplicate::{Duplicate, Tier};
pub use fixture::Prefix;
pub use parametrize::{Literals, Row};

use crate::names::{Loc, short_name};
use crate::scan::{FileAnalysis, Scan};
use crate::visit::{ItemKind, Vis};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

/// Declaration order is report order: the order worth working through.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Duplicate,
    Unused,
    #[value(alias = "single_impl_trait")]
    SingleImplTrait,
    #[value(alias = "long_fn")]
    LongFn,
    #[value(alias = "test_heavy")]
    TestHeavy,
    Parametrize,
    Fixture,
    /// Opt-in: only with `--kind pass-through`.
    #[value(alias = "pass_through")]
    PassThrough,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Impl,
    Test,
}

#[derive(Serialize, Debug)]
pub struct Finding {
    pub kind: Kind,
    /// Whether the code is implementation or test code.
    pub category: Category,
    #[serde(flatten)]
    pub at: Loc,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vis: Option<Vis>,
    /// Code lines involved: the item's own (plus its impl blocks for unused
    /// types), the largest copy's lines after its signature (at least 1)
    /// for duplicates, test lines for test-heavy, the tests' lines after
    /// their signatures for parametrize, the tests' lines in the shared
    /// opening for fixture.
    pub lines: usize,
    /// Duplicates: tier, copies and their facts.
    #[serde(flatten)]
    pub dup: Option<Duplicate>,
    /// Pass-through: the callee.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Mentions of the name outside its definition (name-based). For
    /// pass-through, absent when several fns share the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_refs: Option<u32>,
    /// Traits: their impls. Unused types: impl blocks that go with them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impls: Option<Vec<Loc>>,
    /// Parametrize, fixture: every test of the group, the finding's own
    /// first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<Row>>,
    /// Parametrize: literal positions that differ between the tests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub literals: Option<Literals>,
    /// Fixture: the opening the tests share.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<Prefix>,
    /// Unused only: the crate has code bloaty can't see (generated code,
    /// unparsed files, unresolved modules) that may use this. Compile to
    /// confirm.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub uncertain: bool,
    pub detail: String,
    #[serde(skip)]
    rank: Rank,
}

/// Order within a kind, most actionable first; fields compare in order.
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Rank {
    /// Duplicate notes after everything else of their kind.
    note: bool,
    /// Test code after impl code.
    test: bool,
    /// Private, then restricted, then pub.
    vis: Option<Vis>,
    /// Kind-specific tie-break, lower first (e.g. reference count).
    key: u32,
    /// Bigger first.
    size: Reverse<usize>,
}

impl Finding {
    fn new(kind: Kind, category: Category, at: Loc) -> Finding {
        Finding {
            kind,
            category,
            at,
            vis: None,
            lines: 0,
            dup: None,
            target: None,
            refs: None,
            test_refs: None,
            impls: None,
            tests: None,
            literals: None,
            prefix: None,
            uncertain: false,
            detail: String::new(),
            rank: Rank::default(),
        }
    }

    fn rank(mut self, key: u32, size: usize) -> Finding {
        self.rank = Rank {
            note: self.is_note(),
            test: self.category == Category::Test,
            vis: self.vis,
            key,
            size: Reverse(size),
        };
        self
    }
}

pub struct Options {
    pub long_fn: usize,
    pub min_dup: usize,
    /// Report `pub` items of open-API libraries as unused, single-impl or
    /// pass-through: they're usually used by code outside the repository.
    pub include_pub: bool,
}

pub struct Findings {
    pub all: Vec<Finding>,
    /// `pub` open-API items left out per kind, without --include-pub.
    pub hidden_pub: BTreeMap<Kind, usize>,
    /// Duplicate groups each criterion removed (groups that were
    /// duplicates by text size alone).
    pub excluded: BTreeMap<Criterion, usize>,
    /// Claims of name-based kinds left unmade, per kind and reason.
    pub unclaimed: BTreeMap<Kind, BTreeMap<Reason, usize>>,
    /// Items left out because the smell score excuses them, per kind and
    /// excuse (`table`, `dispatch`, `allowed`).
    pub excused: BTreeMap<Kind, BTreeMap<&'static str, usize>>,
}

/// Why a name-based kind made no claim about an item it would otherwise
/// have reported: the claim would need name resolution or macro expansion.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Other definitions in the workspace share the name the claim
    /// matches by (which one a mention or impl means is unknown).
    SameName,
    /// Named in an opaque macro (`macro_rules!`, `quote!`, ...), whose
    /// expansion may implement it.
    NamedInMacro,
}

/// What a duplicate must be beyond equal bodies (see findings/duplicate.rs).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Criterion {
    /// Enough logic of its own, not just a signature or forwarding.
    Substance,
}
/// What every finding pass needs.
struct Cx<'a> {
    scan: &'a Scan,
    opts: &'a Options,
    refs: HashMap<&'a str, u32>,
    test_refs: HashMap<&'a str, u32>,
    /// How many fns are defined with each name.
    fn_defs: HashMap<&'a str, u32>,
    /// How many fns, types, aliases and traits are defined with each name.
    defs: HashMap<&'a str, u32>,
    /// Words in opaque macro tokens anywhere in the workspace.
    macro_words: HashSet<&'a str>,
    /// Mentions of each type name inside its own definition and impls,
    /// split like `refs`/`test_refs`.
    type_self_refs: HashMap<&'a str, u32>,
    test_type_self_refs: HashMap<&'a str, u32>,
    /// Names a path may start with to reach workspace code: modules,
    /// types and crates defined here.
    workspace_names: HashSet<&'a str>,
    /// Module names each crate declares (to tell a file nothing declares
    /// from one declared somewhere bloaty couldn't follow).
    crate_modules: HashSet<(usize, &'a str)>,
    /// Crates with code we can't see: generated code, unparsed files,
    /// unresolved modules, item macros that may declare modules.
    uncertain: HashSet<usize>,
    hidden_pub: BTreeMap<Kind, usize>,
    excluded: BTreeMap<Criterion, usize>,
    unclaimed: BTreeMap<Kind, BTreeMap<Reason, usize>>,
    excused: BTreeMap<Kind, BTreeMap<&'static str, usize>>,
}

impl<'a> Cx<'a> {
    fn new(scan: &'a Scan, opts: &'a Options) -> Cx<'a> {
        let mut cx = Cx {
            scan,
            opts,
            refs: HashMap::new(),
            test_refs: HashMap::new(),
            fn_defs: HashMap::new(),
            defs: HashMap::new(),
            macro_words: HashSet::new(),
            type_self_refs: HashMap::new(),
            test_type_self_refs: HashMap::new(),
            workspace_names: scan.crates.iter().map(|c| c.lib_name.as_str()).collect(),
            crate_modules: HashSet::new(),
            uncertain: HashSet::new(),
            hidden_pub: BTreeMap::new(),
            excluded: BTreeMap::new(),
            unclaimed: BTreeMap::new(),
            excused: BTreeMap::new(),
        };
        for f in &scan.files {
            cx.add_file(f);
        }
        // Unseen code may use anything its crate depends on, too.
        let mut queue: Vec<usize> = cx.uncertain.iter().copied().collect();
        while let Some(c) = queue.pop() {
            for &dep in &scan.crates[c].links {
                if cx.uncertain.insert(dep) {
                    queue.push(dep);
                }
            }
        }
        cx
    }

    /// Counts a file's mentions, fns and names; notes code it can't see.
    fn add_file(&mut self, f: &'a FileAnalysis) {
        fn merge<'a>(from: &'a HashMap<String, u32>, into: &mut HashMap<&'a str, u32>) {
            for (k, v) in from {
                *into.entry(k.as_str()).or_default() += v;
            }
        }
        // A test file the recommended check builds: all its mentions are
        // test mentions. One it doesn't (`#[cfg(loom)] mod x;`): all are
        // uses, its `#[test]`s included.
        let all_test = f.whole_test && !f.unchecked_test;
        let (main, main_self) = if all_test {
            (&mut self.test_refs, &mut self.test_type_self_refs)
        } else {
            (&mut self.refs, &mut self.type_self_refs)
        };
        merge(&f.facts.refs, main);
        merge(&f.facts.type_self_refs, main_self);
        let (test, test_self) = if f.unchecked_test {
            (&mut self.refs, &mut self.type_self_refs)
        } else {
            (&mut self.test_refs, &mut self.test_type_self_refs)
        };
        merge(&f.facts.test_refs, test);
        merge(&f.facts.test_type_self_refs, test_self);
        for func in &f.facts.fns {
            *self.fn_defs.entry(func.ident.as_str()).or_default() += 1;
        }
        let traits = f.facts.items.iter().filter(|i| i.kind == ItemKind::Trait);
        let defined = (f.facts.fns.iter().map(|func| func.ident.as_str()))
            .chain(traits.map(|t| t.name.as_str()))
            .chain(f.facts.type_names.iter().map(String::as_str));
        for name in defined {
            *self.defs.entry(name).or_default() += 1;
        }
        self.macro_words
            .extend(f.facts.macro_words.iter().map(String::as_str));
        let names = f.facts.modules.iter().chain(&f.facts.type_names);
        self.workspace_names.extend(names.map(String::as_str));
        let Some(c) = f.crate_idx else { return };
        self.crate_modules
            .extend(f.facts.modules.iter().map(|m| (c, m.as_str())));
        let unseen_out_dir = f.facts.out_dir_include && !self.scan.crates[c].has_build_script();
        let unseen = f.unanalysed || f.unresolved_mods || f.facts.opaque_mods;
        if unseen_out_dir || f.facts.generated_code || unseen {
            self.uncertain.insert(c);
        }
    }

    fn refs(&self, name: &str) -> (u32, u32) {
        let get = |m: &HashMap<&str, u32>| m.get(name).copied().unwrap_or(0);
        (get(&self.refs), get(&self.test_refs))
    }

    /// The crate's `pub` items are likely API for code outside the repo.
    fn open_api(&self, f: &FileAnalysis) -> bool {
        f.crate_idx.is_some_and(|c| self.scan.crates[c].open_api)
    }

    /// A `pub` item of an open-API crate is left out unless --include-pub;
    /// counts what it leaves out.
    fn skip_pub(&mut self, kind: Kind, f: &FileAnalysis, vis: Vis) -> bool {
        let skip = vis == Vis::Public && self.open_api(f) && !self.opts.include_pub;
        if skip {
            *self.hidden_pub.entry(kind).or_default() += 1;
        }
        skip
    }

    /// Whether other definitions share `name` (see `defs`).
    fn ambiguous(&self, name: &str) -> bool {
        self.defs.get(name).copied().unwrap_or(0) > 1
    }

    /// Counts a claim `kind` didn't make, and why.
    fn exclude(&mut self, kind: Kind, why: Reason) {
        *self
            .unclaimed
            .entry(kind)
            .or_default()
            .entry(why)
            .or_default() += 1;
    }

    /// Counts an item `kind` left out because the smell score excuses it.
    fn excuse(&mut self, kind: Kind, excuse: &'static str) {
        *self
            .excused
            .entry(kind)
            .or_default()
            .entry(excuse)
            .or_default() += 1;
    }

    fn focus(&self) -> impl Iterator<Item = &'a FileAnalysis> + 'a {
        self.scan.files.iter().filter(|f| f.in_focus)
    }
}

pub fn findings(scan: &Scan, opts: &Options) -> Findings {
    let mut cx = Cx::new(scan, opts);
    let mut out = Vec::new();
    duplicate::duplicates(&mut cx, &mut out);
    // Fns in duplicate findings (not notes), for long_fn's detail.
    let duplicated: HashSet<(String, usize)> = out
        .iter()
        .filter_map(|f| f.dup.as_ref().filter(|d| d.tier == Tier::Finding))
        .flat_map(|d| &d.copies)
        .map(|c| (c.at.file.clone(), c.at.line))
        .collect();
    unused::unused(&mut cx, &mut out);
    single_impl_trait::single_impl_traits(&mut cx, &mut out);
    pass_through::pass_through(&mut cx, &mut out);
    long_fn::long_fns(&mut cx, &duplicated, &mut out);
    test_heavy::test_heavy(&cx, &mut out);
    parametrize::parametrize(&cx, &mut out);
    // After parametrize: a cluster wholly inside one candidate is left out.
    fixture::fixture(&cx, &mut out);
    out.sort_by(|a, b| {
        (a.kind, a.rank, &a.at.file, a.at.line).cmp(&(b.kind, b.rank, &b.at.file, b.at.line))
    });
    Findings {
        all: out,
        hidden_pub: cx.hidden_pub,
        excluded: cx.excluded,
        unclaimed: cx.unclaimed,
        excused: cx.excused,
    }
}

/// `1 ref`, `2 refs`.
fn plural(n: u32, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn category(test: bool) -> Category {
    if test { Category::Test } else { Category::Impl }
}
#[derive(Serialize)]
pub struct Thresholds {
    pub long_fn: usize,
    pub min_dup: usize,
}

#[derive(Serialize)]
pub struct Report<'a> {
    pub root: String,
    pub thresholds: Thresholds,
    pub test_features: &'a [String],
    /// Findings per kind before --top (duplicate notes apart).
    pub totals: BTreeMap<Kind, usize>,
    pub shown: BTreeMap<Kind, usize>,
    /// Duplicate notes, before and after --top (which applies to them on
    /// their own).
    pub duplicate_notes: NoteCount,
    /// `pub` open-API library items left out of the shown kinds;
    /// --include-pub shows them.
    pub hidden_pub: usize,
    /// Duplicate groups each criterion removed: groups of copies that are
    /// duplicates by text size alone (empty unless duplicates are shown).
    pub excluded_duplicates: &'a BTreeMap<Criterion, usize>,
    /// Claims of the shown name-based kinds left unmade, per kind and
    /// reason (they would need name resolution or macro expansion).
    pub unclaimed: BTreeMap<Kind, &'a BTreeMap<Reason, usize>>,
    /// Items of the shown kinds left out because the smell score excuses
    /// them, per kind and excuse.
    pub excused: BTreeMap<Kind, &'a BTreeMap<&'static str, usize>>,
    pub findings: Vec<&'a Finding>,
}

#[derive(Serialize, Default, Clone, Copy)]
pub struct NoteCount {
    pub total: usize,
    pub shown: usize,
}

impl Finding {
    fn is_note(&self) -> bool {
        self.dup.as_ref().is_some_and(|d| d.tier == Tier::Note)
    }
}

pub fn report<'a>(
    scan: &'a Scan,
    all: &'a Findings,
    opts: &Options,
    kinds: &[Kind],
    category: Option<Category>,
    top: usize,
) -> Report<'a> {
    // Pass-through is opt-in: mostly judgement calls.
    let wanted = |k: Kind| {
        if kinds.is_empty() {
            k != Kind::PassThrough
        } else {
            kinds.contains(&k)
        }
    };
    let mut totals = BTreeMap::new();
    let mut shown = BTreeMap::new();
    let mut notes = NoteCount::default();
    let mut findings = Vec::new();
    for f in &all.all {
        if !wanted(f.kind) || category.is_some_and(|c| c != f.category) {
            continue;
        }
        let (total, n) = if f.is_note() {
            (&mut notes.total, &mut notes.shown)
        } else {
            (
                totals.entry(f.kind).or_insert(0),
                shown.entry(f.kind).or_insert(0),
            )
        };
        *total += 1;
        if top == 0 || *n < top {
            *n += 1;
            findings.push(f);
        }
    }
    let hidden_pub = all
        .hidden_pub
        .iter()
        .filter(|(k, _)| wanted(**k))
        .map(|(_, n)| n)
        .sum();
    Report {
        root: scan.root.display().to_string(),
        thresholds: Thresholds {
            long_fn: opts.long_fn,
            min_dup: opts.min_dup,
        },
        test_features: &scan.test_features,
        totals,
        shown,
        duplicate_notes: notes,
        hidden_pub,
        excluded_duplicates: if wanted(Kind::Duplicate) {
            &all.excluded
        } else {
            &NONE_EXCLUDED
        },
        unclaimed: all
            .unclaimed
            .iter()
            .filter(|(k, _)| wanted(**k))
            .map(|(k, v)| (*k, v))
            .collect(),
        excused: all
            .excused
            .iter()
            .filter(|(k, _)| wanted(**k))
            .map(|(k, v)| (*k, v))
            .collect(),
        findings,
    }
}

/// A line `title: 2 unused (same_name), ...` of counts by kind and
/// reason; nothing without counts.
fn per_kind<R: Serialize>(
    out: &mut String,
    title: &str,
    counts: &BTreeMap<Kind, &BTreeMap<R, usize>>,
) {
    let parts: Vec<String> = counts
        .iter()
        .flat_map(|(k, why)| {
            why.iter()
                .map(move |(w, n)| format!("{n} {} ({})", json_name(k), json_name(w)))
        })
        .collect();
    if !parts.is_empty() {
        let _ = writeln!(out, "\n{title}: {}", parts.join(", "));
    }
}

/// The JSON name of a unit variant (`substance`).
fn json_name(v: &impl Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

static NONE_EXCLUDED: BTreeMap<Criterion, usize> = BTreeMap::new();

/// `note: [test] [pub] ` before a finding's name in text output.
fn tags(f: &Finding) -> String {
    let note = f.is_note();
    let vis = match f.vis {
        Some(Vis::Public) => "[pub] ",
        Some(Vis::Restricted) => "[pub(..)] ",
        _ => "",
    };
    let test = if f.category == Category::Test {
        "[test] "
    } else {
        ""
    };
    format!("{}{test}{vis}", if note { "note: " } else { "" })
}

pub fn render(r: &Report, scan: &Scan) -> String {
    const MAX_COPIES: usize = 10;
    let show = |path: &str| scan.display_path(Path::new(path));
    let mut out = String::new();
    if r.findings.is_empty() {
        let _ = writeln!(out, "no findings");
    }
    let mut kind = None;
    for f in &r.findings {
        if kind != Some(f.kind) {
            kind = Some(f.kind);
            let title = match f.kind {
                Kind::Duplicate => "Duplicate bodies (lines saved)",
                Kind::Unused => "Unused",
                Kind::SingleImplTrait => "Traits with at most one impl",
                Kind::PassThrough => "Pass-through functions",
                Kind::LongFn => "Long functions",
                Kind::TestHeavy => "Test-heavy files",
                Kind::Parametrize => "Table-driven-test candidates",
                Kind::Fixture => "Shared test setup (fixture candidates)",
            };
            let get = |m: &BTreeMap<Kind, usize>| m.get(&f.kind).copied().unwrap_or(0);
            let (mut n, mut total) = (get(&r.shown), get(&r.totals));
            if f.kind == Kind::Duplicate {
                n += r.duplicate_notes.shown;
                total += r.duplicate_notes.total;
            }
            let more = if n < total {
                format!(" (showing {n} of {total}; --top N, 0 for all)")
            } else {
                String::new()
            };
            if !out.is_empty() {
                out.push('\n');
            }
            let _ = writeln!(out, "## {title}{more}");
        }
        let size = f.dup.as_ref().map_or(f.lines, |d| d.lines_saved);
        let tags = tags(f);
        let _ = writeln!(
            out,
            "{size:>5}  {}:{}  {tags}{}  ({})",
            show(&f.at.path),
            f.at.line,
            short_name(&f.at.name),
            f.detail
        );
        // Other copies of a duplicate; impls of a trait or unused type;
        // other tests of a table-driven-test or fixture candidate.
        let others: Vec<&Loc> = match (&f.dup, &f.impls, &f.tests) {
            (Some(d), _, _) => d.copies.iter().skip(1).map(|c| &c.at).collect(),
            (None, Some(impls), _) => impls.iter().collect(),
            (None, None, Some(tests)) => tests.iter().skip(1).map(|t| &t.at).collect(),
            (None, None, None) => Vec::new(),
        };
        for c in others.iter().take(MAX_COPIES) {
            let _ = writeln!(
                out,
                "         = {}:{}  {}",
                show(&c.path),
                c.line,
                short_name(&c.name)
            );
        }
        if others.len() > MAX_COPIES {
            let _ = writeln!(out, "         … {} more", others.len() - MAX_COPIES);
        }
    }
    let not_dup: Vec<String> = r
        .excluded_duplicates
        .iter()
        .map(|(k, n)| format!("{n} {}", json_name(k)))
        .collect();
    if !not_dup.is_empty() {
        let _ = writeln!(
            out,
            "\nnot duplicates (groups with equal text, by criterion): {}",
            not_dup.join(", ")
        );
    }
    per_kind(
        &mut out,
        "not claimed (would need name resolution or macro expansion)",
        &r.unclaimed,
    );
    per_kind(
        &mut out,
        "excused (as the smell score excuses them)",
        &r.excused,
    );
    if r.hidden_pub > 0 {
        let _ = writeln!(
            out,
            "\n{} `pub` items of publishable libraries hidden (may have users outside the repo): --include-pub shows them; `publish = false` on internal crates shows them always",
            r.hidden_pub
        );
    }
    out
}
#[cfg(test)]
mod tests {
    //! Shared helpers (`run`, `names`, ...) for every kind's tests, too.
    pub(super) use super::*;
    pub(super) use crate::testutil::Fixture;

    pub(super) fn opts(include_pub: bool) -> Options {
        Options {
            long_fn: 3,
            min_dup: 4,
            include_pub,
        }
    }

    pub(super) fn run(fx: &Fixture, include_pub: bool) -> Findings {
        findings(&fx.scan(), &opts(include_pub))
    }

    pub(super) fn names(all: &Findings, kind: Kind) -> Vec<String> {
        all.all
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.at.name.clone())
            .collect()
    }

    /// Unused findings for a single binary crate with these extra files.
    pub(super) fn unused_in(main: &str, files: &[(&str, &str)]) -> Findings {
        let mut all = vec![
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/main.rs", main),
        ];
        all.extend_from_slice(files);
        run(&Fixture::new(&all), false)
    }

    #[test]
    fn ranks_impl_before_test_and_private_before_pub() {
        let long =
            |name: &str| format!("fn {name}() {{\n    a();\n    b();\n    c();\n    d();\n}}\n");
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                &format!(
                    "fn main() {{}}\n{}pub {}#[cfg(test)]\nmod tests {{\n{}}}\n",
                    long("private_dead"),
                    long("pub_dead"),
                    long("test_long")
                ),
            ),
        ]);
        let all = run(&fx, true);
        assert_eq!(
            names(&all, Kind::LongFn),
            vec!["private_dead", "pub_dead", "test_long"]
        );
        assert_eq!(names(&all, Kind::Unused), vec!["private_dead", "pub_dead"]);

        let scan = fx.scan();
        let o = opts(true);
        let r = report(&scan, &all, &o, &[Kind::LongFn], None, 1);
        assert_eq!(r.totals[&Kind::LongFn], 3);
        assert_eq!(r.shown[&Kind::LongFn], 1);
        assert_eq!(r.findings.len(), 1);
        let r = report(&scan, &all, &o, &[], Some(Category::Test), 0);
        assert!(r.findings.iter().all(|f| f.category == Category::Test));
    }

    #[test]
    fn renders_text() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "fn main() {}\nfn dead() {}\nstruct Ghost;\nimpl Ghost {}\n",
            ),
        ]);
        let scan = fx.scan();
        let all = findings(&scan, &opts(false));
        let text = render(&report(&scan, &all, &opts(false), &[], None, 0), &scan);
        assert!(text.contains("## Unused"), "{text}");
        assert!(text.contains("dead  (no references)"), "{text}");
        assert!(text.starts_with("## Unused"), "{text}");
        assert!(text.contains("main.rs:4  impl Ghost"), "{text}");
        let none = render(
            &report(&scan, &all, &opts(false), &[Kind::PassThrough], None, 0),
            &scan,
        );
        assert_eq!(none.trim(), "no findings");
    }

    /// The fields the Neovim picker and models rely on.
    #[test]
    fn json_contract() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/main.rs", "fn main() {}\nfn dead() {}\n"),
        ]);
        let scan = fx.scan();
        let all = findings(&scan, &opts(false));
        let r = report(&scan, &all, &opts(false), &[], None, 0);
        let v = serde_json::to_value(&r).unwrap();
        for key in [
            "root",
            "thresholds",
            "test_features",
            "totals",
            "shown",
            "hidden_pub",
            "excluded_duplicates",
            "unclaimed",
            "excused",
            "duplicate_notes",
            "findings",
        ] {
            assert!(v.get(key).is_some(), "report.{key}");
        }
        let f = &v["findings"][0];
        for key in [
            "kind",
            "category",
            "name",
            "file",
            "path",
            "start_line",
            "line",
            "end_line",
            "crate",
            "lines",
            "detail",
        ] {
            assert!(f.get(key).is_some(), "finding.{key}");
        }
        assert_eq!(f["kind"], "unused");
        assert_eq!(v["totals"]["unused"], 1);
        assert!(v["thresholds"]["long_fn"].is_u64());
        let n = |k: &str| f[k].as_u64().unwrap();
        assert!(n("start_line") <= n("line") && n("line") <= n("end_line"));
        assert!(Path::new(f["path"].as_str().unwrap()).is_absolute());
    }
}
