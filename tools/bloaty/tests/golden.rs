//! Golden tests: the `--json` output of `summary --by file`, `findings` and
//! `metrics` on the fixture workspace in tests/fixtures/ws, compared to
//! tests/golden/*.json. `UPDATE_GOLDEN=1 cargo test --test golden` rewrites
//! them; review the diff like code.
//!
//! The fixture is copied to a temporary directory first: in place, the
//! repository's tests/fixtures/.bloatyignore hides it (so it doesn't skew
//! bloaty's view of itself), and with an empty `.git` of its own its root is
//! the fixture itself, whatever repository or Cargo.toml surrounds the temp
//! dir. Absolute paths become `<ws>`, so the files are the same on
//! every machine.
//!
//! Fixture roles (fxcore unless noted): hot.rs (long, branchy, deeply nested
//! fn: hot by size and complexity, a fn that is genuinely long),
//! fragile.rs (dense panics: hot by fragility), routes.rs (a table: noted),
//! allowed.rs (`#[allow(clippy::too_many_lines)]`: noted), dispatch.rs (a
//! wide dispatch whose arms repeat a prelude: noted; and a small clean
//! one), badarm.rs (a dispatch with one bad arm: warm, pointing at the
//! arm), dup_a.rs + fxapp's dup_b.rs (duplicate across crates), shapes.rs
//! (single-impl trait, unused fn), long.rs / branchy.rs / f01..f20.rs
//! (ordinary code, so ranks mean something: warm needs >= 20 scored files,
//! and 4 warm slots >= 31), lib.rs (`#[cfg(test)]` module, a `#[path]` test
//! module with a child, a module declared from test and impl code),
//! shared.rs (clones), tests/it.rs (whole-test file), build.rs and fxapp's
//! examples/ (support: measured, never scored).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "target" || name == "Cargo.lock" {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(&name));
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// A fresh copy of the fixture, removed on drop.
struct Ws(PathBuf);

impl Ws {
    /// `tag` keeps the tests, which run in parallel, apart.
    fn new(tag: &str) -> Self {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = std::env::temp_dir().join(format!("bloaty-golden-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_dir(&manifest.join("tests/fixtures/ws"), &dir.join("ws"));
        // Its own repository: a git checkout or Cargo.toml around the temp
        // dir can't move the workspace root.
        std::fs::create_dir_all(dir.join("ws/.git")).unwrap();
        Ws(dir.join("ws").canonicalize().unwrap())
    }

    /// `bloaty args` in the copy, its JSON with the copy's path as `<ws>`.
    fn run(&self, args: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_bloaty"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(
            out.status.success(),
            "bloaty {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ws = self.0.to_str().unwrap();
        serde_json::from_str(&stdout.replace(ws, "<ws>")).unwrap()
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

/// Compares `v` to tests/golden/`name`.json, or rewrites it.
fn golden(name: &str, v: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(v).unwrap() + "\n";
    if std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (UPDATE_GOLDEN=1 writes it)", path.display()));
    if want != text {
        let line = want
            .lines()
            .zip(text.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(want.lines().count().min(text.lines().count()));
        panic!(
            "{} differs from the output at line {}:\n  want: {}\n  got:  {}\n(UPDATE_GOLDEN=1 cargo test --test golden rewrites it; review the diff)",
            path.display(),
            line + 1,
            want.lines().nth(line).unwrap_or("<end>"),
            text.lines().nth(line).unwrap_or("<end>"),
        );
    }
}

fn group<'a>(summary: &'a Value, name: &str) -> &'a Value {
    summary["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == name)
        .unwrap_or_else(|| panic!("no group {name}"))
}

#[test]
fn summary_by_file() {
    let v = Ws::new("summary").run(&["summary", ".", "--by", "file", "--json"]);
    assert_eq!(v["schema"], 3);
    // What the fixture is for: each role still plays out.
    let level = |name: &str| -> Value { group(&v, name)["smell"]["level"].clone() };
    assert_eq!(level("core/src/hot.rs"), "hot");
    assert_eq!(level("core/src/fragile.rs"), "hot");
    assert_eq!(level("core/src/badarm.rs"), "warm");
    for noted in ["routes.rs", "allowed.rs", "dispatch.rs"] {
        assert_eq!(level(&format!("core/src/{noted}")), "noted", "{noted}");
    }
    for plain in ["f01.rs", "long.rs", "branchy.rs", "shared.rs"] {
        assert_eq!(level(&format!("core/src/{plain}")), Value::Null, "{plain}");
    }
    let dims = |name: &str| -> Value { group(&v, name)["smell"]["dims"].clone() };
    // The bad arm is the culprit, by name and line.
    let arm = &dims("core/src/badarm.rs")[2];
    assert_eq!(arm["metric"], "cyclomatic_complexity");
    assert_eq!(arm["arm"]["name"], "Op::Rebalance");
    assert!(arm.get("excused").is_none());
    let excused = |name, i: usize| dims(name)[i]["excused"].clone();
    assert_eq!(excused("core/src/routes.rs", 0)["kind"], "table");
    assert_eq!(excused("core/src/allowed.rs", 0)["lint"], "too_many_lines");
    let d = excused("core/src/dispatch.rs", 0);
    assert_eq!((&d["kind"], &d["arms"]), (&"dispatch".into(), &6.into()));
    assert_eq!(d["repeat"]["at"].as_array().unwrap().len(), 4);
    let metrics: Vec<&str> = group(&v, "core/src/hot.rs")["smell"]["dims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["metric"].as_str().unwrap())
        .collect();
    assert_eq!(
        metrics,
        [
            "largest_fn",
            "impl_lines",
            "cyclomatic_complexity",
            "nesting"
        ]
    );
    assert_eq!(
        group(&v, "core/src/fragile.rs")["smell"]["dims"][0]["metric"],
        "panic_density"
    );
    for support in ["core/build.rs", "app/examples/demo.rs", "core/tests/it.rs"] {
        assert!(group(&v, support).get("smell").is_none(), "{support}");
    }
    golden("summary", &v);
}

#[test]
fn findings() {
    let v = Ws::new("findings").run(&["findings", ".", "--json", "--top", "0"]);
    assert_eq!(v["schema"], 3);
    let kinds: Vec<&str> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    for kind in ["duplicate", "unused", "single_impl_trait", "long_fn"] {
        assert!(kinds.contains(&kind), "no {kind} in {kinds:?}");
    }
    golden("findings", &v);
}

#[test]
fn metrics() {
    let v = Ws::new("metrics").run(&["metrics", ".", "--json"]);
    assert_eq!(v["schema"], 3);
    let sites: Vec<&str> = v["functions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["panic_sites"].as_array().unwrap())
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    for kind in ["unwrap", "expect", "panic!", "assert!"] {
        assert!(sites.contains(&kind), "no {kind} in {sites:?}");
    }
    let shape = |name: &str| {
        let f = v["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("no fn {name}"));
        (f["shape"]["kind"].clone(), f["allowed"].clone())
    };
    assert_eq!(shape("routes").0, "table");
    assert_eq!(shape("run").0, "dispatch");
    assert_eq!(shape("weight").0, "dispatch");
    assert_eq!(shape("apply").0, "dispatch");
    assert_eq!(shape("report").1, serde_json::json!(["too_many_lines"]));
    assert_eq!(shape("plan"), (Value::Null, Value::Null));
    golden("metrics", &v);
}
