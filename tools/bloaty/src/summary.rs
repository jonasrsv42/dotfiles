//! Line counts split into implementation, test, doc and comment, grouped by
//! crate, module (path within the crate, cut at --depth) or file.

use crate::lines::LineClass;
use crate::scan::{FileAnalysis, Scan};
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
    pub files: usize,
    /// Non-test code lines.
    #[serde(rename = "impl")]
    pub implementation: usize,
    pub test: usize,
    pub doc: usize,
    pub comment: usize,
    pub blank: usize,
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
    let focus: Vec<&FileAnalysis> = scan.files.iter().filter(|f| f.in_focus).collect();
    let crates: HashSet<_> = focus.iter().map(|f| f.crate_idx).collect();
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
    for f in focus {
        let krate = scan.crate_name(f).to_string();
        let (key, module) = match by {
            GroupBy::Crate => (krate.clone(), None),
            GroupBy::File => (f.rel.clone(), None),
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
        let _ = writeln!(
            out,
            "{:<width$}  {:>5}  {:>8}  {:>8}  {:>6.0}%  {:>7}  {:>7}",
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
    if !s.test_features.is_empty() {
        let _ = writeln!(
            out,
            "\ncounted as test: cfg(test) and features {}",
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
    }
}
