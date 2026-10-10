//! Per-fn size, complexity and fragility of non-test code, and their per-file
//! roll-up for `summary --by file`. Raw numbers only: nothing is scored here.

use crate::findings::{Loc, short_name};
use crate::scan::{FileAnalysis, Scan};
use crate::visit::{FnFact, FnMetrics, PanicSite};
use serde::Serialize;
use std::cmp::Reverse;
use std::fmt::Write;
use std::path::Path;

/// Below this many impl lines a file gets no panic density: one `unwrap` in
/// a 10-line file isn't a hotspot.
const MIN_DENSITY_LINES: usize = 30;

/// The measured fns of a file: non-test, in a file bloaty could parse.
fn measured(f: &FileAnalysis) -> impl Iterator<Item = (&FnFact, FnMetrics)> {
    f.facts
        .fns
        .iter()
        .filter(|_| !f.whole_test)
        .filter_map(|func| Some((func, func.metrics?)))
}

#[derive(Serialize, Clone, Debug)]
pub struct FnSize {
    pub name: String,
    pub line: usize,
    pub lines: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct FnPeak {
    pub name: String,
    pub line: usize,
    pub value: u32,
}

/// The fn with the most panic sites in a file, and where they are.
#[derive(Serialize, Clone, Debug)]
pub struct FnPanics {
    #[serde(flatten)]
    pub peak: FnPeak,
    pub sites: Vec<PanicSite>,
}

/// A file's worst fns and its panic sites and clones. Fields about fns are
/// absent when it has no measured fns.
#[derive(Serialize, Clone, Debug, Default)]
pub struct FileMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largest_fn: Option<FnSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_complexity: Option<FnPeak>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_nesting: Option<FnPeak>,
    /// Absent when the file has no panic sites.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_panics: Option<FnPanics>,
    /// Panic sites in non-test fns.
    pub panics: u32,
    /// Panic sites per 100 impl lines; absent below `MIN_DENSITY_LINES`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panic_density: Option<f64>,
    pub clones: u32,
}

/// `None` for whole-test files and files whose syntax wasn't analysed.
pub fn file_metrics(f: &FileAnalysis, impl_lines: usize) -> Option<FileMetrics> {
    if f.whole_test || f.unanalysed {
        return None;
    }
    let mut out = FileMetrics::default();
    // Ties go to the first fn in the file.
    let peak = |slot: &mut Option<FnPeak>, func: &FnFact, value: u32| {
        if slot.as_ref().is_none_or(|p| value > p.value) {
            *slot = Some(FnPeak {
                name: func.name.clone(),
                line: func.line,
                value,
            });
        }
    };
    for (func, m) in measured(f) {
        let lines = f.own_lines(func);
        if out.largest_fn.as_ref().is_none_or(|l| lines > l.lines) {
            out.largest_fn = Some(FnSize {
                name: func.name.clone(),
                line: func.line,
                lines,
            });
        }
        peak(&mut out.max_complexity, func, m.complexity);
        peak(&mut out.max_nesting, func, m.nesting);
        if m.panics > 0
            && out
                .most_panics
                .as_ref()
                .is_none_or(|p| m.panics > p.peak.value)
        {
            out.most_panics = Some(FnPanics {
                peak: FnPeak {
                    name: func.name.clone(),
                    line: func.line,
                    value: m.panics,
                },
                sites: func.panic_sites.clone(),
            });
        }
        out.panics += m.panics;
        out.clones += m.clones;
    }
    out.panic_density = (impl_lines >= MIN_DENSITY_LINES)
        .then(|| (out.panics as f64 * 10000.0 / impl_lines as f64).round() / 100.0);
    Some(out)
}

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
            "complexity",
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
                "complexity",
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
        assert_eq!(lib["max_complexity"]["value"], 2);
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

    #[test]
    fn most_panics_is_the_fn_with_most_and_absent_without() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/lib.rs",
                "fn one(x: Option<u8>) { x.unwrap(); }\nfn two(x: Option<u8>) {\n    x.unwrap();\n    assert!(x.is_some());\n}\nfn also_two(x: Option<u8>) { x.unwrap(); x.expect(\"e\"); }\n",
            ),
            ("src/calm.rs", "fn calm() {}\n"),
        ]);
        let scan = fx.scan();
        let m = |name: &str| {
            let f = scan.files.iter().find(|f| f.rel.ends_with(name)).unwrap();
            file_metrics(f, 6).unwrap()
        };
        let p = m("lib.rs").most_panics.unwrap();
        // Ties go to the first fn.
        assert_eq!(
            (p.peak.name.as_str(), p.peak.line, p.peak.value),
            ("two", 2, 2)
        );
        let kinds: Vec<_> = p.sites.iter().map(|s| (s.line, s.kind.as_str())).collect();
        assert_eq!(kinds, vec![(3, "unwrap"), (4, "assert!")]);
        assert!(m("calm.rs").most_panics.is_none());
    }

    #[test]
    fn panic_density_per_100_impl_lines() {
        let body: String = (0..38).map(|i| format!("    let _x{i} = {i};\n")).collect();
        let src = format!("fn f(x: Option<u8>) {{\n{body}    x.unwrap();\n}}\n");
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        let scan = fx.scan();
        let f = scan
            .files
            .iter()
            .find(|f| f.rel.ends_with("lib.rs"))
            .unwrap();
        let m = file_metrics(f, 40).unwrap();
        assert_eq!(m.panic_density, Some(2.5));
        assert_eq!(file_metrics(f, 29).unwrap().panic_density, None);
    }
}
