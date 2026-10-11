//! `bloaty metrics`: every measured fn with its numbers, shape and panic
//! sites.

use crate::fnmetrics::{FnMetrics, PanicSite};
use crate::measure::Measure;
use crate::names::{Loc, short_name};
use crate::rollup::{blamed_arm, counted, measured};
use crate::scan::Scan;
use crate::shape::{Lint, Repeat, Shape};
use crate::visit::FnFact;
use serde::Serialize;
use std::cmp::Reverse;
use std::fmt::Write;
use std::path::Path;

#[derive(Serialize)]
pub struct FnRow {
    #[serde(flatten)]
    pub at: Loc,
    /// Own code lines (nested items excluded).
    pub lines: usize,
    #[serde(flatten)]
    pub metrics: FnMetrics,
    /// As many as `metrics.panics`, in source order.
    pub panic_sites: Vec<PanicSite>,
    /// Set for tables and dispatches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<ShapeRow>,
    /// Allowed clippy lints that excuse a smell dimension.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<Lint>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShapeRow {
    Table {
        entries: usize,
        /// The literal's code lines (excused).
        lines: usize,
    },
    Dispatch {
        arms: usize,
        /// The fn without the arms: lets, scrutinee, base complexity.
        remainder: Measures,
        /// The arms that aren't small (and so count): how many, their
        /// summed lines and cyclomatic complexity.
        counted: Counted,
        /// The arm the score blames by each measure: the worst counted arm
        /// by lines and cyclomatic complexity, the deepest of all by
        /// nesting; absent when none counts.
        worst: WorstArms,
        #[serde(skip_serializing_if = "Option::is_none")]
        repeat: Option<Repeat>,
    },
}

#[derive(Serialize)]
pub struct Measures {
    pub lines: usize,
    #[serde(rename = "cyclomatic_complexity")]
    pub complexity: u32,
    pub nesting: u32,
}

#[derive(Serialize)]
pub struct ArmValue {
    pub name: String,
    pub line: usize,
    pub value: u32,
}

#[derive(Serialize)]
pub struct Counted {
    pub arms: usize,
    pub lines: u32,
    pub cyclomatic_complexity: u32,
}

#[derive(Serialize)]
pub struct WorstArms {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<ArmValue>,
    #[serde(
        rename = "cyclomatic_complexity",
        skip_serializing_if = "Option::is_none"
    )]
    pub complexity: Option<ArmValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nesting: Option<ArmValue>,
}

fn shape_row(own_lines: usize, func: &FnFact) -> Option<ShapeRow> {
    Some(match func.shape.as_ref()? {
        Shape::Table(t) => ShapeRow::Table {
            entries: t.entries,
            lines: t.lines,
        },
        Shape::Dispatch(d) => {
            let w = |x: Measure| {
                blamed_arm(d, x).map(|(value, a)| ArmValue {
                    name: a.name.clone(),
                    line: a.line,
                    value,
                })
            };
            let (arms, lines, cyclomatic_complexity) = counted(d);
            ShapeRow::Dispatch {
                arms: d.arms.len(),
                remainder: Measures {
                    lines: own_lines.saturating_sub(d.lines),
                    complexity: d.remainder.complexity,
                    nesting: d.remainder.nesting,
                },
                counted: Counted {
                    arms,
                    lines,
                    cyclomatic_complexity,
                },
                worst: WorstArms {
                    lines: w(Measure::Lines),
                    complexity: w(Measure::CyclomaticComplexity),
                    nesting: w(Measure::Nesting),
                },
                repeat: d.repeat.clone(),
            }
        }
    })
}

#[derive(Serialize)]
pub struct Report {
    pub root: String,
    /// Every measured fn in the focus files, by file and line.
    pub functions: Vec<FnRow>,
}

pub fn report(scan: &Scan) -> Report {
    let mut functions: Vec<FnRow> = scan
        .files
        .iter()
        .filter(|f| f.in_focus)
        .flat_map(|f| {
            measured(f).map(move |(func, metrics)| FnRow {
                at: Loc::of_fn(scan, f, func),
                lines: f.own_lines(func),
                metrics,
                panic_sites: func.panic_sites.clone(),
                shape: shape_row(f.own_lines(func), func),
                allowed: func.allowed.clone(),
            })
        })
        .collect();
    functions.sort_by(|a, b| (&a.at.file, a.at.line).cmp(&(&b.at.file, b.at.line)));
    Report {
        root: scan.root.display().to_string(),
        functions,
    }
}

/// The `top` most complex fns (0: all), most complex first.
pub fn render(r: &Report, scan: &Scan, top: usize) -> String {
    let mut rows: Vec<&FnRow> = r.functions.iter().collect();
    rows.sort_by_key(|f| (Reverse(f.metrics.complexity), Reverse(f.lines)));
    let shown = if top == 0 {
        rows.len()
    } else {
        top.min(rows.len())
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:>4}  {:>4}  {:>6}  {:>6}  {:>6}  {:>5}  at  fn",
        "cc", "nest", "params", "panics", "clones", "lines"
    );
    for f in &rows[..shown] {
        let m = &f.metrics;
        let _ = writeln!(
            out,
            "{:>4}  {:>4}  {:>6}  {:>6}  {:>6}  {:>5}  {}:{}  {}",
            m.complexity,
            m.nesting,
            m.params,
            m.panics,
            m.clones,
            f.lines,
            scan.display_path(Path::new(&f.at.path)),
            f.at.line,
            short_name(&f.at.name)
        );
        let tags: Vec<String> = f
            .shape
            .iter()
            .map(|s| match s {
                ShapeRow::Table { entries, .. } => format!("table · {entries} entries"),
                ShapeRow::Dispatch { arms, .. } => format!("dispatch · {arms} arms"),
            })
            .chain(f.allowed.iter().map(|l| format!("allows {}", l.as_str())))
            .collect();
        if !tags.is_empty() {
            out.pop();
            let _ = writeln!(out, "  [{}]", tags.join("; "));
        }
    }
    if shown < rows.len() {
        let _ = writeln!(out, "… {} more (--top)", rows.len() - shown);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::{GroupBy, SortBy, summary};
    use crate::testutil::Fixture;

    const LIB: &str = "pub fn a(x: Option<u32>) -> u32 {\n    if x.is_some() {\n        x.unwrap()\n    } else {\n        0\n    }\n}\nfn b(v: &Vec<u8>) -> Vec<u8> {\n    v.clone()\n}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        None::<u8>.unwrap();\n    }\n}\n";

    /// The fields the Neovim views rely on.
    #[test]
    fn json_contract() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", LIB),
            ("tests/it.rs", "fn helper() { None::<u8>.unwrap(); }\n"),
        ]);
        let scan = fx.scan();
        let v = serde_json::to_value(report(&scan)).unwrap();
        assert!(v["root"].is_string());
        let fns = v["functions"].as_array().unwrap();
        // Test fns, in test modules or test files, are left out.
        let names: Vec<_> = fns.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["a", "b"]);
        let a = &fns[0];
        for key in [
            "name",
            "file",
            "path",
            "start_line",
            "line",
            "end_line",
            "lines",
            "cyclomatic_complexity",
            "nesting",
            "params",
            "panics",
            "clones",
        ] {
            assert!(a.get(key).is_some(), "fn.{key}");
        }
        assert!(Path::new(a["path"].as_str().unwrap()).is_absolute());
        let n = |f: &serde_json::Value, k: &str| f[k].as_u64().unwrap();
        assert_eq!(
            [
                "lines",
                "cyclomatic_complexity",
                "nesting",
                "params",
                "panics",
                "clones"
            ]
            .map(|k| n(a, k)),
            [7, 2, 1, 1, 1, 0]
        );
        assert_eq!(n(&fns[1], "clones"), 1);
        assert_eq!(
            a["panic_sites"],
            serde_json::json!([{"line": 3, "col": 11, "kind": "unwrap"}])
        );
        assert_eq!(fns[1]["panic_sites"], serde_json::json!([]));

        let s = summary(&scan, Some(GroupBy::File), 2, SortBy::Impl, 0);
        let v = serde_json::to_value(&s).unwrap();
        let lib = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"].as_str().unwrap().ends_with("lib.rs"))
            .unwrap();
        assert_eq!(lib["largest_fn"]["name"], "a");
        assert_eq!(lib["largest_fn"]["lines"], 7);
        assert_eq!(lib["max_cyclomatic_complexity"]["value"], 2);
        assert_eq!(lib["max_nesting"]["line"], 1);
        assert_eq!(lib["panics"], 1);
        assert_eq!(lib["clones"], 1);
        assert_eq!(lib["most_panics"]["name"], "a");
        assert_eq!(lib["most_panics"]["line"], 1);
        assert_eq!(lib["most_panics"]["value"], 1);
        assert_eq!(lib["most_panics"]["sites"][0]["line"], 3);
        // Too small for a density.
        assert!(lib.get("panic_density").is_none());
        assert!(lib["impl"].is_u64());
        // Whole-test files carry no metrics; other groupings neither.
        let it = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"].as_str().unwrap().ends_with("it.rs"));
        assert!(it.is_none_or(|g| g.get("panics").is_none()));
        let s = summary(&scan, Some(GroupBy::Crate), 2, SortBy::Impl, 0);
        let v = serde_json::to_value(&s).unwrap();
        assert!(v["groups"][0].get("panics").is_none());
    }

    #[test]
    fn render_puts_most_complex_first_and_cuts_at_top() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/lib.rs",
                "fn simple() {}\nfn branchy(x: u8) -> u8 {\n    if x > 1 { 1 } else if x > 2 { 2 } else { 3 }\n}\nstruct Node<A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z>(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);\nimpl<A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z> Node<A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z> {\n    fn get(&self) -> u8 { if true { 1 } else { 0 } }\n}\n",
            ),
        ]);
        let scan = fx.scan();
        let r = report(&scan);
        let text = render(&r, &scan, 2);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[1].contains("lib.rs:2  branchy"), "{text}");
        assert!(lines[1].trim_start().starts_with("3 "), "{text}");
        assert!(lines[2].contains("lib.rs:7  Node<…>::get"), "{text}");
        assert_eq!(lines[3], "… 1 more (--top)");
        assert_eq!(render(&r, &scan, 0).lines().count(), 4);
    }
}
