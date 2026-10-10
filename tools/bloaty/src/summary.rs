//! Line counts split into implementation, test, doc and comment, grouped by
//! crate, module (path within the crate, cut at --depth) or file.

use crate::lines::LineClass;
use crate::metrics::{FileMetrics, file_metrics};
use crate::scan::{FileAnalysis, Scan};
use crate::smell::{self, Level, Smell};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::path::{Component, Path};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    Crate,
    Module,
    File,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    Impl,
    Test,
    Code,
}

#[derive(Serialize, Default, Clone)]
pub struct Counts {
    pub name: String,
    #[serde(rename = "crate", skip_serializing_if = "Option::is_none")]
    pub krate: Option<String>,
    /// Module path within the crate, when grouping by module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Absolute, when grouping by file (`name` is for people).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub files: usize,
    /// Non-test code lines.
    #[serde(rename = "impl")]
    pub implementation: usize,
    pub test: usize,
    pub doc: usize,
    pub comment: usize,
    pub blank: usize,
    /// Worst fns, panic sites and clones, when grouping by file.
    #[serde(flatten)]
    pub metrics: Option<FileMetrics>,
    /// Relative smell within the summarised files, when grouping by file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smell: Option<Smell>,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.files += o.files;
        self.implementation += o.implementation;
        self.test += o.test;
        self.doc += o.doc;
        self.comment += o.comment;
        self.blank += o.blank;
    }

    fn code(&self) -> usize {
        self.implementation + self.test
    }

    fn key(&self, sort: SortBy) -> usize {
        match sort {
            SortBy::Impl => self.implementation,
            SortBy::Test => self.test,
            SortBy::Code => self.code(),
        }
    }
}

pub fn file_counts(f: &FileAnalysis) -> Counts {
    let mut c = Counts {
        files: 1,
        ..Default::default()
    };
    for (i, class) in f.lines.iter().enumerate() {
        match class {
            LineClass::Code if f.is_test_line(i) => c.test += 1,
            LineClass::Code => c.implementation += 1,
            LineClass::Doc => c.doc += 1,
            LineClass::Comment => c.comment += 1,
            LineClass::Blank => c.blank += 1,
        }
    }
    c
}

#[derive(Serialize)]
pub struct Summary {
    pub root: String,
    pub by: GroupBy,
    pub depth: usize,
    pub sort: SortBy,
    /// Groups not shown because of --top.
    pub hidden: usize,
    pub test_features: Vec<String>,
    pub groups: Vec<Counts>,
    pub total: Counts,
}

/// Grouping defaults to crate for workspaces, module for a single crate.
pub fn summary(
    scan: &Scan,
    by: Option<GroupBy>,
    depth: usize,
    sort: SortBy,
    top: usize,
) -> Summary {
    let focus: Vec<(usize, &FileAnalysis)> = scan
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.in_focus)
        .collect();
    let crates: HashSet<_> = focus.iter().map(|(_, f)| f.crate_idx).collect();
    let by = by.unwrap_or(if crates.len() > 1 {
        GroupBy::Crate
    } else {
        GroupBy::Module
    });

    let mut groups: BTreeMap<String, Counts> = BTreeMap::new();
    let mut total = Counts {
        name: "total".into(),
        ..Default::default()
    };
    // Scored against every scanned file, before --top, so neither PATH nor
    // --top changes a file's level.
    let mut smells = if by == GroupBy::File {
        smell::for_scan(scan)
    } else {
        Vec::new()
    };
    for (i, f) in focus {
        let krate = scan.crate_name(f).to_string();
        let (key, module) = match by {
            GroupBy::Crate => (krate.clone(), None),
            GroupBy::File => (scan.display_path(&f.path), None),
            GroupBy::Module => {
                let module = module_key(f, scan, depth);
                let key = if crates.len() > 1 {
                    format!("{krate}:{module}")
                } else {
                    module.clone()
                };
                (key, Some(module))
            }
        };
        let c = file_counts(f);
        total.add(&c);
        groups
            .entry(key.clone())
            .or_insert_with(|| Counts {
                name: key,
                krate: Some(krate),
                module,
                path: (by == GroupBy::File).then(|| f.path.display().to_string()),
                metrics: (by == GroupBy::File)
                    .then(|| file_metrics(f, c.implementation))
                    .flatten(),
                smell: smells.get_mut(i).and_then(Option::take),
                ..Default::default()
            })
            .add(&c);
    }
    let mut rows: Vec<Counts> = groups.into_values().collect();
    rows.sort_by(|a, b| {
        b.key(sort)
            .cmp(&a.key(sort))
            .then_with(|| a.name.cmp(&b.name))
    });
    let shown = if top == 0 {
        rows.len()
    } else {
        top.min(rows.len())
    };
    let hidden = rows.len() - shown;
    rows.truncate(shown);
    Summary {
        root: scan.root.display().to_string(),
        by,
        depth,
        sort,
        hidden,
        test_features: scan.test_features.clone(),
        groups: rows,
        total,
    }
}

/// The file's module path within its crate, cut to `depth` components:
/// `src/node/biunion.rs` and `src/node/biunion/x.rs` are both `node/biunion`
/// at depth 2. The `src/` prefix is dropped; tests/, benches/, examples/ stay.
fn module_key(f: &FileAnalysis, scan: &Scan, depth: usize) -> String {
    let base = f
        .crate_idx
        .map_or(scan.root.as_path(), |i| scan.crates[i].dir.as_path());
    let rel = f.path.strip_prefix(base).unwrap_or(Path::new(&f.rel));
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.first().is_some_and(|p| p == "src") && parts.len() > 1 {
        parts.remove(0);
    }
    if let Some(last) = parts.pop() {
        match last.strip_suffix(".rs") {
            Some("mod") => {}
            Some(stem) => parts.push(stem.to_string()),
            None => parts.push(last),
        }
    }
    parts.truncate(depth.max(1));
    parts.join("/")
}

pub fn render(s: &Summary) -> String {
    let mut out = String::new();
    let width = s
        .groups
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max(5);
    let row = |out: &mut String, r: &Counts| {
        let pct = if r.code() == 0 {
            0.0
        } else {
            100.0 * r.test as f64 / r.code() as f64
        };
        let mark = match r.smell.as_ref().and_then(|s| s.level) {
            Some(Level::Hot) => "  !!",
            Some(Level::Warm) => "  !",
            None => "",
        };
        let _ = writeln!(
            out,
            "{:<width$}  {:>5}  {:>8}  {:>8}  {:>6.0}%  {:>7}  {:>7}{mark}",
            r.name, r.files, r.implementation, r.test, pct, r.doc, r.comment
        );
    };
    let _ = writeln!(
        out,
        "{:<width$}  {:>5}  {:>8}  {:>8}  {:>7}  {:>7}  {:>7}",
        "", "files", "impl", "test", "test%", "doc", "comment"
    );
    for r in &s.groups {
        row(&mut out, r);
    }
    if s.hidden > 0 {
        let _ = writeln!(out, "… {} more (--top)", s.hidden);
    }
    let _ = writeln!(out, "{}", "-".repeat(width + 54));
    row(&mut out, &s.total);
    if s.groups
        .iter()
        .any(|g| g.smell.as_ref().is_some_and(|s| s.level.is_some()))
    {
        let _ = writeln!(
            out,
            "\n!! hot, ! warm: the project's largest, most complex or most panicky files (--json says why)"
        );
    }
    if !s.test_features.is_empty() {
        let _ = writeln!(
            out,
            "\ncounted as test: cfg(test), loom, fuzzing and features {}",
            s.test_features.join(", ")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn groups_modules_without_src_prefix() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/lib.rs",
                "mod node;\n#[cfg(test)]\nmod t {\n    fn x() {}\n}\n",
            ),
            ("src/node/mod.rs", "mod biunion;\nfn n() {}\n"),
            ("src/node/biunion.rs", "mod x;\nfn a() {}\n"),
            ("src/node/biunion/x.rs", "fn b() {}\nfn c() {}\n"),
        ]);
        let s = summary(&fx.scan(), None, 2, SortBy::Impl, 0);
        assert_eq!(s.by, GroupBy::Module);
        let rows: Vec<_> = s
            .groups
            .iter()
            .map(|g| (g.name.as_str(), g.implementation, g.test))
            .collect();
        assert_eq!(
            rows,
            vec![("node/biunion", 4, 0), ("node", 2, 0), ("lib", 1, 4)]
        );
        let s = summary(&fx.scan(), None, 2, SortBy::Impl, 1);
        assert_eq!((s.groups.len(), s.hidden), (1, 2));
        // File groups carry an absolute path for tools.
        let s = summary(&fx.scan(), Some(GroupBy::File), 2, SortBy::Impl, 0);
        for g in &s.groups {
            let path = std::path::Path::new(g.path.as_deref().unwrap());
            assert!(path.is_absolute() && path.is_file(), "{path:?}");
        }
    }

    /// The smell fields the Neovim tree relies on.
    #[test]
    fn file_groups_carry_a_smell() {
        let mut files = vec![(
            "Cargo.toml".to_string(),
            "[package]\nname = \"m\"\n".to_string(),
        )];
        let mut lib = String::new();
        for i in 0..12 {
            lib.push_str(&format!("mod m{i};\n"));
            let body: String = (0..10 + i)
                .map(|k| format!("    let _x{k} = {k};\n"))
                .collect();
            files.push((format!("src/m{i}.rs"), format!("fn f{i}() {{\n{body}}}\n")));
        }
        let mut big = String::from("fn big(x: Option<u8>) -> u8 {\n");
        for k in 0..60 {
            big.push_str(&format!("    if x == Some({k}) {{ return x.unwrap(); }}\n"));
        }
        big.push_str("    0\n}\n");
        lib.push_str("mod big;\nmod tiny;\n");
        files.push(("src/big.rs".into(), big));
        files.push(("src/tiny.rs".into(), "fn t() {}\n".into()));
        // Support code: measured, never scored, however bad.
        let worse = files
            .iter()
            .find(|f| f.0 == "src/big.rs")
            .unwrap()
            .1
            .replace("big", "worse");
        files.push(("build.rs".into(), worse.replace("fn worse(", "fn main2(")));
        files.push((
            "examples/ex.rs".into(),
            worse.clone() + &worse.replace("worse", "again"),
        ));
        files.push(("src/lib.rs".into(), lib));
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let fx = Fixture::new(&refs);
        // The two support files are larger; --top cuts after scoring.
        let s = summary(&fx.scan(), Some(GroupBy::File), 2, SortBy::Impl, 3);
        let v = serde_json::to_value(&s).unwrap();
        let g = &v["groups"][2];
        assert!(g["name"].as_str().unwrap().ends_with("big.rs"));
        let smell = &g["smell"];
        assert_eq!(smell["level"], "hot");
        assert_eq!(smell["front"], 1);
        assert_eq!(
            [&smell["size"], &smell["complexity"], &smell["fragility"]],
            [1.0, 1.0, 0.9]
        );
        assert!(
            smell["why"]
                .as_str()
                .unwrap()
                .starts_with("size: largest fn")
        );
        assert!(render(&s).lines().nth(3).unwrap().ends_with("  !!"));
        // Too small to score; not scored at all for other groupings.
        let s = summary(&fx.scan(), Some(GroupBy::File), 2, SortBy::Impl, 0);
        let v = serde_json::to_value(&s).unwrap();
        let groups = v["groups"].as_array().unwrap();
        let tiny = groups
            .iter()
            .find(|g| g["name"].as_str().unwrap().ends_with("tiny.rs"));
        assert!(tiny.unwrap().get("smell").is_none());
        let small = groups
            .iter()
            .find(|g| g["name"].as_str().unwrap().ends_with("m0.rs"));
        assert!(small.unwrap()["smell"]["level"].is_null());
        assert!(small.unwrap()["smell"].get("why").is_none());
        for name in ["build.rs", "ex.rs"] {
            let g = groups
                .iter()
                .find(|g| g["name"].as_str().unwrap().ends_with(name));
            let g = g.unwrap_or_else(|| panic!("{name}"));
            assert!(
                g.get("smell").is_none() && g["panics"].as_u64() > Some(0),
                "{name}"
            );
        }
        let s = summary(&fx.scan(), Some(GroupBy::Module), 2, SortBy::Impl, 0);
        assert!(s.groups.iter().all(|g| g.smell.is_none()));
    }

    /// Smell ranks against the whole linked workspace, so asking about a
    /// crate or a single file gives a file the same score.
    #[test]
    fn smell_does_not_depend_on_path() {
        let long_fn = |name: &str, n: usize| {
            let body: String = (0..n).map(|k| format!("    let _x{k} = {k};\n")).collect();
            format!("pub fn {name}() {{\n{body}}}\n")
        };
        let mut files = vec![
            (
                "Cargo.toml".to_string(),
                "[workspace]\nmembers = [\"a\", \"b\"]\n".to_string(),
            ),
            ("a/Cargo.toml".into(), "[package]\nname = \"a\"\n".into()),
            ("a/src/lib.rs".into(), "mod x;\n".into()),
            ("a/src/x.rs".into(), long_fn("x", 60)),
            ("b/Cargo.toml".into(), "[package]\nname = \"b\"\n".into()),
            ("b/src/worse.rs".into(), long_fn("worse", 90)),
        ];
        let mut lib = String::from("mod worse;\n");
        for i in 0..20 {
            lib.push_str(&format!("mod m{i};\n"));
            files.push((format!("b/src/m{i}.rs"), long_fn(&format!("f{i}"), 10 + i)));
        }
        files.push(("b/src/lib.rs".into(), lib));
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let fx = Fixture::new(&refs);
        let smell_of_x = |path: std::path::PathBuf| {
            let scan = crate::scan::scan(&[path], &Default::default()).unwrap();
            let s = summary(&scan, Some(GroupBy::File), 2, SortBy::Impl, 0);
            let g = s.groups.iter().find(|g| g.name.ends_with("x.rs")).unwrap();
            g.smell.clone().unwrap()
        };
        let whole = smell_of_x(fx.root.clone());
        assert_eq!(whole.level, Some(Level::Hot));
        // b's larger file counts too: x is not the top.
        assert!(whole.size < 1.0);
        assert_eq!(smell_of_x(fx.root.join("a")), whole);
        assert_eq!(smell_of_x(fx.root.join("a/src/x.rs")), whole);
    }
}
