//! Dogfood: bloaty's own fns (from the built binary's `metrics` JSON) stay
//! under `BAR` cyclomatic complexity, unless the fn allows (or expects)
//! `clippy::cognitive_complexity`, as bloaty's own `allowed` field reports
//! it, and a plain `//` comment right above that attribute says why.

mod common;

use common::run_json;
use std::path::Path;

/// Cyclomatic complexity a fn of bloaty's must stay under.
const BAR: u64 = 15;

/// Whether the attribute naming `cognitive_complexity` within `lines` (the
/// fn's docs and attributes) sits right under a plain `//` comment.
fn justified(lines: &[&str]) -> bool {
    let Some(at) = lines
        .iter()
        .position(|l| l.contains("cognitive_complexity"))
    else {
        return false;
    };
    // A multi-line attribute starts at the last `#[` at or before it.
    let start = lines[..=at]
        .iter()
        .rposition(|l| l.trim_start().starts_with("#["))
        .unwrap_or(at);
    let above = start.checked_sub(1).map(|i| lines[i].trim_start());
    above.is_some_and(|l| l.starts_with("//") && !l.starts_with("///") && !l.starts_with("//!"))
}

#[test]
fn bloaty_stays_simple() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let v = run_json(dir, &["metrics", "src", "--json"]);
    let fns = v["functions"].as_array().expect("functions");
    assert!(fns.len() > 100, "metrics saw only {} fns", fns.len());
    let mut bad = Vec::new();
    for f in fns {
        let cc = f["cyclomatic_complexity"].as_u64().unwrap();
        if cc < BAR {
            continue;
        }
        let allowed = f["allowed"]
            .as_array()
            .is_some_and(|a| a.iter().any(|l| l == "cognitive_complexity"));
        let src = std::fs::read_to_string(f["path"].as_str().unwrap()).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        let (start, line) = (
            f["start_line"].as_u64().unwrap() as usize,
            f["line"].as_u64().unwrap() as usize,
        );
        if !(allowed && justified(&lines[start - 1..line - 1])) {
            bad.push(format!(
                "{}:{} {}: cyclomatic complexity {cc} >= {BAR}",
                f["file"].as_str().unwrap(),
                line,
                f["name"].as_str().unwrap()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "simplify, or justify with a `//` comment over #[allow(clippy::cognitive_complexity)]:\n{}",
        bad.join("\n")
    );
}

#[test]
fn what_counts_as_justified() {
    let ok = [
        "/// Docs.",
        "// A token table.",
        "#[allow(clippy::cognitive_complexity)]",
    ];
    assert!(justified(&ok));
    let combined = [
        "// Why.",
        "#[expect(clippy::too_many_lines, clippy::cognitive_complexity)]",
    ];
    assert!(justified(&combined));
    let multi = [
        "// Why.",
        "#[allow(",
        "    clippy::cognitive_complexity",
        ")]",
    ];
    assert!(justified(&multi));
    assert!(!justified(&[
        "/// Docs only.",
        "#[allow(clippy::cognitive_complexity)]"
    ]));
    assert!(!justified(&["#[allow(clippy::cognitive_complexity)]"]));
    assert!(!justified(&["// Why.", "#[inline]"]));
}
