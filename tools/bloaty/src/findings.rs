//! Ranked hints of bloat. Each finding says where to look and why; none of
//! them is proof that code can go.

use crate::scan::{FileAnalysis, Scan};
use crate::summary::file_counts;
use crate::visit::{FnFact, FnKind, ImplKind, ItemKind, Vis};
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

#[derive(Serialize, Clone, Debug)]
pub struct Loc {
    pub name: String,
    /// Relative to the workspace root.
    pub file: String,
    /// Absolute.
    pub path: String,
    /// First line of the item's doc comments and attributes: delete from
    /// here to `end_line` to remove it whole.
    pub start_line: usize,
    /// The line with the item's name (where to jump).
    pub line: usize,
    pub end_line: usize,
    #[serde(rename = "crate")]
    pub krate: String,
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
    /// types), the largest copy for duplicates, test lines for test-heavy.
    pub lines: usize,
    /// Duplicates: code lines removed by keeping one copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_saved: Option<usize>,
    /// Duplicates: identical bodies, or only the same shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identical: Option<bool>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copies: Option<Vec<Loc>>,
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
            lines_saved: None,
            identical: None,
            target: None,
            refs: None,
            test_refs: None,
            impls: None,
            copies: None,
            uncertain: false,
            detail: String::new(),
            rank: Rank::default(),
        }
    }

    fn rank(mut self, key: u32, size: usize) -> Finding {
        self.rank = Rank {
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
}

/// Std derives that never implement a workspace trait unless imported.
const STD_DERIVES: &[&str] = &[
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
];

/// What every finding pass needs.
struct Cx<'a> {
    scan: &'a Scan,
    opts: &'a Options,
    refs: HashMap<&'a str, u32>,
    test_refs: HashMap<&'a str, u32>,
    /// How many fns are defined with each name.
    fn_defs: HashMap<&'a str, u32>,
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
    /// unresolved modules.
    uncertain: HashSet<usize>,
    hidden_pub: BTreeMap<Kind, usize>,
}

impl<'a> Cx<'a> {
    fn new(scan: &'a Scan, opts: &'a Options) -> Cx<'a> {
        let mut cx = Cx {
            scan,
            opts,
            refs: HashMap::new(),
            test_refs: HashMap::new(),
            fn_defs: HashMap::new(),
            type_self_refs: HashMap::new(),
            test_type_self_refs: HashMap::new(),
            workspace_names: scan.crates.iter().map(|c| c.lib_name.as_str()).collect(),
            crate_modules: HashSet::new(),
            uncertain: HashSet::new(),
            hidden_pub: BTreeMap::new(),
        };
        fn merge<'a>(from: &'a HashMap<String, u32>, into: &mut HashMap<&'a str, u32>) {
            for (k, v) in from {
                *into.entry(k.as_str()).or_default() += v;
            }
        }
        for f in &scan.files {
            // A test file the recommended check builds: all its mentions
            // are test mentions. One it doesn't (`#[cfg(loom)] mod x;`):
            // all are uses, its `#[test]`s included.
            let all_test = f.whole_test && !f.unchecked_test;
            let all_uses = f.unchecked_test;
            let (main, main_self) = if all_test {
                (&mut cx.test_refs, &mut cx.test_type_self_refs)
            } else {
                (&mut cx.refs, &mut cx.type_self_refs)
            };
            merge(&f.facts.refs, main);
            merge(&f.facts.type_self_refs, main_self);
            let (test, test_self) = if all_uses {
                (&mut cx.refs, &mut cx.type_self_refs)
            } else {
                (&mut cx.test_refs, &mut cx.test_type_self_refs)
            };
            merge(&f.facts.test_refs, test);
            merge(&f.facts.test_type_self_refs, test_self);
            for func in &f.facts.fns {
                *cx.fn_defs.entry(func.ident.as_str()).or_default() += 1;
            }
            cx.workspace_names.extend(
                f.facts
                    .modules
                    .iter()
                    .chain(&f.facts.type_names)
                    .map(String::as_str),
            );
            if let Some(c) = f.crate_idx {
                cx.crate_modules
                    .extend(f.facts.modules.iter().map(|m| (c, m.as_str())));
            }
            let Some(c) = f.crate_idx else { continue };
            let unseen_out_dir = f.facts.out_dir_include && !scan.crates[c].has_build_script();
            if unseen_out_dir || f.facts.generated_code || f.unanalysed || f.unresolved_mods {
                cx.uncertain.insert(c);
            }
        }
        // Unseen code may use anything its crate depends on, too.
        let mut queue: Vec<usize> = cx.uncertain.iter().copied().collect();
        while let Some(c) = queue.pop() {
            for &(dep, _) in &scan.crates[c].links {
                if cx.uncertain.insert(dep) {
                    queue.push(dep);
                }
            }
        }
        cx
    }

    fn refs(&self, name: &str) -> (u32, u32) {
        let get = |m: &HashMap<&str, u32>| m.get(name).copied().unwrap_or(0);
        (get(&self.refs), get(&self.test_refs))
    }

    fn loc(&self, f: &FileAnalysis, name: &str, lines: (usize, usize, usize)) -> Loc {
        let (start_line, line, end_line) = lines;
        Loc {
            name: name.to_string(),
            file: f.rel.clone(),
            path: f.path.display().to_string(),
            start_line,
            line,
            end_line,
            krate: self.scan.crate_name(f).to_string(),
        }
    }

    fn fn_loc(&self, f: &FileAnalysis, func: &FnFact) -> Loc {
        self.loc(f, &func.name, (func.start_line, func.line, func.end_line))
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

    fn focus(&self) -> impl Iterator<Item = &'a FileAnalysis> + 'a {
        self.scan.files.iter().filter(|f| f.in_focus)
    }
}

pub fn findings(scan: &Scan, opts: &Options) -> Findings {
    let mut cx = Cx::new(scan, opts);
    let mut out = Vec::new();
    duplicates(&cx, &mut out);
    let duplicated: HashSet<(String, usize)> = out
        .iter()
        .flat_map(|f| f.copies.iter().flatten())
        .map(|c| (c.file.clone(), c.line))
        .collect();
    unused(&mut cx, &mut out);
    single_impl_traits(&mut cx, &mut out);
    pass_through(&mut cx, &mut out);
    long_fns(&cx, &duplicated, &mut out);
    test_heavy(&cx, &mut out);
    out.sort_by(|a, b| {
        (a.kind, a.rank, &a.at.file, a.at.line).cmp(&(b.kind, b.rank, &b.at.file, b.at.line))
    });
    Findings {
        all: out,
        hidden_pub: cx.hidden_pub,
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

/// Code lines of a fn, minus items nested in its body (counted on their own).
fn own_lines(f: &FileAnalysis, func: &FnFact) -> usize {
    // Nested items arrive in source order: skip each range in one sweep.
    let mut nested = func.nested.iter().peekable();
    let mut count = 0;
    let mut l = func.line;
    let end = func.end_line.min(f.lines.len());
    while l <= end {
        while nested.next_if(|&&(_, e)| e < l).is_some() {}
        match nested.peek() {
            Some(&&(s, e)) if s <= l => l = e + 1,
            _ => {
                count += f.code_lines(l, l);
                l += 1;
            }
        }
    }
    count
}

/// A fn long enough to be compared for duplicates.
struct Member<'a> {
    file: &'a FileAnalysis,
    func: &'a FnFact,
    lines: usize,
}

impl Member<'_> {
    fn hash(&self) -> crate::visit::BodyHash {
        self.func.body_hash.expect("members have body hashes")
    }
}

fn duplicates(cx: &Cx, out: &mut Vec<Finding>) {
    let members: Vec<Member> = cx
        .scan
        .files
        .iter()
        .flat_map(|f| f.facts.fns.iter().map(move |func| (f, func)))
        .filter(|(_, func)| func.body_hash.is_some())
        .map(|(file, func)| Member {
            file,
            func,
            lines: own_lines(file, func),
        })
        .filter(|m| m.lines >= cx.opts.min_dup)
        .collect();
    for (group, identical) in duplicate_groups(&members, cx.opts.min_dup) {
        let mut ms: Vec<&Member> = group.iter().map(|&i| &members[i]).collect();
        if let Some(f) = duplicate_finding(cx, &mut ms, identical) {
            out.push(f);
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

fn duplicate_finding(cx: &Cx, ms: &mut [&Member], identical: bool) -> Option<Finding> {
    // `#[cfg(unix)] fn join` / `#[cfg(windows)] fn join`: only one compiles.
    let cfgs: HashSet<_> = ms.iter().filter_map(|m| m.func.cfg.as_ref()).collect();
    let same_ident = ms.iter().all(|m| m.func.ident == ms[0].func.ident);
    if (cfgs.len() == ms.len() && same_ident) || !ms.iter().any(|m| m.file.in_focus) {
        return None;
    }
    // Lead with a copy inside the requested paths.
    ms.sort_by_key(|m| (!m.file.in_focus, &m.file.rel, m.func.line));
    let max = ms.iter().map(|m| m.lines).max().unwrap_or(0);
    let saved = ms.iter().map(|m| m.lines).sum::<usize>() - max;
    let mut detail = format!(
        "{} {}",
        ms.len(),
        if identical {
            "identical bodies"
        } else {
            "bodies with the same shape (names/literals differ; needs a parameterised helper)"
        }
    );
    let same_type = ms
        .iter()
        .all(|m| base_type(&m.func.name) == base_type(&ms[0].func.name));
    let same_trait = ms
        .iter()
        .all(|m| trait_of(&m.func.name) == trait_of(&ms[0].func.name));
    let all_trait_impls = ms.iter().all(|m| m.func.kind == FnKind::TraitImpl);
    let names_differ = ms.iter().any(|m| m.func.name != ms[0].func.name);
    if same_ident && same_type && same_trait && names_differ {
        // `<Node<Async> as Build>::build` and `<Node<Sync> as Build>::build`.
        detail.push_str("; same fn for different type parameters: one generic impl (or a macro)");
    } else if all_trait_impls && same_ident && same_trait && !same_type {
        detail.push_str("; same trait method on different types: a macro or a default method");
    } else if all_trait_impls && same_type && !same_trait {
        // `<A as Display>::fmt` and `<A as Debug>::fmt`.
        detail.push_str("; same type, different traits: have one impl call the other");
    }
    let crates: HashSet<_> = ms.iter().map(|m| m.file.crate_idx).collect();
    if crates.len() > 1 {
        detail.push_str("; spans crates (needs a shared dependency)");
    }
    let _ = write!(detail, "; up to ~{saved} lines saved");
    let all_test = ms.iter().all(|m| m.func.in_test || m.file.whole_test);
    let first = ms[0];
    Some(
        Finding {
            lines: max,
            lines_saved: Some(saved),
            identical: Some(identical),
            copies: Some(ms.iter().map(|m| cx.fn_loc(m.file, m.func)).collect()),
            detail,
            ..Finding::new(
                Kind::Duplicate,
                category(all_test),
                cx.fn_loc(first.file, first.func),
            )
        }
        // Identical groups first: same-shape ones are harder to merge.
        .rank(u32::from(!identical), saved),
    )
}

/// `<Node<A> as Build<B>>::f` -> `Some("Build")`; inherent or free -> None.
fn trait_of(name: &str) -> Option<&str> {
    let rest = name.strip_prefix('<')?;
    // ` as ` at generic depth 0.
    let mut depth = 0i32;
    for (i, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ' ' if depth == 0 && rest[i..].starts_with(" as ") => {
                let t = &rest[i + 4..];
                return Some(&t[..t.find(['<', '>']).unwrap_or(t.len())]);
            }
            _ => {}
        }
    }
    None
}

/// `<Node<A> as T>::f` and `Node<A>::f` -> `Node`; `f` -> ``.
fn base_type(name: &str) -> &str {
    let name = name.strip_prefix('<').unwrap_or(name);
    let end = name.find(['<', ' ', ':']).unwrap_or(0);
    &name[..end]
}

/// (crate, type name) -> its non-test impl blocks with their code lines.
type TypeImpls<'a> = HashMap<(Option<usize>, &'a str), Vec<(Loc, usize)>>;

fn unused(cx: &mut Cx, out: &mut Vec<Finding>) {
    // Impl blocks that go with each type, so deleting it is complete. When
    // a crate defines the name more than once, only same-file impls are
    // surely this type's.
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
        for i in f.facts.impls.iter().filter(|i| !i.checked_test) {
            if matches!(i.kind, ImplKind::Inherent | ImplKind::Concrete) {
                let name = match i.trait_path.last() {
                    Some(t) => format!("impl {t} for {}", i.self_ty),
                    None => format!("impl {}", i.self_ty),
                };
                let at = cx.loc(f, &name, (i.start_line, i.line, i.end_line));
                type_impls
                    .entry((f.crate_idx, i.self_name.as_str()))
                    .or_default()
                    .push((at, f.code_lines(i.line, i.end_line)));
            }
        }
    }

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
                at: cx.fn_loc(f, func),
                self_refs: func.self_refs,
                test_self_refs: 0,
                vis: func.vis,
                lines: own_lines(f, func),
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
                at: cx.loc(f, &item.name, (item.start_line, item.line, item.end_line)),
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

/// Resolves trait paths in impls and derives to (crate, trait name), using
/// the file's imports, so `fmt::Display` or `#[derive(Debug)]` don't count
/// as impls of a workspace trait that happens to share the name.
struct Resolver<'a> {
    crate_by_lib: HashMap<&'a str, usize>,
    modules: Vec<HashSet<&'a str>>,
    /// Workspace trait definitions: (crate, name).
    traits: HashSet<(usize, &'a str)>,
    /// Trait name -> crates defining it.
    trait_crates: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Resolver<'a> {
    fn new(scan: &'a Scan) -> Self {
        let crate_by_lib = scan
            .crates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.lib_name.as_str(), i))
            .collect();
        let mut modules = vec![HashSet::new(); scan.crates.len()];
        let mut traits = HashSet::new();
        let mut trait_crates: HashMap<&str, Vec<usize>> = HashMap::new();
        for f in &scan.files {
            let Some(c) = f.crate_idx else { continue };
            modules[c].extend(f.facts.modules.iter().map(String::as_str));
            for t in f.facts.items.iter().filter(|i| i.kind == ItemKind::Trait) {
                if traits.insert((c, t.name.as_str())) {
                    trait_crates.entry(t.name.as_str()).or_default().push(c);
                }
            }
        }
        Resolver {
            crate_by_lib,
            modules,
            traits,
            trait_crates,
        }
    }

    fn crate_of_root(&self, this: usize, seg: &str) -> Option<usize> {
        match seg {
            "crate" | "self" | "super" => Some(this),
            s if self.modules[this].contains(s) => Some(this),
            s => self.crate_by_lib.get(s).copied(),
        }
    }

    /// `None` for traits from outside the workspace.
    fn resolve(&self, f: &FileAnalysis, path: &[String], derive: bool) -> Option<(usize, String)> {
        let this = f.crate_idx?;
        let (first, name) = (path.first()?, path.last()?);
        let import = f.facts.imports.iter().find(|(alias, _)| alias == first);
        let (c, name) = match (path.len(), import) {
            (1, Some((_, full))) => (self.crate_of_root(this, &full[0])?, full.last()?.clone()),
            (1, None) if derive && STD_DERIVES.contains(&name.as_str()) => return None,
            (1, None) => (this, name.clone()),
            (_, Some((_, full))) => (self.crate_of_root(this, &full[0])?, name.clone()),
            (_, None) => (self.crate_of_root(this, first)?, name.clone()),
        };
        if self.traits.contains(&(c, name.as_str())) {
            return Some((c, name));
        }
        // Reached through a glob import (`use dep::prelude::*`) or a
        // re-export in another crate: trust a name only one crate defines.
        match self.trait_crates.get(name.as_str()).map(Vec::as_slice) {
            Some([only]) => Some((*only, name)),
            _ => None,
        }
    }
}

/// Every workspace trait's impls (with the impl's bare self type name), and
/// the traits exempt from the check (blanket or macro-generated impls).
struct TraitImpls<'a> {
    impls: HashMap<(usize, String), Vec<(Loc, &'a str)>>,
    exempt: HashSet<(usize, String)>,
}

fn collect_trait_impls<'a>(cx: &Cx<'a>, resolver: &Resolver) -> TraitImpls<'a> {
    let mut t = TraitImpls {
        impls: HashMap::new(),
        exempt: HashSet::new(),
    };
    for f in &cx.scan.files {
        for i in f
            .facts
            .impls
            .iter()
            .filter(|i| i.kind != ImplKind::Inherent)
        {
            let derive = i.kind == ImplKind::Derive;
            let Some(key) = resolver.resolve(f, &i.trait_path, derive) else {
                continue;
            };
            match i.kind {
                ImplKind::Blanket => {
                    t.exempt.insert(key);
                }
                ImplKind::Concrete | ImplKind::Derive => {
                    let name = if derive {
                        format!("#[derive] {}", i.self_ty)
                    } else {
                        i.self_ty.clone()
                    };
                    let at = cx.loc(f, &name, (i.start_line, i.line, i.end_line));
                    t.impls.entry(key).or_default().push((at, &i.self_name));
                }
                ImplKind::Forwarding | ImplKind::Inherent => {}
            }
        }
        // A macro that implements a trait is usually invoked many times.
        for path in &f.facts.macro_impls {
            if let Some(key) = resolver.resolve(f, path, false) {
                t.exempt.insert(key);
            }
        }
    }
    t
}

fn single_impl_traits(cx: &mut Cx, out: &mut Vec<Finding>) {
    let resolver = Resolver::new(cx.scan);
    let found = collect_trait_impls(cx, &resolver);
    let mut crate_types: HashMap<Option<usize>, HashSet<&str>> = HashMap::new();
    for f in &cx.scan.files {
        crate_types
            .entry(f.crate_idx)
            .or_default()
            .extend(f.facts.type_names.iter().map(String::as_str));
    }

    let traits: Vec<_> = cx
        .focus()
        .filter(|f| !f.whole_test)
        .flat_map(|f| f.facts.items.iter().map(move |t| (f, t)))
        .filter(|(_, t)| t.kind == ItemKind::Trait && !t.in_test)
        .collect();
    for (f, t) in traits {
        let Some(c) = f.crate_idx else { continue };
        let key = (c, t.name.clone());
        let impls = found.impls.get(&key).map(Vec::as_slice).unwrap_or(&[]);
        let (r, tr) = cx.refs(&t.name);
        let local = |ty: &str| {
            crate_types
                .get(&f.crate_idx)
                .is_some_and(|s| s.contains(ty))
        };
        let detail = match impls {
            _ if found.exempt.contains(&key) => continue,
            [] if r == 0 => continue, // reported as unused
            // An extension trait adding methods to a type from another
            // crate: the orphan rule makes the trait necessary.
            [(_, self_name)] if !local(self_name) => continue,
            [] if t.vis == Vis::Public => {
                "no impls in the workspace; pub, so maybe implemented elsewhere".to_string()
            }
            [] => format!("never implemented, but named {r} times"),
            [(one, _)] => format!("only impl: {}", one.name),
            _ => continue,
        };
        if cx.skip_pub(Kind::SingleImplTrait, f, t.vis) {
            continue;
        }
        let lines = f.code_lines(t.line, t.end_line);
        let at = cx.loc(f, &t.name, (t.start_line, t.line, t.end_line));
        out.push(
            Finding {
                vis: Some(t.vis),
                lines,
                refs: Some(r),
                test_refs: Some(tr),
                impls: Some(impls.iter().map(|(l, _)| l.clone()).collect()),
                detail,
                ..Finding::new(Kind::SingleImplTrait, Category::Impl, at)
            }
            .rank(0, lines),
        );
    }
}

/// Only forwarding to the workspace's own fns counts: a free fn, an
/// associated fn, or another method on `self`. Delegating to a field
/// (`self.items.len()`), to a parameter, to a constructor (`new`,
/// `from_*`) or to code outside the workspace is a wrapper's job, not
/// indirection to remove.
fn pass_through(cx: &mut Cx, out: &mut Vec<Finding>) {
    for f in cx.focus().filter(|f| !f.whole_test) {
        for func in f.facts.fns.iter().filter(|func| !func.in_test) {
            let Some(target) = &func.forwards_to else {
                continue;
            };
            let callee = match target.rsplit_once('.') {
                Some(("self", method)) => method,
                Some(_) => continue,
                None => {
                    // `helper`, `crate::m::f`, `Self::f`, `Type::f` with a
                    // workspace type; not `Arc::ptr_eq` or `u32::from`.
                    let head = target.split("::").next().unwrap_or(target);
                    let std = matches!(head, "std" | "core" | "alloc");
                    let local = matches!(head, "crate" | "self" | "super" | "Self")
                        || (!std && cx.workspace_names.contains(head));
                    if target.contains("::") && !local {
                        continue;
                    }
                    target.rsplit("::").next().unwrap_or(target)
                }
            };
            let constructor = callee == "new" || callee.starts_with("from_");
            if constructor || callee == func.ident || !cx.fn_defs.contains_key(callee) {
                continue;
            }
            if cx.skip_pub(Kind::PassThrough, f, func.vis) {
                continue;
            }
            // Counts mean nothing for a name many fns share (`push`, `get`).
            let defs = cx.fn_defs.get(func.ident.as_str()).copied().unwrap_or(0);
            let (r, t) = cx.refs(&func.ident);
            let refs = (defs <= 1).then_some((r.saturating_sub(func.self_refs), t));
            let refs_text = match refs {
                Some((r, t)) => format!("{} (+{t} in tests)", plural(r, "ref")),
                None => format!("refs unknown ({defs} fns named {})", func.ident),
            };
            let lines = own_lines(f, func);
            let callers = refs.map_or(0xFFFF, |(r, t)| (r + t).min(0xFFFF));
            out.push(
                Finding {
                    vis: Some(func.vis),
                    lines,
                    target: Some(target.clone()),
                    refs: refs.map(|(r, _)| r),
                    test_refs: refs.map(|(_, t)| t),
                    detail: format!("forwards to {target}; {refs_text}"),
                    ..Finding::new(Kind::PassThrough, Category::Impl, cx.fn_loc(f, func))
                }
                // Inline the least-called first.
                .rank(callers, lines),
            );
        }
    }
}

/// `duplicated`: (file, line) of fns already reported as duplicates.
fn long_fns(cx: &Cx, duplicated: &HashSet<(String, usize)>, out: &mut Vec<Finding>) {
    for f in cx.focus() {
        for func in &f.facts.fns {
            let lines = own_lines(f, func);
            if lines > cx.opts.long_fn {
                let category = category(func.in_test || f.whole_test);
                out.push(
                    Finding {
                        lines,
                        detail: if duplicated.contains(&(f.rel.clone(), func.line)) {
                            format!("{lines} own code lines; also a duplicate")
                        } else {
                            format!("{lines} own code lines")
                        },
                        ..Finding::new(Kind::LongFn, category, cx.fn_loc(f, func))
                    }
                    .rank(0, lines),
                );
            }
        }
    }
}

fn test_heavy(cx: &Cx, out: &mut Vec<Finding>) {
    for f in cx.focus().filter(|f| !f.whole_test) {
        let c = file_counts(f);
        if c.implementation > 0 && c.test >= 200 && c.test > 3 * c.implementation {
            let name = f
                .path
                .file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let at = cx.loc(f, &name, (1, 1, f.lines.len()));
            out.push(
                Finding {
                    lines: c.test,
                    detail: format!("{} test lines for {} impl lines", c.test, c.implementation),
                    ..Finding::new(Kind::TestHeavy, Category::Test, at)
                }
                .rank(0, c.test),
            );
        }
    }
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
    /// Findings per kind before --top.
    pub totals: BTreeMap<Kind, usize>,
    pub shown: BTreeMap<Kind, usize>,
    /// `pub` open-API library items left out of the shown kinds;
    /// --include-pub shows them.
    pub hidden_pub: usize,
    pub findings: Vec<&'a Finding>,
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
    let mut findings = Vec::new();
    for f in &all.all {
        if !wanted(f.kind) || category.is_some_and(|c| c != f.category) {
            continue;
        }
        *totals.entry(f.kind).or_insert(0) += 1;
        let n = shown.entry(f.kind).or_insert(0);
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
        hidden_pub,
        findings,
    }
}

/// Long names with generics read `<Node<…> as GraphBuilder<…>>::build` in
/// text output; JSON keeps them whole.
fn short_name(name: &str) -> String {
    if name.chars().count() <= 60 {
        return name.to_string();
    }
    let mut out = String::new();
    let mut depth = 0;
    let mut prev = ' ';
    for c in name.chars() {
        match c {
            // Generic arguments open right after a name: `Node<`, not `<Node`.
            '<' if depth > 0 || prev.is_alphanumeric() || prev == '_' => {
                if depth == 0 {
                    out.push_str("<…");
                }
                depth += 1;
            }
            '>' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push('>');
                }
            }
            _ if depth > 0 => {}
            c => out.push(c),
        }
        prev = c;
    }
    if depth > 0 {
        out.push('>');
    }
    out
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
            };
            let (n, total) = (r.shown[&f.kind], r.totals[&f.kind]);
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
        let size = f.lines_saved.unwrap_or(f.lines);
        let mut tags = String::new();
        if f.category == Category::Test {
            tags.push_str("[test] ");
        }
        match f.vis {
            Some(Vis::Public) => tags.push_str("[pub] "),
            Some(Vis::Restricted) => tags.push_str("[pub(..)] "),
            _ => {}
        }
        let _ = writeln!(
            out,
            "{size:>5}  {}:{}  {tags}{}  ({})",
            show(&f.at.path),
            f.at.line,
            short_name(&f.at.name),
            f.detail
        );
        // Other copies of a duplicate; impls of a trait or unused type.
        let others = match (&f.copies, &f.impls) {
            (Some(copies), _) => copies.get(1..).unwrap_or(&[]),
            (None, Some(impls)) => impls.as_slice(),
            (None, None) => &[],
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
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn short_names_collapse_generics() {
        let long = "<Node<Allocated, Async, Async, Sync, Left, Right, Out> as GraphBuilder<ThreadIdType>>::build";
        assert_eq!(short_name(long), "<Node<…> as GraphBuilder<…>>::build");
        assert_eq!(short_name("Foo<T>::f"), "Foo<T>::f");
        let huge = format!(
            "<Wrapper<{}> as Tr>::never_called_method_name",
            "A, ".repeat(60)
        );
        assert_eq!(
            short_name(&huge),
            "<Wrapper<…> as Tr>::never_called_method_name"
        );
        assert_eq!(trait_of("<A<B> as Display>::fmt"), Some("Display"));
        assert_eq!(trait_of("A::f"), None);
    }

    fn opts(include_pub: bool) -> Options {
        Options {
            long_fn: 3,
            min_dup: 4,
            include_pub,
        }
    }

    fn run(fx: &Fixture, include_pub: bool) -> Findings {
        findings(&fx.scan(), &opts(include_pub))
    }

    fn names(all: &Findings, kind: Kind) -> Vec<String> {
        all.all
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.at.name.clone())
            .collect()
    }

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
    fn traits_resolve_across_crates() {
        let fx = Fixture::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"core\", \"app\", \"facade\"]\n",
            ),
            (
                "core/Cargo.toml",
                "[package]\nname = \"my-core-pkg\"\n[lib]\nname = \"mcore\"\n",
            ),
            (
                "core/src/lib.rs",
                "pub trait Codec {}\npub trait Plugin {}\npub trait Render {}\npub trait Display {}\npub trait Debug {}\npub trait Lonely {}\npub trait Ext {}\npub trait EpochExt {}\npub struct Core;\nimpl Codec for Core {}\nimpl Plugin for Core {}\nimpl Render for Core {}\nimpl Display for Core {}\nimpl Debug for Core {}\nimpl Lonely for Core {}\nimpl Ext for std::time::Duration {}\nimpl EpochExt for facade::Epoch {}\npub mod prelude {\n    pub use crate::Plugin;\n}\n",
            ),
            ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
            (
                "app/src/main.rs",
                "use mcore::prelude::*;\nuse std::fmt;\nstruct A;\nimpl mcore::Codec for A {}\nimpl Plugin for A {}\nimpl facade::Render for A {}\nimpl fmt::Display for A {}\n#[derive(Debug)]\nstruct B;\nfn main() {}\n",
            ),
            ("facade/Cargo.toml", "[package]\nname = \"facade\"\n"),
            (
                "facade/src/lib.rs",
                "pub use mcore::Render;\npub struct Epoch;\n",
            ),
        ]);
        let mut single = names(&run(&fx, true), Kind::SingleImplTrait);
        single.sort();
        assert_eq!(single, vec!["Debug", "Display", "Lonely"]);
    }

    #[test]
    fn pass_through_only_flags_indirection_to_workspace_fns() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "fn main() { let s = S { v: vec![] }; s.len(); s.go(1); s.fwd(1); verify(1); s.make(1); }\nstruct S { v: Vec<u8> }\nimpl S {\n    fn len(&self) -> usize { self.v.len() }\n    fn go(&self, x: u8) -> u8 { self.real_go(x) }\n    fn real_go(&self, x: u8) -> u8 { x + 1 }\n    fn fwd(&self, x: u8) -> u8 { helper(x) }\n    fn make(&self, x: u8) -> S { S::new(x) }\n    fn new(x: u8) -> S { S { v: vec![x] } }\n}\nfn helper(x: u8) -> u8 { x }\nfn verify(t: u8) -> bool { checks::verify(t) }\nmod checks {\n    pub fn verify(t: u8) -> bool { t > 0 }\n}\n",
            ),
        ]);
        let mut pass = names(&run(&fx, false), Kind::PassThrough);
        pass.sort();
        assert_eq!(pass, vec!["S::fwd", "S::go"]);
    }

    #[test]
    fn duplicates_skip_data_tables_and_cfg_twins() {
        let body = |lit: &str| {
            format!(
                "{{\n    let v = x + {lit};\n    let w = v * 2;\n    let z = w - v;\n    z + v\n}}\n"
            )
        };
        let table = |name: &str, a: &str, b: &str| {
            let arms: String = (0..16)
                .map(|i| format!("        {i} => \"{a}{i}{b}\",\n"))
                .collect();
            format!(
                "fn {name}(k: u8) -> &'static str {{\n    match k {{\n{arms}        _ => \"\",\n    }}\n}}\n"
            )
        };
        let src = format!(
            "fn a(x: u32) -> u32 {}fn b(x: u32) -> u32 {}#[cfg(unix)]\nmod sys {{\n    fn join(x: u32) -> u32 {}}}\n#[cfg(windows)]\nmod sys {{\n    fn join(x: u32) -> u32 {}}}\n{}{}",
            body("1"),
            body("1"),
            body("2"),
            body("2"),
            table("name", "n", ""),
            table("as_stored", "s", "_x"),
        );
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        let all = run(&fx, true);
        let dups: Vec<_> = all
            .all
            .iter()
            .filter(|f| f.kind == Kind::Duplicate)
            .collect();
        assert_eq!(dups.len(), 1, "{dups:#?}");
        assert_eq!(dups[0].identical, Some(true));
        assert_eq!(dups[0].lines_saved, Some(6));
        assert_eq!(dups[0].copies.as_ref().unwrap().len(), 2);
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

    /// Unused findings for a single binary crate with these extra files.
    fn unused_in(main: &str, files: &[(&str, &str)]) -> Findings {
        let mut all = vec![
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/main.rs", main),
        ];
        all.extend_from_slice(files);
        run(&Fixture::new(&all), false)
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
    fn pass_through_ignores_std_targets_and_is_opt_in() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "use std::sync::Arc;\nfn main() { same(&Arc::new(1), &Arc::new(1)); widen(1); go(1); }\nfn same(a: &Arc<u8>, b: &Arc<u8>) -> bool { Arc::ptr_eq(a, b) }\nfn widen(x: u8) -> u32 { u32::from(x) }\nfn go(x: u8) -> u8 { real(x) }\nfn real(x: u8) -> u8 { x }\nfn ptr_eq() {}\nfn from() {}\n",
            ),
        ]);
        let all = run(&fx, false);
        assert_eq!(names(&all, Kind::PassThrough), vec!["go"]);
        let scan = fx.scan();
        let default = report(&scan, &all, &opts(false), &[], None, 0);
        assert!(default.findings.iter().all(|f| f.kind != Kind::PassThrough));
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
    fn duplicate_notes_respect_traits() {
        let body = "{\n        let a = 1;\n        let b = a + 2;\n        let c = b * 3;\n        write!(f, \"{}\", c)\n    }\n";
        let src = format!(
            "use std::fmt;\nstruct A;\nimpl fmt::Display for A {{\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {body}}}\nimpl fmt::Debug for A {{\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {body}}}\nfn main() {{}}\n"
        );
        let all = unused_in(&src, &[]);
        let dup = all.all.iter().find(|f| f.kind == Kind::Duplicate).unwrap();
        assert!(dup.detail.contains("different traits"), "{}", dup.detail);
        assert!(!dup.detail.contains("type parameters"), "{}", dup.detail);
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
