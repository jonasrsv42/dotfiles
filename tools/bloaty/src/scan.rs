//! Scans the whole workspace around the requested paths: finds crates and
//! .rs files (respecting .gitignore, skipping cargo target dirs), analyses
//! files in parallel, and resolves module trees from the crate roots to work
//! out which files are test code. Reports narrow to the requested paths, but
//! classification and cross-file matching always see the whole workspace.

use crate::lines::{self, LineClass};
use crate::visit::{self, Ctx, FileFacts, FnFact};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Files larger than this aren't parsed: syn needs ~100x their size in
/// memory and they're almost always generated.
const MAX_PARSE_BYTES: usize = 4 << 20;

pub struct ScanOptions {
    /// Cargo features to treat like `cfg(test)`, on top of detected ones.
    pub test_features: Vec<String>,
    /// Detected test features to not treat like `cfg(test)`.
    pub no_test_features: Vec<String>,
    /// Files nested deeper than this aren't parsed: syn recurses once per
    /// level, so this must fit the stack the scan runs on.
    pub max_depth: usize,
    /// Analyse only the crates holding the requested paths, not the crates
    /// linked to them (enough for line counts, not for findings).
    pub focus_crates_only: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            test_features: Vec::new(),
            no_test_features: Vec::new(),
            max_depth: 2000,
            focus_crates_only: false,
        }
    }
}

pub struct Crate {
    pub name: String,
    /// How other crates write it in paths: `[lib] name`, or the package
    /// name with `-` as `_`.
    pub lib_name: String,
    pub dir: PathBuf,
    /// A library whose `pub` items may be API for code outside the
    /// repository: it has a lib target and isn't `publish = false`.
    pub open_api: bool,
    has_lib: bool,
    /// `None`: `publish.workspace = true`, decided by the workspace.
    publish: Option<bool>,
    deps: Vec<Dep>,
    /// Workspace crates it depends on (index, dev-only), resolved from
    /// `deps` once every manifest is read.
    pub links: Vec<(usize, bool)>,
    roots: Vec<PathBuf>,
    /// Root files that are tests/benches (whole-test module trees).
    test_roots: Vec<PathBuf>,
    build_script: Option<PathBuf>,
}

impl Crate {
    pub fn has_build_script(&self) -> bool {
        self.build_script.is_some()
    }

    /// build.rs (or the declared build script) and anything under build/.
    fn is_build_script(&self, p: &Path) -> bool {
        self.build_script.as_deref() == Some(p) || p.starts_with(self.dir.join("build"))
    }
}

/// A dependency as written in a manifest.
struct Dep {
    /// The key in the dependency table (`workspace = true` looks it up).
    key: String,
    /// Package name, renames (`package = ".."`) resolved.
    package: String,
    /// `path = ".."`, canonical.
    path: Option<PathBuf>,
    workspace: bool,
    dev: bool,
}

pub struct FileAnalysis {
    pub path: PathBuf,
    /// Path relative to the workspace root, for display.
    pub rel: String,
    pub crate_idx: Option<usize>,
    /// Inside one of the paths the user asked about.
    pub in_focus: bool,
    /// The whole file is test code: under a tests/benches target, behind
    /// `#![cfg(test)]`, or a module declared from test code.
    pub whole_test: bool,
    /// Syntax-level facts are missing (unparsable, too deep or too large):
    /// only lines and name mentions are known.
    pub unanalysed: bool,
    /// Declares a `mod x;` whose file wasn't found (gitignored, generated,
    /// or for another platform): code we can't see.
    pub unresolved_mods: bool,
    /// No crate root reaches it through `mod` declarations: not compiled.
    pub orphan: bool,
    /// Whole-test, but only under cfgs the recommended check doesn't build
    /// (`#[cfg(loom)] mod x;`): its mentions count as ordinary uses.
    pub unchecked_test: bool,
    pub lines: Vec<LineClass>,
    /// Per line (0-based): inside a test-only item.
    pub test_mask: Vec<bool>,
    pub facts: FileFacts,
}

impl FileAnalysis {
    pub fn is_test_line(&self, line0: usize) -> bool {
        self.whole_test || self.test_mask.get(line0).copied().unwrap_or(false)
    }

    /// Code lines in a 1-based inclusive range.
    pub fn code_lines(&self, start: usize, end: usize) -> usize {
        let end = end.min(self.lines.len());
        (start.max(1)..=end)
            .filter(|&l| self.lines[l - 1] == LineClass::Code)
            .count()
    }

    /// Code lines of a fn, minus items nested in its body (counted on
    /// their own).
    pub fn own_lines(&self, func: &FnFact) -> usize {
        // Nested items arrive in source order: skip each range in one sweep.
        let mut nested = func.nested.iter().peekable();
        let mut count = 0;
        let mut l = func.line;
        let end = func.end_line.min(self.lines.len());
        while l <= end {
            while nested.next_if(|&&(_, e)| e < l).is_some() {}
            match nested.peek() {
                Some(&&(s, e)) if s <= l => l = e + 1,
                _ => {
                    count += self.code_lines(l, l);
                    l += 1;
                }
            }
        }
        count
    }
}

pub struct Scan {
    pub root: PathBuf,
    pub crates: Vec<Crate>,
    pub files: Vec<FileAnalysis>,
    /// Cargo features treated like `cfg(test)`.
    pub test_features: Vec<String>,
    /// Files that couldn't be read or fully analysed, with the reason.
    pub skipped: Vec<(String, String)>,
    /// The working directory, when it is inside `root`.
    cwd: Option<PathBuf>,
}

impl Scan {
    pub fn crate_name(&self, f: &FileAnalysis) -> &str {
        f.crate_idx.map_or("", |i| &self.crates[i].name)
    }

    /// Build scripts and examples: support code, where `unwrap` is
    /// idiomatic, not the product.
    pub fn is_support(&self, f: &FileAnalysis) -> bool {
        f.crate_idx.is_some_and(|i| {
            let c = &self.crates[i];
            c.is_build_script(&f.path) || f.path.starts_with(c.dir.join("examples"))
        })
    }

    /// A path for people: relative to the working directory (`../x` for
    /// files beside it) when that is inside the root, so it opens from the
    /// shell; else relative to the root.
    pub fn display_path(&self, path: &Path) -> String {
        match &self.cwd {
            Some(cwd) => relative_to(path, cwd).display().to_string(),
            None => path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .display()
                .to_string(),
        }
    }
}

/// `path` relative to `base`, climbing with `..` as needed.
fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let common = base
        .ancestors()
        .find(|a| path.starts_with(a))
        .unwrap_or(Path::new("/"));
    let ups = base
        .strip_prefix(common)
        .map_or(0, |r| r.components().count());
    let mut out: PathBuf = std::iter::repeat_n("..", ups).collect();
    out.push(path.strip_prefix(common).unwrap_or(path));
    out
}

pub fn scan(focus: &[PathBuf], opts: &ScanOptions) -> Result<Scan, String> {
    let focus: Vec<PathBuf> = focus
        .iter()
        .map(|p| {
            p.canonicalize()
                .map_err(|e| format!("{}: {e}", p.display()))
        })
        .collect::<Result<_, _>>()?;
    // Outside any project the "workspace" is whatever lies around PATH
    // (`/tmp`, `~`): refuse rather than scan it all.
    if let Some(p) = focus.iter().find(|p| {
        !p.ancestors()
            .any(|d| d.join("Cargo.toml").is_file() || d.join(".git").exists())
    }) {
        return Err(format!(
            "{}: not inside a cargo project or git repository",
            p.display()
        ));
    }
    let roots: Vec<PathBuf> = focus.iter().map(|p| workspace_root(p, true)).collect();
    let mut root = roots[0].clone();
    if let Some(other) = roots.iter().find(|r| **r != root) {
        return Err(format!(
            "paths are in different workspaces ({} and {}); run bloaty on each",
            root.display(),
            other.display()
        ));
    }

    let (mut rs_files, mut manifests, mut skipped) = walk(&root);
    // A crate in a directory the repository ignores (`scratch/`): walk from
    // its own workspace instead.
    if !rs_files
        .iter()
        .any(|f| focus.iter().any(|p| f.starts_with(p)))
    {
        let cargo_root = workspace_root(&focus[0], false);
        if cargo_root != root {
            root = cargo_root;
            (rs_files, manifests, skipped) = walk(&root);
        }
    }
    rs_files.sort();
    manifests.sort();

    let (crates, workspaces, test_features) = load_crates(&manifests, opts);
    let crate_dirs: HashMap<&Path, usize> = crates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.dir.as_path(), i))
        .collect();
    let crate_of = |p: &Path| p.ancestors().find_map(|d| crate_dirs.get(d).copied());
    let related = related_crates(&crates, &workspaces, &focus, opts.focus_crates_only);
    rs_files.retain(|p| {
        focus.iter().any(|f| p.starts_with(f)) || crate_of(p).is_some_and(|c| related[c])
    });

    let analysed: Vec<_> = rs_files
        .par_iter()
        .map(|p| {
            let build_script = crate_of(p).is_some_and(|c| crates[c].is_build_script(p));
            let ctx = Ctx {
                test_features: &test_features,
                build_script,
            };
            analyse(&root, p, &ctx, opts.max_depth)
        })
        .collect();
    let mut files = Vec::new();
    for a in analysed {
        match a {
            Ok((mut f, problem)) => {
                f.in_focus = focus.iter().any(|p| f.path.starts_with(p));
                f.crate_idx = crate_of(&f.path);
                if let Some(e) = problem {
                    skipped.push((f.rel.clone(), e));
                }
                files.push(f);
            }
            Err(s) => skipped.push(s),
        }
    }
    resolve_modules(&mut files, &crates);

    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    skipped.sort();
    let mut test_features: Vec<String> = test_features.into_iter().collect();
    test_features.sort();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|d| d.canonicalize().ok())
        .filter(|d| d.starts_with(&root));
    Ok(Scan {
        root,
        crates,
        files,
        test_features,
        skipped,
        cwd,
    })
}

/// Reads every manifest: the crates, the `[workspace]`s, and the cargo
/// features to treat like `cfg(test)` (detected by name, adjusted by the
/// options).
fn load_crates(
    manifests: &[PathBuf],
    opts: &ScanOptions,
) -> (Vec<Crate>, Vec<Workspace>, HashSet<String>) {
    let read: Vec<_> = manifests
        .par_iter()
        .filter_map(|m| read_manifest(m))
        .collect();
    let mut crates = Vec::new();
    let mut workspaces = Vec::new();
    let mut patches: HashMap<String, PathBuf> = HashMap::new();
    let mut test_features: HashSet<String> = opts.test_features.iter().cloned().collect();
    for info in read {
        workspaces.extend(info.workspace);
        patches.extend(info.patches);
        let Some((krate, features)) = info.krate else {
            continue;
        };
        test_features.extend(features.into_iter().filter(|f| is_test_feature(f)));
        crates.push(krate);
    }
    for f in &opts.no_test_features {
        test_features.remove(f);
    }
    link_crates(&mut crates, &workspaces, &patches);
    let ws_at: HashMap<&Path, &Workspace> =
        workspaces.iter().map(|w| (w.dir.as_path(), w)).collect();
    for c in &mut crates {
        let publish = c.publish.unwrap_or_else(|| {
            let ws = c.dir.ancestors().find_map(|d| ws_at.get(d));
            ws.is_none_or(|w| w.publish)
        });
        // Even used inside the repo, a publishable library may have users
        // outside it (tokio and tokio-util). Mark internal crates
        // `publish = false` to have their `pub` items checked.
        c.open_api = c.has_lib && publish;
    }
    (crates, workspaces, test_features)
}

/// Resolves each crate's dependencies to workspace crates: `path = ..`,
/// `workspace = true` (through the nearest `[workspace.dependencies]`),
/// and registry/git dependencies redirected by `[patch]`/`[replace]`.
fn link_crates(crates: &mut [Crate], workspaces: &[Workspace], patches: &HashMap<String, PathBuf>) {
    let by_dir: HashMap<PathBuf, usize> = crates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.dir.clone(), i))
        .collect();
    let ws_at: HashMap<&Path, &Workspace> =
        workspaces.iter().map(|w| (w.dir.as_path(), w)).collect();
    for c in crates.iter_mut() {
        let workspace = c.dir.ancestors().find_map(|d| ws_at.get(d));
        let links = c.deps.iter().filter_map(|d| {
            let dir = match (&d.path, d.workspace) {
                (Some(p), _) => Some(p),
                (None, true) => workspace.and_then(|w| w.deps.get(&d.key)),
                (None, false) => patches.get(&d.package),
            }?;
            Some((*by_dir.get(dir)?, d.dev))
        });
        c.links = links.collect();
    }
}

/// A `[workspace]`: its directory, member/exclude globs, and the paths of
/// its `[workspace.dependencies]`.
struct Workspace {
    dir: PathBuf,
    members: Vec<String>,
    exclude: Vec<String>,
    deps: HashMap<String, PathBuf>,
    /// `[workspace.package] publish`, for `publish.workspace = true`.
    publish: bool,
}

/// Crates the requested paths can see or be seen by: the crates holding
/// (or held by) them, plus everything linked to those through `path`
/// dependencies or a shared `[workspace]`. Unrelated Rust in the same
/// repository (vendored crates, other projects) is left out: a crate can't
/// use another's items without a dependency, so analysing it only costs
/// time and its name mentions hide unused code.
fn related_crates(
    crates: &[Crate],
    workspaces: &[Workspace],
    focus: &[PathBuf],
    focus_only: bool,
) -> Vec<bool> {
    let holds_focus = |c: &Crate| {
        focus
            .iter()
            .any(|f| c.dir.starts_with(f) || f.starts_with(&c.dir))
    };
    if focus_only {
        return crates.iter().map(holds_focus).collect();
    }
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (a, b) = (find(parent, a), find(parent, b));
        parent[a] = b;
    }
    let mut parent: Vec<usize> = (0..crates.len()).collect();
    for (i, c) in crates.iter().enumerate() {
        for &(j, _) in &c.links {
            union(&mut parent, i, j);
        }
    }
    // Members of one workspace see each other. Each crate only checks the
    // workspaces above it: linear even with a workspace per crate.
    let matches = |rel: &Path, globs: &[String]| globs.iter().any(|g| glob_match(g, rel));
    let ws_at: HashMap<&Path, usize> = workspaces
        .iter()
        .enumerate()
        .map(|(w, ws)| (ws.dir.as_path(), w))
        .collect();
    let mut first: Vec<Option<usize>> = vec![None; workspaces.len()];
    for (i, c) in crates.iter().enumerate() {
        for dir in c.dir.ancestors() {
            let Some(&w) = ws_at.get(dir) else { continue };
            let rel = c.dir.strip_prefix(dir).unwrap_or(Path::new(""));
            if matches(rel, &workspaces[w].members) && !matches(rel, &workspaces[w].exclude) {
                match first[w] {
                    Some(f) => union(&mut parent, i, f),
                    None => first[w] = Some(i),
                }
            }
        }
    }
    let mut wanted = HashSet::new();
    for (i, c) in crates.iter().enumerate() {
        if holds_focus(c) {
            wanted.insert(find(&mut parent, i));
        }
    }
    (0..crates.len())
        .map(|i| wanted.contains(&find(&mut parent, i)))
        .collect()
}

/// Cargo's member globs, component by component: `crates/*`, `a/b`, `.`,
/// `crates/**` (any depth).
fn glob_match(glob: &str, rel: &Path) -> bool {
    fn segment(g: &str, r: &str) -> bool {
        match g.split_once('*') {
            Some((pre, post)) => {
                r.len() >= pre.len() + post.len() && r.starts_with(pre) && r.ends_with(post)
            }
            None => g == r,
        }
    }
    fn walk(g: &[&str], r: &[String]) -> bool {
        match g.split_first() {
            None => r.is_empty(),
            Some((&"**", rest)) => (0..=r.len()).any(|k| walk(rest, &r[k..])),
            Some((first, rest)) => !r.is_empty() && segment(first, &r[0]) && walk(rest, &r[1..]),
        }
    }
    let mut glob: Vec<&str> = glob
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    glob.dedup_by(|a, b| *a == "**" && *b == "**");
    glob.truncate(64);
    let rel: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    walk(&glob, &rel)
}

/// The git repository root above `path` (if `git`); without one, the
/// outermost directory with a Cargo.toml; without that, `path` (or its
/// directory). Crates in the same repository see each other even without a
/// workspace manifest.
fn workspace_root(path: &Path, git: bool) -> PathBuf {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let mut best = start.to_path_buf();
    for dir in start.ancestors() {
        if git && dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        if dir.join("Cargo.toml").is_file() {
            best = dir.to_path_buf();
        }
    }
    best
}

/// Files walked: .rs files, Cargo.toml manifests, and what couldn't be
/// read (path, reason).
type Walked = (Vec<PathBuf>, Vec<PathBuf>, Vec<(String, String)>);

/// All .rs files and Cargo.toml manifests under `root`, in parallel (on as
/// many threads as the pool has, which fits address-space limits).
fn walk(root: &Path) -> Walked {
    let found = Mutex::new((Vec::new(), Vec::new(), Vec::new()));
    ignore::WalkBuilder::new(root)
        .require_git(false) // honour .gitignore even outside a git checkout
        .threads(rayon::current_num_threads())
        .filter_entry(|e| !(e.file_type().is_some_and(|t| t.is_dir()) && is_target_dir(e.path())))
        .build_parallel()
        .run(|| {
            let found = &found;
            Box::new(move |entry| {
                let mut found = found.lock().expect("walk lock");
                match entry {
                    Ok(e) if e.file_type().is_some_and(|t| t.is_file()) => {
                        let path = e.into_path();
                        if path.file_name().is_some_and(|n| n == "Cargo.toml") {
                            found.1.push(path);
                        } else if path.extension().is_some_and(|x| x == "rs") {
                            found.0.push(path);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => found.2.push(("(walk)".to_string(), e.to_string())),
                }
                ignore::WalkState::Continue
            })
        });
    found.into_inner().expect("walk lock")
}

/// A cargo build dir: `target/` next to a Cargo.toml, or anything cargo
/// tagged with CACHEDIR.TAG. A source module named `target` is kept.
fn is_target_dir(path: &Path) -> bool {
    path.join("CACHEDIR.TAG").is_file()
        || (path.file_name().is_some_and(|n| n == "target")
            && path
                .parent()
                .is_some_and(|p| p.join("Cargo.toml").is_file()))
}

/// Features that only exist to support tests, judged by their `-`/`_`
/// separated words: `test`, `testing`, `test-utils`, `testdb`, `mock`...
/// but not `testnet` or `attestation`.
fn is_test_feature(name: &str) -> bool {
    name.split(['-', '_']).any(|p| {
        matches!(
            p,
            "test" | "tests" | "testing" | "mock" | "mocks" | "fake" | "fakes"
        ) || p.strip_prefix("test").is_some_and(|rest| {
            matches!(
                rest,
                "db" | "util" | "utils" | "kit" | "support" | "helpers" | "data"
            )
        })
    })
}

#[derive(serde::Deserialize)]
struct Manifest {
    package: Option<Package>,
    #[serde(default)]
    features: HashMap<String, toml::Value>,
    lib: Option<Target>,
    #[serde(default)]
    bin: Vec<Target>,
    #[serde(default)]
    example: Vec<Target>,
    #[serde(default)]
    test: Vec<Target>,
    #[serde(default)]
    bench: Vec<Target>,
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: toml::Table,
    #[serde(default, rename = "build-dependencies")]
    build_dependencies: toml::Table,
    /// `[target.'cfg(..)'.dependencies]` and friends.
    #[serde(default)]
    target: toml::Table,
    workspace: Option<WorkspaceSection>,
    /// `[patch.<registry>] name = { path = .. }`.
    #[serde(default)]
    patch: toml::Table,
    /// `[replace] "name:version" = { path = .. }`.
    #[serde(default)]
    replace: toml::Table,
}

#[derive(serde::Deserialize)]
struct WorkspaceSection {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    dependencies: toml::Table,
    /// `[workspace.package]`: values members inherit.
    package: Option<toml::Table>,
}

/// `publish = false` / `publish = []` mean unpublishable; unset or a
/// registry list mean publishable; `{ workspace = true }` is `None`.
fn publish_setting(v: Option<&toml::Value>) -> Option<bool> {
    match v {
        None => Some(true),
        Some(toml::Value::Boolean(b)) => Some(*b),
        Some(toml::Value::Array(a)) => Some(!a.is_empty()),
        Some(_) => None,
    }
}

/// `{ path = "x" }` -> `dir/x`, canonical.
fn spec_path(spec: &toml::Value, dir: &Path) -> Option<PathBuf> {
    let path = spec.get("path")?.as_str()?;
    dir.join(path).canonicalize().ok()
}

impl Manifest {
    /// Every dependency, from all dependency tables (platform ones too).
    fn dependencies(&self, dir: &Path) -> Vec<Dep> {
        let mut tables = vec![
            (&self.dependencies, false),
            (&self.dev_dependencies, true),
            (&self.build_dependencies, false),
        ];
        for platform in self.target.values().filter_map(toml::Value::as_table) {
            for (key, dev) in [
                ("dependencies", false),
                ("dev-dependencies", true),
                ("build-dependencies", false),
            ] {
                if let Some(t) = platform.get(key).and_then(toml::Value::as_table) {
                    tables.push((t, dev));
                }
            }
        }
        tables
            .into_iter()
            .flat_map(|(t, dev)| t.iter().map(move |(k, spec)| (k, spec, dev)))
            .map(|(key, spec, dev)| Dep {
                key: key.clone(),
                package: spec
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key)
                    .to_string(),
                path: spec_path(spec, dir),
                workspace: spec.get("workspace").and_then(toml::Value::as_bool) == Some(true),
                dev,
            })
            .collect()
    }

    /// Package name -> local directory, from `[patch]` and `[replace]`.
    fn patches(&self, dir: &Path) -> Vec<(String, PathBuf)> {
        let patched = self
            .patch
            .values()
            .filter_map(toml::Value::as_table)
            .flatten()
            .map(|(name, spec)| (name.clone(), spec));
        let replaced = self.replace.iter().map(|(id, spec)| {
            let name = id.split(':').next().unwrap_or(id);
            (name.to_string(), spec)
        });
        patched
            .chain(replaced)
            .filter_map(|(name, spec)| Some((name, spec_path(spec, dir)?)))
            .collect()
    }
}

#[derive(serde::Deserialize)]
struct Package {
    name: String,
    /// `build = "x.rs"` or `build = false`.
    build: Option<toml::Value>,
    /// `publish = false` or a registry list.
    publish: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
struct Target {
    name: Option<String>,
    path: Option<String>,
}

/// What one manifest declares.
struct ManifestInfo {
    /// Its crate with its feature names, if it has a `[package]`.
    krate: Option<(Crate, Vec<String>)>,
    workspace: Option<Workspace>,
    patches: Vec<(String, PathBuf)>,
}

fn read_manifest(path: &Path) -> Option<ManifestInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut m: Manifest = toml::from_str(&text).ok()?;
    let dir = path.parent()?.to_path_buf();
    let workspace = m.workspace.take().map(|w| {
        let mut members = w.members;
        if m.package.is_some() {
            members.push(".".into()); // a root package is a member
        }
        let deps = w
            .dependencies
            .iter()
            .filter_map(|(k, spec)| Some((k.clone(), spec_path(spec, &dir)?)))
            .collect();
        let publish = w.package.as_ref().map(|p| p.get("publish"));
        Workspace {
            dir: dir.clone(),
            members,
            exclude: w.exclude,
            deps,
            publish: publish_setting(publish.flatten()).unwrap_or(true),
        }
    });
    let patches = m.patches(&dir);
    Some(ManifestInfo {
        krate: read_package(m, dir),
        workspace,
        patches,
    })
}

fn read_package(m: Manifest, dir: PathBuf) -> Option<(Crate, Vec<String>)> {
    let deps = m.dependencies(&dir);
    let package = m.package?;
    let canon = |p: PathBuf| p.canonicalize().ok();
    let custom = |targets: &[Target]| -> Vec<PathBuf> {
        targets
            .iter()
            .filter_map(|t| t.path.as_ref().and_then(|p| canon(dir.join(p))))
            .collect()
    };
    let globbed = |sub: &str| -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter_map(|p| {
                if p.extension().is_some_and(|x| x == "rs") {
                    canon(p)
                } else {
                    canon(p.join("main.rs"))
                }
            })
            .collect()
    };
    let lib_path = m
        .lib
        .as_ref()
        .and_then(|l| l.path.as_ref())
        .map_or_else(|| dir.join("src/lib.rs"), |p| dir.join(p));
    let lib_path = canon(lib_path);
    let build_script = match &package.build {
        Some(toml::Value::String(p)) => canon(dir.join(p)),
        Some(toml::Value::Boolean(false)) => None,
        _ => canon(dir.join("build.rs")),
    };
    let mut roots: Vec<PathBuf> = [lib_path.clone(), canon(dir.join("src/main.rs"))]
        .into_iter()
        .flatten()
        .chain(build_script.clone())
        .collect();
    roots.extend(custom(&m.bin));
    roots.extend(custom(&m.example));
    roots.extend(globbed("src/bin"));
    roots.extend(globbed("examples"));
    let mut test_roots = custom(&m.test);
    test_roots.extend(custom(&m.bench));
    test_roots.extend(globbed("tests"));
    test_roots.extend(globbed("benches"));
    let lib_name = m
        .lib
        .as_ref()
        .and_then(|l| l.name.clone())
        .unwrap_or_else(|| package.name.replace('-', "_"));
    let publish = publish_setting(package.publish.as_ref());
    let features = m.features.into_keys().collect();
    Some((
        Crate {
            name: package.name,
            lib_name,
            open_api: false, // decided once every manifest is read
            has_lib: m.lib.is_some() || lib_path.is_some(),
            publish,
            deps,
            links: Vec::new(), // resolved once every manifest is read
            dir,
            roots,
            test_roots,
            build_script,
        },
        features,
    ))
}

/// Reads and analyses one file. Returns, besides the analysis, why it was
/// only partly analysed (lines and name mentions still count).
fn analyse(
    root: &Path,
    path: &Path,
    ctx: &Ctx,
    max_depth: usize,
) -> Result<(FileAnalysis, Option<String>), (String, String)> {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    let bytes = std::fs::read(path).map_err(|e| (rel.clone(), e.to_string()))?;
    let src = String::from_utf8_lossy(&bytes);
    let classified = lines::classify(&src);
    let (mut facts, problem) = if classified.max_depth > max_depth {
        let why = format!(
            "nested {} deep; lines counted, not analysed",
            classified.max_depth
        );
        (fallback_facts(&src), Some(why))
    } else if src.len() > MAX_PARSE_BYTES {
        let why = format!("{} MiB; lines counted, not analysed", src.len() >> 20);
        (fallback_facts(&src), Some(why))
    } else {
        match syn::parse_file(&src) {
            Ok(file) => (visit::collect(&file, ctx), None),
            Err(e) => (fallback_facts(&src), Some(format!("parse error: {e}"))),
        }
    };
    // proc-macro2 keeps every parsed file's source per thread for span
    // lookups; free it once we're done with this file's spans.
    proc_macro2::extra::invalidate_current_thread_spans();
    // `include!("x.inc")` pastes that file here: its names are used here.
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut includes = std::mem::take(&mut facts.literal_includes);
    includes.sort();
    includes.dedup();
    for inc in includes {
        let p = dir.join(inc);
        // Regular files only (not /dev/zero or a FIFO), and not huge ones.
        let readable =
            std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() <= MAX_PARSE_BYTES as u64);
        if readable && let Ok(text) = std::fs::read(&p) {
            count_words(&String::from_utf8_lossy(&text), &mut facts.refs);
        }
    }
    let lines = classified.lines;
    let mut test_mask = vec![false; lines.len()];
    let mut ranges = facts.test_ranges.clone();
    ranges.sort_unstable();
    let mut done = 0; // lines 1..=done are marked
    for (start, end) in ranges {
        for l in start.max(done + 1).max(1)..=end.min(lines.len()) {
            test_mask[l - 1] = true;
        }
        done = done.max(end);
    }
    let file = FileAnalysis {
        path: path.to_path_buf(),
        rel,
        crate_idx: None,
        in_focus: false,
        whole_test: facts.file_test,
        unanalysed: problem.is_some(),
        unresolved_mods: false,
        orphan: false,
        unchecked_test: false,
        lines,
        test_mask,
        facts,
    };
    Ok((file, problem))
}

/// Facts for a file syn can't handle: just its name mentions, so items it
/// uses aren't reported unused. Every word counts, comments and strings
/// included: over-counting only hides unused items, never invents them.
fn fallback_facts(src: &str) -> FileFacts {
    let mut refs = HashMap::new();
    count_words(src, &mut refs);
    FileFacts {
        refs,
        ..Default::default()
    }
}

/// Counts every identifier-like word of `src` into `refs`.
fn count_words(src: &str, refs: &mut HashMap<String, u32>) {
    for word in visit::words(src) {
        *refs.entry(word.to_string()).or_default() += 1;
    }
}

/// `a/b/../c` -> `a/c`, without touching the filesystem (the index holds
/// walked paths, which never contain `..`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Walks each crate's module tree from its roots, following `mod x;`
/// declarations the way rustc does, and marks every file reached only from
/// test code as whole-test. Files no root reaches (dead files, or declared
/// from somewhere we can't see) fall back to their path: under the crate's
/// tests/ or benches/ means test. Their own declarations still propagate.
fn resolve_modules(files: &mut [FileAnalysis], crates: &[Crate]) {
    let index: HashMap<PathBuf, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.clone(), i))
        .collect();
    // Reached from non-test code anywhere wins: such a file is not test code.
    let mut reached: HashMap<usize, bool> = HashMap::new();
    // Files with a `mod x;` whose file wasn't found.
    let mut unresolved = HashSet::new();
    // Reached as test code the recommended check doesn't build (`loom`).
    let mut unchecked = HashSet::new();
    // (file, reached from test code, behaves like mod.rs for its children,
    // that test code is unchecked)
    let mut walk = |queue: &mut VecDeque<(usize, bool, bool, bool)>,
                    reached: &mut HashMap<usize, bool>| {
        while let Some((i, test, mod_rs, off)) = queue.pop_front() {
            let facts = &files[i].facts;
            let off = (test && off) || (facts.file_test && !facts.file_checked);
            let test = test || facts.file_test;
            if test && off {
                unchecked.insert(i);
            }
            if let Some(&prev) = reached.get(&i)
                && (!prev || prev == test)
            {
                continue;
            }
            reached.insert(i, test);
            let path = &files[i].path;
            let Some(dir) = path.parent() else { continue };
            let is_mod_rs = mod_rs || path.file_name().is_some_and(|n| n == "mod.rs");
            let base = if is_mod_rs {
                dir.to_path_buf()
            } else {
                dir.join(path.file_stem().unwrap_or_default())
            };
            for d in &files[i].facts.mod_decls {
                let mut child_dir = base.clone();
                child_dir.extend(&d.inline_path);
                let lookup = |p: PathBuf| index.get(&normalize(&p)).copied();
                let found = match &d.path_attr {
                    // Relative to the file's own directory outside inline modules.
                    Some(p) if d.inline_path.is_empty() => lookup(dir.join(p)).map(|c| (c, true)),
                    Some(p) => lookup(child_dir.join(p)).map(|c| (c, true)),
                    None => lookup(child_dir.join(format!("{}.rs", d.name)))
                        .map(|c| (c, false))
                        .or_else(|| {
                            lookup(child_dir.join(&d.name).join("mod.rs")).map(|c| (c, true))
                        }),
                };
                match found {
                    Some((c, child_mod_rs)) => {
                        let child_off = (test && off) || (d.test && !d.checked);
                        queue.push_back((c, test || d.test, child_mod_rs, child_off));
                    }
                    None => {
                        unresolved.insert(i);
                    }
                }
            }
        }
    };

    let mut queue = VecDeque::new();
    for c in crates {
        for (roots, test) in [(&c.roots, false), (&c.test_roots, true)] {
            for r in roots {
                if let Some(&i) = index.get(r) {
                    queue.push_back((i, test, true, false));
                }
            }
        }
    }
    walk(&mut queue, &mut reached);
    let compiled: HashSet<usize> = reached.keys().copied().collect();

    // Unreached files, shallowest first so a parent seeds its children.
    let mut orphans: Vec<usize> = (0..files.len())
        .filter(|i| !reached.contains_key(i))
        .collect();
    orphans.sort_by_key(|&i| files[i].path.components().count());
    for i in orphans {
        if reached.contains_key(&i) {
            continue;
        }
        let f = &files[i];
        let under_tests = f.crate_idx.is_some_and(|c| {
            let rel = f.path.strip_prefix(&crates[c].dir).unwrap_or(&f.path);
            rel.components()
                .next()
                .is_some_and(|c| matches!(c.as_os_str().to_str(), Some("tests" | "benches")))
        });
        queue.push_back((i, under_tests, false, false));
        walk(&mut queue, &mut reached);
    }

    for (i, f) in files.iter_mut().enumerate() {
        f.whole_test = reached.get(&i).copied().unwrap_or(f.facts.file_test);
        f.unresolved_mods = unresolved.contains(&i);
        f.orphan = !compiled.contains(&i);
        f.unchecked_test = f.whole_test && unchecked.contains(&i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn whole_test(scan: &Scan) -> HashMap<&str, bool> {
        scan.files
            .iter()
            .map(|f| (f.rel.as_str(), f.whole_test))
            .collect()
    }

    #[test]
    fn resolves_test_modules_from_crate_roots() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\" # a comment\n"),
            (
                "src/lib.rs",
                "mod a;\nmod b;\nmod shared;\n#[cfg(test)]\nmod tests;\nmod inner {\n    #[cfg(test)]\n    mod t2;\n}\n#[cfg(test)]\n#[path = \"../support/helpers.rs\"]\nmod helpers;\ncfg_x! {\n    #[cfg(test)]\n    mod in_macro;\n}\n",
            ),
            ("src/a.rs", "#[cfg(test)]\nmod a_tests;\npub fn a() {}\n"),
            ("src/a/a_tests.rs", "fn t() {}\n"),
            ("src/b.rs", "#![cfg(test)]\nfn b() {}\n"),
            ("src/shared.rs", "fn s() {}\n"),
            (
                "src/tests.rs",
                "#[path = \"shared.rs\"]\nmod shared_again;\nmod child;\n",
            ),
            ("src/tests/child.rs", "fn c() {}\n"),
            ("src/inner/t2.rs", "fn t2() {}\n"),
            ("src/in_macro.rs", "fn m() {}\n"),
            ("support/helpers.rs", "fn h() {}\n"),
            ("tests/it.rs", "mod common;\nfn it() {}\n"),
            ("tests/common/mod.rs", "fn common() {}\n"),
            ("tests/data/orphan.rs", "mod sub;\n"),
            ("tests/data/orphan/sub.rs", "fn sub() {}\n"),
            ("src/dead.rs", "fn dead() {}\n"),
        ]);
        let scan = fx.scan();
        let t = whole_test(&scan);
        for (file, expected) in [
            ("src/lib.rs", false),
            ("src/a.rs", false),
            ("src/a/a_tests.rs", true),
            ("src/b.rs", true),
            ("src/shared.rs", false), // reached from non-test code too
            ("src/tests.rs", true),
            ("src/tests/child.rs", true),
            ("src/inner/t2.rs", true),
            ("src/in_macro.rs", true),
            ("support/helpers.rs", true),
            ("tests/it.rs", true),
            ("tests/common/mod.rs", true),
            ("tests/data/orphan.rs", true),
            ("tests/data/orphan/sub.rs", true),
            ("src/dead.rs", false),
        ] {
            assert_eq!(t[file], expected, "{file}");
        }
        assert_eq!(scan.crates[0].name, "m");
    }

    #[test]
    fn detects_test_features_by_word() {
        for f in [
            "test",
            "testing",
            "test-utils",
            "testdb",
            "mock",
            "with_fakes",
        ] {
            assert!(is_test_feature(f), "{f}");
        }
        for f in ["testnet", "attestation", "contest", "latest"] {
            assert!(!is_test_feature(f), "{f}");
        }
    }

    #[test]
    fn feature_gated_code_follows_test_feature_options() {
        let files = [
            (
                "Cargo.toml",
                "[package]\nname = \"m\"\n[features]\ntestdb = []\ntestnet = []\n",
            ),
            (
                "src/lib.rs",
                "#[cfg(feature = \"testdb\")]\nfn a() {}\n#[cfg(feature = \"testnet\")]\nfn b() {}\n",
            ),
        ];
        let fx = Fixture::new(&files);
        let test_lines = |opts: &ScanOptions| {
            let scan = scan(std::slice::from_ref(&fx.root), opts).unwrap();
            let f = &scan.files[0];
            (0..f.lines.len()).filter(|&l| f.is_test_line(l)).count()
        };
        let opts = |on: &[&str], off: &[&str]| ScanOptions {
            test_features: on.iter().map(|s| s.to_string()).collect(),
            no_test_features: off.iter().map(|s| s.to_string()).collect(),
            ..ScanOptions::default()
        };
        assert_eq!(test_lines(&opts(&[], &[])), 2);
        assert_eq!(test_lines(&opts(&["testnet"], &[])), 4);
        assert_eq!(test_lines(&opts(&[], &["testdb"])), 0);
    }

    #[test]
    fn related_crates_in_a_repo_are_scanned_and_focus_narrows() {
        let fx = Fixture::new(&[
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("a/src/lib.rs", "pub fn a() {}\n"),
            // Depends on `a`, so may use its items.
            (
                "b/Cargo.toml",
                "[package]\nname = \"b\"\n[dependencies]\na = { path = \"../a\" }\n",
            ),
            ("b/src/lib.rs", "pub fn b() {}\n"),
            // A workspace: `c` and `d` see each other, `skip` is excluded.
            (
                "ws/Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n",
            ),
            ("ws/crates/c/Cargo.toml", "[package]\nname = \"c\"\n"),
            ("ws/crates/c/src/lib.rs", "pub fn c() {}\n"),
            ("ws/crates/d/Cargo.toml", "[package]\nname = \"d\"\n"),
            ("ws/crates/d/src/lib.rs", "pub fn d() {}\n"),
            ("ws/crates/skip/Cargo.toml", "[package]\nname = \"skip\"\n"),
            ("ws/crates/skip/src/lib.rs", "pub fn skip() {}\n"),
            // Unrelated vendored code mentioning the same names.
            ("vendor/v/Cargo.toml", "[package]\nname = \"v\"\n"),
            ("vendor/v/src/lib.rs", "pub fn a() {}\n"),
        ]);
        let scanned = |path: &str| {
            let scan = scan(&[fx.root.join(path)], &ScanOptions::default()).unwrap();
            assert_eq!(scan.root, fx.root.canonicalize().unwrap());
            scan.files
                .iter()
                .map(|f| (f.rel.clone(), f.in_focus))
                .collect::<Vec<_>>()
        };
        let own = |v: &[(&str, bool)]| -> Vec<(String, bool)> {
            v.iter().map(|(p, f)| (p.to_string(), *f)).collect()
        };
        assert_eq!(
            scanned("a/src"),
            own(&[("a/src/lib.rs", true), ("b/src/lib.rs", false)])
        );
        assert_eq!(
            scanned("ws/crates/c"),
            own(&[
                ("ws/crates/c/src/lib.rs", true),
                ("ws/crates/d/src/lib.rs", false)
            ])
        );
    }

    #[test]
    fn crates_in_ignored_dirs_scan_from_their_own_manifest() {
        let fx = Fixture::new(&[
            (".gitignore", "scratch/\n"),
            (
                "scratch/mycrate/Cargo.toml",
                "[package]\nname = \"mycrate\"\n",
            ),
            ("scratch/mycrate/src/lib.rs", "pub fn f() {}\n"),
        ]);
        let dir = fx.root.join("scratch/mycrate");
        let scan = scan(std::slice::from_ref(&dir), &ScanOptions::default()).unwrap();
        assert_eq!(scan.root, dir.canonicalize().unwrap());
        assert_eq!(scan.files.len(), 1);
    }

    #[test]
    fn scoping_follows_workspace_deps_patches_and_deep_globs() {
        let fx = Fixture::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\"]\n[workspace.dependencies]\ncorelib = { path = \"libs/corelib\" }\n[patch.crates-io]\nfoo = { path = \"vendor/foo\" }\n",
            ),
            ("libs/corelib/Cargo.toml", "[package]\nname = \"corelib\"\n"),
            ("libs/corelib/src/lib.rs", "pub fn c() {}\n"),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\n[dependencies]\ncorelib = { workspace = true }\nfoo = \"1\"\n",
            ),
            ("app/src/main.rs", "fn main() {}\n"),
            ("vendor/foo/Cargo.toml", "[package]\nname = \"foo\"\n"),
            ("vendor/foo/src/lib.rs", "pub fn f() {}\n"),
            (
                "deep/Cargo.toml",
                "[workspace]\nmembers = [\"crates/**\"]\n",
            ),
            ("deep/crates/x/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("deep/crates/x/a/src/lib.rs", "pub fn a() {}\n"),
            ("deep/crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
            ("deep/crates/b/src/lib.rs", "pub fn b() {}\n"),
        ]);
        let rels = |path: &str| {
            let scan = scan(&[fx.root.join(path)], &ScanOptions::default()).unwrap();
            scan.files.iter().map(|f| f.rel.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            rels("libs/corelib"),
            [
                "app/src/main.rs",
                "libs/corelib/src/lib.rs",
                "vendor/foo/src/lib.rs"
            ]
        );
        assert_eq!(
            rels("vendor/foo"),
            [
                "app/src/main.rs",
                "libs/corelib/src/lib.rs",
                "vendor/foo/src/lib.rs"
            ]
        );
        assert_eq!(
            rels("deep/crates/b"),
            ["deep/crates/b/src/lib.rs", "deep/crates/x/a/src/lib.rs"]
        );
    }

    #[test]
    fn relative_paths_climb_out_of_the_cwd() {
        fn p(s: &str) -> &Path {
            Path::new(s)
        }
        assert_eq!(relative_to(p("/r/a/src/x.rs"), p("/r/a")), p("src/x.rs"));
        assert_eq!(
            relative_to(p("/r/b/src/x.rs"), p("/r/a")),
            p("../b/src/x.rs")
        );
        assert_eq!(relative_to(p("/r/x.rs"), p("/r/a/b")), p("../../x.rs"));
    }

    #[test]
    fn refuses_paths_outside_any_project() {
        let dir = std::env::temp_dir().join(format!("bloaty-noproject-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/x.rs"), "fn f() {}\n").unwrap();
        let result = scan(std::slice::from_ref(&dir), &ScanOptions::default());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_paths_in_different_workspaces() {
        let one = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"one\"\n"),
            ("src/lib.rs", ""),
        ]);
        let two = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"two\"\n"),
            ("src/lib.rs", ""),
        ]);
        let err = scan(
            &[one.root.clone(), two.root.clone()],
            &ScanOptions::default(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn skips_cargo_target_dirs_but_not_modules_named_target() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", "mod target;\n"),
            ("src/target/mod.rs", "fn t() {}\n"),
            ("target/debug/build/x/out/gen.rs", "fn gen() {}\n"),
        ]);
        let rels: Vec<_> = fx.scan().files.iter().map(|f| f.rel.clone()).collect();
        assert_eq!(rels, vec!["src/lib.rs", "src/target/mod.rs"]);
    }

    #[test]
    fn unparsable_and_too_deep_files_still_count() {
        let deep = format!("fn f() {}{}\n", "{".repeat(2001), "}".repeat(2001));
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", "mod bad;\nmod deep;\n"),
            (
                "src/bad.rs",
                "fn broken() {\n    let x = gen { helper() };\n}\n",
            ),
            ("src/deep.rs", &deep),
        ]);
        let scan = fx.scan();
        assert_eq!(scan.skipped.len(), 2);
        let bad = scan.files.iter().find(|f| f.rel == "src/bad.rs").unwrap();
        assert!(bad.unanalysed);
        assert_eq!(bad.facts.refs.get("helper"), Some(&1));
    }
}
