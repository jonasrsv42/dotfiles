//! Ranked hints of bloat. Each finding says where to look and why; none of
//! them is proof that code can go.

use crate::scan::{FileAnalysis, Scan};
use crate::summary::file_counts;
use crate::visit::{FnFact, FnKind, ImplKind, ItemKind, Vis};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

/// Declaration order is report order: the order worth working through.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Functions with identical (or same-shape) bodies.
    Duplicate,
    /// Items nothing references, or only tests do.
    Unused,
    /// Traits with at most one implementation.
    SingleImplTrait,
    /// Functions whose body is one call passing their inputs along.
    PassThrough,
    /// Functions with more own code lines than --long-fn.
    LongFn,
    /// Files whose inline tests dwarf the code they test.
    TestHeavy,
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
    /// Own code lines of the item (the largest copy, for duplicates).
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
    /// Mentions of the name outside its definition. Name-based, so absent
    /// when several fns share the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_refs: Option<u32>,
    /// Traits: their impls. Unused types: impl blocks that go with them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impls: Option<Vec<Loc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copies: Option<Vec<Loc>>,
    /// The crate has code we can't see (generated `include!`s, files we
    /// couldn't parse) that may use this: confirm by compiling.
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
    /// Report unused/single-impl `pub` items of library crates, which are
    /// usually API for code outside the workspace.
    pub include_pub: bool,
}

pub struct Findings {
    pub all: Vec<Finding>,
    /// `pub` library items left out without --include-pub.
    pub hidden_pub: usize,
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
    /// Crates with code we can't see: dynamic `include!`s or unparsed files.
    uncertain: HashSet<usize>,
    hidden_pub: usize,
}

impl<'a> Cx<'a> {
    fn refs(&self, name: &str) -> (u32, u32) {
        let get = |m: &HashMap<&str, u32>| m.get(name).copied().unwrap_or(0);
        (get(&self.refs), get(&self.test_refs))
    }

    fn loc(&self, f: &FileAnalysis, name: &str, line: usize, end_line: usize) -> Loc {
        Loc {
            name: name.to_string(),
            file: f.rel.clone(),
            path: f.path.display().to_string(),
            line,
            end_line,
            krate: self.scan.crate_name(f).to_string(),
        }
    }

    /// A `pub` item of a library crate, hidden unless --include-pub.
    fn hide_pub(&mut self, f: &FileAnalysis, vis: Vis) -> bool {
        let lib = f.crate_idx.is_some_and(|c| self.scan.crates[c].has_lib);
        let hide = vis == Vis::Public && lib && !self.opts.include_pub;
        self.hidden_pub += usize::from(hide);
        hide
    }

    fn focus(&self) -> impl Iterator<Item = &'a FileAnalysis> + 'a {
        self.scan.files.iter().filter(|f| f.in_focus)
    }
}

pub fn findings(scan: &Scan, opts: &Options) -> Findings {
    let mut refs = HashMap::new();
    let mut test_refs = HashMap::new();
    let mut fn_defs: HashMap<&str, u32> = HashMap::new();
    let mut uncertain = HashSet::new();
    for f in &scan.files {
        let main = if f.whole_test {
            &mut test_refs
        } else {
            &mut refs
        };
        for (k, v) in &f.facts.refs {
            *main.entry(k.as_str()).or_default() += v;
        }
        for (k, v) in &f.facts.test_refs {
            *test_refs.entry(k.as_str()).or_default() += v;
        }
        for func in &f.facts.fns {
            *fn_defs.entry(func.ident.as_str()).or_default() += 1;
        }
        if (f.facts.dynamic_include || f.unanalysed)
            && let Some(c) = f.crate_idx
        {
            uncertain.insert(c);
        }
    }
    let mut cx = Cx {
        scan,
        opts,
        refs,
        test_refs,
        fn_defs,
        uncertain,
        hidden_pub: 0,
    };
    let mut out = Vec::new();
    duplicates(&cx, &mut out);
    unused(&mut cx, &mut out);
    single_impl_traits(&mut cx, &mut out);
    pass_through(&cx, &mut out);
    long_fns(&cx, &mut out);
    test_heavy(&cx, &mut out);
    out.sort_by(|a, b| {
        (a.kind, a.rank, &a.at.file, a.at.line).cmp(&(b.kind, b.rank, &b.at.file, b.at.line))
    });
    Findings {
        all: out,
        hidden_pub: cx.hidden_pub,
    }
}

fn category(test: bool) -> Category {
    if test { Category::Test } else { Category::Impl }
}

/// Code lines of a fn, minus items nested in its body (counted on their own).
fn own_lines(f: &FileAnalysis, func: &FnFact) -> usize {
    let nested = |l: usize| func.nested.iter().any(|&(s, e)| (s..=e).contains(&l));
    (func.line..=func.end_line.min(f.lines.len()))
        .filter(|&l| !nested(l) && f.code_lines(l, l) == 1)
        .count()
}

fn duplicates(cx: &Cx, out: &mut Vec<Finding>) {
    struct Member<'a> {
        file: &'a FileAnalysis,
        func: &'a FnFact,
        lines: usize,
    }
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
    let hash = |i: usize| members[i].func.body_hash.expect("filtered above");

    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    for i in 0..members.len() {
        exact.entry(hash(i).exact).or_default().push(i);
    }
    let mut groups: Vec<(Vec<usize>, bool)> = Vec::new();
    let mut grouped = HashSet::new();
    for g in exact.into_values().filter(|g| g.len() > 1) {
        grouped.extend(g.iter().copied());
        groups.push((g, true));
    }
    // Same shape only for longer bodies: short ones match by accident
    // (every builder setter has the same shape).
    let mut shape: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, m) in members.iter().enumerate() {
        if !grouped.contains(&i) && m.lines >= cx.opts.min_dup.max(15) {
            shape.entry(hash(i).shape).or_default().push(i);
        }
    }
    // Bodies differing only in literals are data tables (`match` arms
    // mapping to different strings, queries with different SQL).
    groups.extend(
        shape
            .into_values()
            .filter(|g| g.len() > 1 && g.iter().any(|&i| hash(i).names != hash(g[0]).names))
            .map(|g| (g, false)),
    );

    for (mut g, identical) in groups {
        // `#[cfg(unix)] fn join` / `#[cfg(windows)] fn join`: only one compiles.
        let ms: Vec<&Member> = g.iter().map(|&i| &members[i]).collect();
        let cfgs: HashSet<_> = ms.iter().filter_map(|m| m.func.cfg.as_ref()).collect();
        if cfgs.len() == ms.len() && ms.iter().all(|m| m.func.ident == ms[0].func.ident) {
            continue;
        }
        if !ms.iter().any(|m| m.file.in_focus) {
            continue;
        }
        // Lead with a copy inside the requested paths.
        g.sort_by_key(|&i| {
            let m = &members[i];
            (!m.file.in_focus, &m.file.rel, m.func.line)
        });
        let ms: Vec<&Member> = g.iter().map(|&i| &members[i]).collect();
        let max = ms.iter().map(|m| m.lines).max().unwrap_or(0);
        let saved = ms.iter().map(|m| m.lines).sum::<usize>() - max;
        let first = ms[0];
        let crates: HashSet<_> = ms.iter().map(|m| m.file.crate_idx).collect();
        let what = if identical {
            "identical bodies"
        } else {
            "bodies with the same shape (names/literals differ; needs a parameterised helper)"
        };
        let across = if crates.len() > 1 {
            "; spans crates (needs a shared dependency)"
        } else {
            ""
        };
        let all_test = ms.iter().all(|m| m.func.in_test || m.file.whole_test);
        let at = cx.loc(
            first.file,
            &first.func.name,
            first.func.line,
            first.func.end_line,
        );
        out.push(
            Finding {
                lines: max,
                lines_saved: Some(saved),
                identical: Some(identical),
                copies: Some(
                    ms.iter()
                        .map(|m| cx.loc(m.file, &m.func.name, m.func.line, m.func.end_line))
                        .collect(),
                ),
                detail: format!("{} {what}{across}; up to ~{saved} lines saved", ms.len()),
                ..Finding::new(Kind::Duplicate, category(all_test), at)
            }
            // Same-shape groups are harder to merge than identical ones.
            .rank(0, if identical { saved } else { saved / 2 }),
        );
    }
}

/// (crate, type name) -> its impl blocks with their code lines.
type TypeImpls<'a> = HashMap<(Option<usize>, &'a str), Vec<(Loc, usize)>>;

fn unused(cx: &mut Cx, out: &mut Vec<Finding>) {
    // Impl blocks that go with each type, so deleting it is complete.
    let mut type_impls: TypeImpls = HashMap::new();
    for f in &cx.scan.files {
        for i in &f.facts.impls {
            if matches!(i.kind, ImplKind::Inherent | ImplKind::Concrete) {
                let name = match i.trait_path.last() {
                    Some(t) => format!("impl {t} for {}", i.self_ty),
                    None => format!("impl {}", i.self_ty),
                };
                type_impls
                    .entry((f.crate_idx, i.self_name.as_str()))
                    .or_default()
                    .push((
                        cx.loc(f, &name, i.line, i.end_line),
                        f.code_lines(i.line, i.end_line),
                    ));
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
                name: &func.name,
                line: func.line,
                end_line: func.end_line,
                vis: func.vis,
                lines: own_lines(f, func),
                impls: None,
            };
            push_unused(cx, out, f, c);
        }
        for item in f.facts.items.iter().filter(|i| !i.in_test) {
            let impls = (item.kind != ItemKind::Trait).then(|| {
                type_impls
                    .remove(&(f.crate_idx, item.name.as_str()))
                    .unwrap_or_default()
            });
            let c = Candidate {
                ident: &item.name,
                name: &item.name,
                line: item.line,
                end_line: item.end_line,
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
    /// Display name.
    name: &'a str,
    line: usize,
    end_line: usize,
    vis: Vis,
    lines: usize,
    /// For types: their impl blocks with code lines.
    impls: Option<Vec<(Loc, usize)>>,
}

fn push_unused(cx: &mut Cx, out: &mut Vec<Finding>, f: &FileAnalysis, c: Candidate) {
    let (r, t) = cx.refs(c.ident);
    if r > 0 || cx.hide_pub(f, c.vis) {
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
    if c.vis == Vis::Public {
        detail.push_str("; pub, so maybe used outside the scanned code");
    }
    if uncertain {
        detail.push_str("; crate has code we can't see, so confirm by compiling");
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
            ..Finding::new(
                Kind::Unused,
                Category::Impl,
                cx.loc(f, c.name, c.line, c.end_line),
            )
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

fn single_impl_traits(cx: &mut Cx, out: &mut Vec<Finding>) {
    let resolver = Resolver::new(cx.scan);
    let workspace_types: HashSet<&str> = cx
        .scan
        .files
        .iter()
        .flat_map(|f| f.facts.type_names.iter().map(String::as_str))
        .collect();
    let mut impls: HashMap<(usize, String), Vec<(Loc, &str)>> = HashMap::new();
    let mut exempt: HashSet<(usize, String)> = HashSet::new();
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
                    exempt.insert(key);
                }
                ImplKind::Concrete | ImplKind::Derive => {
                    let name = if derive {
                        format!("#[derive] {}", i.self_ty)
                    } else {
                        i.self_ty.clone()
                    };
                    let at = cx.loc(f, &name, i.line, i.end_line);
                    impls.entry(key).or_default().push((at, &i.self_name));
                }
                ImplKind::Forwarding | ImplKind::Inherent => {}
            }
        }
        // A macro that implements a trait is usually invoked many times.
        for path in &f.facts.macro_impls {
            if let Some(key) = resolver.resolve(f, path, false) {
                exempt.insert(key);
            }
        }
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
        let found = impls.get(&key).map(Vec::as_slice).unwrap_or(&[]);
        let (r, tr) = cx.refs(&t.name);
        let detail = match found {
            _ if exempt.contains(&key) => continue,
            [] if r == 0 => continue, // reported as unused
            // An extension trait adding methods to a foreign type.
            [(_, self_name)] if !workspace_types.contains(self_name) => continue,
            [] if t.vis == Vis::Public => {
                "no impls in the workspace; pub, so maybe implemented elsewhere".to_string()
            }
            [] => format!("never implemented, but named {r} times"),
            [(one, _)] => format!("only impl: {} ({}:{})", one.name, one.file, one.line),
            _ => continue,
        };
        if cx.hide_pub(f, t.vis) {
            continue;
        }
        let lines = f.code_lines(t.line, t.end_line);
        let at = cx.loc(f, &t.name, t.line, t.end_line);
        out.push(
            Finding {
                vis: Some(t.vis),
                lines,
                refs: Some(r),
                test_refs: Some(tr),
                impls: Some(found.iter().map(|(l, _)| l.clone()).collect()),
                detail,
                ..Finding::new(Kind::SingleImplTrait, Category::Impl, at)
            }
            .rank(0, lines),
        );
    }
}

fn pass_through(cx: &Cx, out: &mut Vec<Finding>) {
    for f in cx.focus().filter(|f| !f.whole_test) {
        for func in f.facts.fns.iter().filter(|func| !func.in_test) {
            let Some(target) = &func.forwards_to else {
                continue;
            };
            // Counts mean nothing for a name many fns share (`push`, `get`).
            let defs = cx.fn_defs.get(func.ident.as_str()).copied().unwrap_or(0);
            let (r, t) = cx.refs(&func.ident);
            let refs = (defs <= 1).then_some((r, t));
            // `fn len(&self) { self.items.len() }` is a wrapper's API.
            let same_name =
                target.starts_with("self.") && target.ends_with(&format!(".{}", func.ident));
            let refs_text = match refs {
                Some((r, t)) => format!("{r} refs (+{t} in tests)"),
                None => format!("refs unknown ({defs} fns named {})", func.ident),
            };
            let lines = own_lines(f, func);
            let at = cx.loc(f, &func.name, func.line, func.end_line);
            let callers = refs.map_or(0xFFFF, |(r, t)| (r + t).min(0xFFFF));
            out.push(
                Finding {
                    vis: Some(func.vis),
                    lines,
                    target: Some(target.clone()),
                    refs: refs.map(|(r, _)| r),
                    test_refs: refs.map(|(_, t)| t),
                    detail: format!("forwards to {target}; {refs_text}"),
                    ..Finding::new(Kind::PassThrough, Category::Impl, at)
                }
                // Inline the least-called first; same-name wrappers last.
                .rank((u32::from(same_name) << 16) | callers, lines),
            );
        }
    }
}

fn long_fns(cx: &Cx, out: &mut Vec<Finding>) {
    for f in cx.focus() {
        for func in &f.facts.fns {
            let lines = own_lines(f, func);
            if lines > cx.opts.long_fn {
                let at = cx.loc(f, &func.name, func.line, func.end_line);
                out.push(
                    Finding {
                        lines,
                        detail: format!("{lines} own code lines"),
                        ..Finding::new(Kind::LongFn, category(func.in_test || f.whole_test), at)
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
            let at = cx.loc(f, &f.rel, 1, f.lines.len());
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
pub struct Report<'a> {
    pub root: String,
    pub thresholds: BTreeMap<&'static str, usize>,
    pub test_features: &'a [String],
    /// Findings per kind before --top.
    pub totals: BTreeMap<Kind, usize>,
    pub shown: BTreeMap<Kind, usize>,
    /// `pub` library items left out; --include-pub shows them.
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
    let mut totals = BTreeMap::new();
    let mut shown = BTreeMap::new();
    let mut findings = Vec::new();
    for f in &all.all {
        if (!kinds.is_empty() && !kinds.contains(&f.kind))
            || category.is_some_and(|c| c != f.category)
        {
            continue;
        }
        *totals.entry(f.kind).or_insert(0) += 1;
        let n = shown.entry(f.kind).or_insert(0);
        if top == 0 || *n < top {
            *n += 1;
            findings.push(f);
        }
    }
    Report {
        root: scan.root.display().to_string(),
        thresholds: BTreeMap::from([("long_fn", opts.long_fn), ("min_dup", opts.min_dup)]),
        test_features: &scan.test_features,
        totals,
        shown,
        // Only meaningful when the kinds that hide pub items are shown.
        hidden_pub: if kinds.is_empty()
            || kinds.contains(&Kind::Unused)
            || kinds.contains(&Kind::SingleImplTrait)
        {
            all.hidden_pub
        } else {
            0
        },
        findings,
    }
}

pub fn render(r: &Report) -> String {
    const MAX_COPIES: usize = 10;
    let mut out = String::new();
    if r.findings.is_empty() {
        let _ = writeln!(
            out,
            "no findings (long_fn > {}, min_dup >= {})",
            r.thresholds["long_fn"], r.thresholds["min_dup"]
        );
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
            let _ = writeln!(out, "\n## {title}{more}");
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
            f.at.file, f.at.line, f.at.name, f.detail
        );
        let others = f.copies.as_deref().unwrap_or(&[]).get(1..).unwrap_or(&[]);
        for c in others.iter().take(MAX_COPIES) {
            let _ = writeln!(out, "         = {}:{}  {}", c.file, c.line, c.name);
        }
        if others.len() > MAX_COPIES {
            let _ = writeln!(out, "         … {} more copies", others.len() - MAX_COPIES);
        }
    }
    if r.hidden_pub > 0 {
        let _ = writeln!(
            out,
            "\n{} pub items of library crates hidden (likely API); --include-pub shows them",
            r.hidden_pub
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn run(fx: &Fixture, include_pub: bool) -> Findings {
        let opts = Options {
            long_fn: 3,
            min_dup: 4,
            include_pub,
        };
        findings(&fx.scan(), &opts)
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
                "mod plat;\nmod kinds;\nmod py;\nmod gen;\nmod rec;\nuse crate::plat::LinuxThing as Active;\npub fn api() {\n    Active::go();\n    kinds::k();\n    rec::entry();\n}\n",
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
        assert_eq!(
            test_only.detail,
            "only referenced from tests (1); crate has code we can't see, so confirm by compiling"
        );
        // `pub` items of a lib crate are hidden by default...
        assert!(!unused.contains(&"api".to_string()));
        assert!(all.hidden_pub > 0);
        // ...and shown on request.
        assert!(names(&run(&fx, true), Kind::Unused).contains(&"api".to_string()));
    }

    #[test]
    fn unused_types_bring_their_impls() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "fn main() {}\nstruct Ghost;\nimpl Ghost {\n    fn a(&self) {}\n}\nimpl Clone for Ghost {\n    fn clone(&self) -> Self { Self }\n}\n",
            ),
        ]);
        let all = run(&fx, false);
        let ghost = all.all.iter().find(|f| f.at.name == "Ghost").unwrap();
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
                "pub trait Codec {}\npub trait Plugin {}\npub trait Render {}\npub trait Display {}\npub trait Debug {}\npub trait Lonely {}\npub trait Ext {}\npub struct Core;\nimpl Codec for Core {}\nimpl Plugin for Core {}\nimpl Render for Core {}\nimpl Display for Core {}\nimpl Debug for Core {}\nimpl Lonely for Core {}\nimpl Ext for std::time::Duration {}\npub mod prelude {\n    pub use crate::Plugin;\n}\n",
            ),
            ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
            (
                "app/src/main.rs",
                "use mcore::prelude::*;\nuse std::fmt;\nstruct A;\nimpl mcore::Codec for A {}\nimpl Plugin for A {}\nimpl facade::Render for A {}\nimpl fmt::Display for A {}\n#[derive(Debug)]\nstruct B;\nfn main() {}\n",
            ),
            ("facade/Cargo.toml", "[package]\nname = \"facade\"\n"),
            ("facade/src/lib.rs", "pub use mcore::Render;\n"),
        ]);
        let mut single = names(&run(&fx, true), Kind::SingleImplTrait);
        single.sort();
        assert_eq!(single, vec!["Debug", "Display", "Lonely"]);
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
            "fn a(x: u32) -> u32 {}fn b(x: u32) -> u32 {}#[cfg(unix)]\nfn join(x: u32) -> u32 {}#[cfg(windows)]\nfn join(x: u32) -> u32 {}{}{}",
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
        let opts = Options {
            long_fn: 3,
            min_dup: 4,
            include_pub: true,
        };
        let r = report(&scan, &all, &opts, &[Kind::LongFn], None, 1);
        assert_eq!(r.totals[&Kind::LongFn], 3);
        assert_eq!(r.shown[&Kind::LongFn], 1);
        assert_eq!(r.findings.len(), 1);
        let r = report(&scan, &all, &opts, &[], Some(Category::Test), 0);
        assert!(r.findings.iter().all(|f| f.category == Category::Test));
    }
}
