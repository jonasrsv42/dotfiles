//! Consistency checks across bloaty's three JSON outputs, on the fixture
//! workspace and on every directory in `BLOATY_CORPUS` (colon-separated,
//! e.g. a big workspace of yours plus a few crates under
//! ~/.cargo/registry/src/*/; skipped with a note when unset):
//!
//!   BLOATY_CORPUS=~/dev/proj:$HOME/.cargo/registry/src/index.crates.io-*/syn-2.0.104 \
//!     cargo test --test invariants -- --nocapture
//!
//! What must hold whatever the code looks like (see ARCHITECTURE.md,
//! "Invariants"):
//! - every command exits 0 and prints `schema` == SCHEMA (src/main.rs),
//!   which lua/mbloaty/init.lua's SCHEMA equals;
//! - a levelled group has `why` and non-empty `dims`, an unlevelled one
//!   neither; each dim's `dim` matches its `metric`;
//! - fn dims name a measured fn of the file; their value is at most the
//!   fn's own number and the group's raw peak; excused dims carry the fn's
//!   own number, and the excuse matches `metrics` (table / dispatch shape,
//!   allowed lint) and the measure it may cover;
//! - `culprit` is set exactly on real fn dims at or past the bad-arm bar
//!   and on panic density dims;
//! - panics are never excused, tables only for largest_fn;
//! - a noted group has only excused dims, a hot or warm one a real one
//!   and a culprit (the lens always has something to point at), a hot one
//!   a culprit that isn't `flat` (long flat fns alone cap at warm); `flat`
//!   marks exactly the culprit largest_fn dims with nothing nested
//!   (worst_increment <= FLAT_MAX_INCREMENT), and a flat one never hides a
//!   culprit-sized fn with something nested;
//!   excused dims are at or past the culprit bar;
//! - a dim blaming a dispatch arm names `metrics`' `shape.worst` for it;
//! - `metrics` panics == panic_sites; a fn's worst_increment lies in it
//!   and is at most its cognitive complexity (absent only at 0); the group's panics are its fns' sum,
//!   `most_panics` is the first fn with the most, with its sites; the
//!   group's largest_fn / max_* are its fns' first maxima;
//! - scoring a subdirectory or a single file gives each file the smell the
//!   whole workspace gives it (PATH-independence).
//! - a duplicate has >= 2 distinct copies, the first at the finding; its
//!   `rule` is known and its `tier` the rule's (notes: the copies'
//!   signatures differ, `differs` says how, or their cfgs do; findings:
//!   one signature); cfg rules only within one crate; every copy has at
//!   least min(--min-dup, 7) lines, the finding its largest's; a same-shape
//!   finding has two impl copies, `test_shape` at most one; a test-only
//!   identical group with one copy per crate is `test_crates` exactly when
//!   its smallest copy is under 15 lines; the excluded counts
//!   name known criteria; findings and notes are counted apart.

mod common;

use common::{Ws, run_json};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The number after `prefix` on the line starting with it in `file`
/// (relative to the crate).
fn constant(file: &str, prefix: &str) -> u64 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}` in {file}"));
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap()
}

/// `FLAT_MAX_INCREMENT` in src/measure.rs.
fn flat_max_increment() -> u64 {
    constant("src/measure.rs", "pub const FLAT_MAX_INCREMENT: u32 = ")
}

/// `SCHEMA` in src/main.rs.
fn schema() -> u64 {
    constant("src/main.rs", "const SCHEMA: u32 = ")
}

/// `Measure::bad_arm` in src/measure.rs, by dim metric.
fn bad_arm(metric: &str) -> Option<u64> {
    match metric {
        "largest_fn" => Some(60),
        "cognitive_complexity" => Some(10),
        _ => None,
    }
}

/// The key of a fn dim's measure in `metrics` fns and `shape.worst`.
fn fn_key(metric: &str) -> Option<&'static str> {
    match metric {
        "largest_fn" => Some("lines"),
        "cognitive_complexity" => Some("cognitive_complexity"),
        _ => None,
    }
}

/// The group's raw peak field for a fn dim metric, and its value key.
fn group_peak(metric: &str) -> (&'static str, &'static str) {
    match metric {
        "largest_fn" => ("largest_fn", "lines"),
        _ => ("max_cognitive_complexity", "value"),
    }
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(u64::MAX)
}

#[derive(Default)]
struct Checked {
    violations: Vec<String>,
    groups: usize,
    levels: HashMap<String, usize>,
    dims: usize,
    fns: usize,
    findings: usize,
    subpaths: usize,
}

impl Checked {
    fn fail(&mut self, msg: String) {
        self.violations.push(msg);
    }

    fn expect(&mut self, ok: bool, msg: impl FnOnce() -> String) {
        if !ok {
            self.fail(msg());
        }
    }
}

/// The three outputs of a workspace, with `metrics` fns indexed by file.
struct Outputs {
    summary: Value,
    findings: Value,
    metrics: Value,
}

impl Outputs {
    fn of(dir: &Path) -> Outputs {
        Outputs {
            summary: run_json(dir, &["summary", ".", "--by", "file", "--json"]),
            findings: run_json(dir, &["findings", ".", "--json", "--top", "0"]),
            metrics: run_json(dir, &["metrics", ".", "--json"]),
        }
    }

    /// Fns by absolute path, in file/line order.
    fn fns_by_path(&self) -> HashMap<&str, Vec<&Value>> {
        let mut out: HashMap<&str, Vec<&Value>> = HashMap::new();
        for f in arr(&self.metrics["functions"]) {
            out.entry(f["path"].as_str().unwrap()).or_default().push(f);
        }
        out
    }
}

fn check_schema(o: &Outputs, c: &mut Checked) {
    for (name, v) in [
        ("summary", &o.summary),
        ("findings", &o.findings),
        ("metrics", &o.metrics),
    ] {
        let want = schema();
        c.expect(v["schema"] == want, || {
            format!("{name}: schema {} != {want}", v["schema"])
        });
    }
}

fn check_metrics(o: &Outputs, c: &mut Checked) {
    for f in arr(&o.metrics["functions"]) {
        c.fns += 1;
        let sites = arr(&f["panic_sites"]).len() as u64;
        c.expect(u(&f["panics"]) == sites, || {
            format!(
                "{}:{} {}: panics {} != {sites} panic_sites",
                f["file"], f["line"], f["name"], f["panics"]
            )
        });
        // The worst cognitive increment lies in the fn, and is part of it.
        let cog = u(&f["cognitive_complexity"]);
        let w = &f["worst_increment"];
        let holds = if w.is_null() {
            cog == 0
        } else {
            u(&w["value"]) <= cog
                && (u(&f["start_line"])..=u(&f["end_line"])).contains(&u(&w["line"]))
        };
        c.expect(holds, || {
            format!(
                "{}:{} {}: worst_increment {w} vs cognitive_complexity {cog}",
                f["file"], f["line"], f["name"]
            )
        });
    }
}

fn check_findings(o: &Outputs, c: &mut Checked) {
    const KINDS: &[&str] = &[
        "duplicate",
        "unused",
        "single_impl_trait",
        "long_fn",
        "test_heavy",
        "parametrize",
        "fixture",
        "pass_through",
    ];
    let mut per_kind: HashMap<&str, u64> = HashMap::new();
    // Tests in a fixture finding: (file, line).
    let mut in_fixture = HashSet::new();
    for f in arr(&o.findings["findings"]) {
        c.findings += 1;
        let kind = f["kind"].as_str().unwrap_or("");
        // Duplicate notes are counted apart (`duplicate_notes`).
        let counted = if f["tier"] == "note" {
            "duplicate_notes"
        } else {
            kind
        };
        *per_kind.entry(counted).or_default() += 1;
        c.expect(KINDS.contains(&kind), || format!("finding kind {kind}"));
        if kind == "duplicate" {
            check_duplicate(f, u(&o.findings["thresholds"]["min_dup"]), c);
        }
        if kind == "parametrize" {
            check_parametrize(f, u(&o.findings["thresholds"]["min_dup"]), c);
        }
        if kind == "fixture" {
            check_fixture(f, &mut in_fixture, c);
        }
        let (s, l, e) = (u(&f["start_line"]), u(&f["line"]), u(&f["end_line"]));
        c.expect(s <= l && l <= e, || {
            format!("finding {} {}: lines {s} {l} {e}", f["kind"], f["name"])
        });
    }
    let excluded = o.findings["excluded_duplicates"].as_object();
    c.expect(excluded.is_some(), || {
        "findings without excluded_duplicates".into()
    });
    for k in excluded.into_iter().flat_map(|m| m.keys()) {
        c.expect(k == "substance", || {
            format!("excluded_duplicates: unknown {k}")
        });
    }
    for (kind, n) in per_kind {
        let total = match kind {
            "duplicate_notes" => &o.findings["duplicate_notes"]["total"],
            _ => &o.findings["totals"][kind],
        };
        c.expect(*total == n, || {
            format!("findings {kind} total != {n} (--top 0)")
        });
    }
}

/// Each rule and its tier (src/findings/duplicate/policy.rs).
const RULES: &[(&str, &str)] = &[
    ("signatures", "note"),
    ("cfg_variants", "note"),
    ("cfg_identical", "finding"),
    ("test_shape", "note"),
    ("test_crates", "note"),
    ("crates", "finding"),
    ("identical", "finding"),
    ("same_shape", "finding"),
];

/// Lines after the signature an identical free fn or inherent method needs
/// below --min-dup (src/findings/duplicate.rs `MIN_CALLABLE_DUP`).
const MIN_CALLABLE_DUP: u64 = 7;
/// The same-shape bar, which short test-only groups across crates need too
/// (`MIN_SHAPE_LINES`).
const MIN_SHAPE_LINES: u64 = 15;

fn check_duplicate(f: &Value, min_dup: u64, c: &mut Checked) {
    let at = |l: &Value| (l["file"].clone(), l["line"].clone());
    let copies: Vec<_> = arr(&f["copies"]).iter().map(at).collect();
    let id = format!("duplicate {} {}", f["file"], f["line"]);
    let mut distinct = copies.clone();
    distinct.sort_by_key(|(f, l)| (f.to_string(), l.to_string()));
    distinct.dedup();
    c.expect(copies.len() >= 2 && distinct.len() == copies.len(), || {
        format!("{id}: copies {copies:?}")
    });
    c.expect(copies.first() == Some(&at(f)), || {
        format!("{id}: not copies[0]")
    });
    let (rule, tier) = (
        f["rule"].as_str().unwrap_or(""),
        f["tier"].as_str().unwrap_or(""),
    );
    c.expect(RULES.contains(&(rule, tier)), || {
        format!("{id}: rule {rule}, tier {tier}")
    });
    let sigs: Vec<_> = arr(&f["copies"]).iter().map(|c| &c["signature"]).collect();
    let one_sig = sigs.iter().all(|s| *s == sigs[0]);
    let differs = !arr(&f["differs"]).is_empty();
    let one_crate = arr(&f["crates"]).len() == 1;
    c.expect(!rule.starts_with("cfg_") || one_crate, || {
        format!("{id}: {rule} across crates")
    });
    c.expect(
        one_sig != differs && (rule == "signatures") == differs,
        || format!("{id}: one signature {one_sig}, differs {differs}, rule {rule}"),
    );
    // Every copy has substance; the finding's lines are its largest's.
    let lines: Vec<u64> = arr(&f["copies"]).iter().map(|c| u(&c["lines"])).collect();
    c.expect(
        lines.iter().all(|&l| l >= min_dup.min(MIN_CALLABLE_DUP)),
        || format!("{id}: copies of {lines:?} lines"),
    );
    c.expect(lines.iter().max() == Some(&u(&f["lines"])), || {
        format!("{id}: lines {} of copies {lines:?}", f["lines"])
    });
    let copies = arr(&f["copies"]);
    let impl_copies = copies.iter().filter(|c| c["test"] != true).count();
    let identical = f["identical"] == true;
    let per_crate: HashSet<_> = copies.iter().map(|c| c["crate"].to_string()).collect();
    let one_per_crate = per_crate.len() == copies.len() && !one_crate;
    let smallest = lines.iter().min().copied().unwrap_or(0);
    // Same shape is a finding only with two impl copies.
    c.expect(tier != "finding" || identical || impl_copies >= 2, || {
        format!("{id}: same-shape finding with {impl_copies} impl copies")
    });
    c.expect(
        rule != "test_shape" || (!identical && impl_copies < 2 && impl_copies < copies.len()),
        || format!("{id}: test_shape with {impl_copies} impl copies, identical {identical}"),
    );
    // Test-only, one copy per crate: a note under 15 lines, a finding from.
    if impl_copies == 0 && identical && one_per_crate && arr(&f["differs"]).is_empty() {
        c.expect(
            (rule == "test_crates") == (smallest < MIN_SHAPE_LINES),
            || format!("{id}: {rule} with smallest copy of {smallest} lines"),
        );
    }
    c.expect(
        rule != "test_crates" || (impl_copies == 0 && identical && one_per_crate),
        || format!("{id}: test_crates on impl code or a crate's two copies"),
    );
}

/// A table-driven-test candidate: its tests (the finding's own first, one
/// file, test code), the threshold, literals that differ, its lines.
fn check_parametrize(f: &Value, min_dup: u64, c: &mut Checked) {
    let id = format!("parametrize {} {}", f["file"], f["line"]);
    let tests = arr(&f["tests"]);
    let lines: Vec<u64> = tests.iter().map(|t| u(&t["lines"])).collect();
    let enough = tests.len() >= 3 || (tests.len() == 2 && lines.iter().all(|&l| l >= min_dup));
    c.expect(enough, || {
        format!("{id}: {} tests of {lines:?} lines", tests.len())
    });
    c.expect(
        tests
            .first()
            .is_some_and(|t| t["line"] == f["line"] && t["file"] == f["file"]),
        || format!("{id}: not tests[0]"),
    );
    c.expect(tests.iter().all(|t| t["file"] == f["file"]), || {
        format!("{id}: tests in several files")
    });
    c.expect(f["category"] == "test", || format!("{id}: not test code"));
    // A name written twice is cfg variants (at most one compiles).
    let mut names: Vec<&str> = tests
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();
    names.sort_unstable();
    names.dedup();
    c.expect(names.len() == tests.len(), || format!("{id}: names repeat"));
    // `identical` pairs tests up: never a lone one; all of them only with
    // two bodies twice or more (some literal differs).
    let identical = tests.iter().filter(|t| t["identical"] == true).count();
    c.expect(
        identical != 1 && (identical < tests.len() || tests.len() >= 4),
        || format!("{id}: {identical} identical"),
    );
    let (differ, of) = (u(&f["literals"]["differ"]), u(&f["literals"]["of"]));
    c.expect(1 <= differ && differ <= of, || {
        format!("{id}: literals {differ} of {of}")
    });
    c.expect(u(&f["lines"]) == lines.iter().sum::<u64>(), || {
        format!("{id}: lines {} != its tests' {lines:?}", f["lines"])
    });
}

/// A fixture candidate: at least 3 tests (the finding's own first, one
/// file, test code, distinct names) opening with at least 4 statements,
/// no test in two fixture findings; its lines are the tests' in the
/// opening: the first test's and at least one per other test.
fn check_fixture(f: &Value, seen: &mut HashSet<(String, u64)>, c: &mut Checked) {
    let id = format!("fixture {} {}", f["file"], f["line"]);
    let tests = arr(&f["tests"]);
    let statements = u(&f["prefix"]["statements"]);
    c.expect(tests.len() >= 3 && statements >= 4, || {
        format!("{id}: {} tests, {statements} statements", tests.len())
    });
    c.expect(
        tests
            .first()
            .is_some_and(|t| t["line"] == f["line"] && t["file"] == f["file"]),
        || format!("{id}: not tests[0]"),
    );
    c.expect(tests.iter().all(|t| t["file"] == f["file"]), || {
        format!("{id}: tests in several files")
    });
    c.expect(f["category"] == "test", || format!("{id}: not test code"));
    let mut names: Vec<&str> = tests
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();
    names.sort_unstable();
    names.dedup();
    c.expect(names.len() == tests.len(), || format!("{id}: names repeat"));
    for t in tests {
        let at = (t["file"].to_string(), u(&t["line"]));
        c.expect(seen.insert(at.clone()), || {
            format!("{id}: test {at:?} in two fixture findings")
        });
    }
    // Each other test has at least a line in the opening.
    let first = u(&f["prefix"]["lines"]);
    let others = tests.len().saturating_sub(1) as u64;
    c.expect(first >= 1 && u(&f["lines"]) >= first + others, || {
        format!("{id}: lines {} for prefix lines {first}", f["lines"])
    });
}

/// The first fn with the greatest `key` (bloaty's tie rule).
fn first_max<'a>(fns: &[&'a Value], key: &str) -> Option<&'a Value> {
    let mut best: Option<&Value> = None;
    for f in fns {
        if best.is_none_or(|b| u(&f[key]) > u(&b[key])) {
            best = Some(f);
        }
    }
    best
}

/// A group's raw fields against its fns in `metrics`.
fn check_rollup(g: &Value, fns: &[&Value], c: &mut Checked) {
    let name = &g["name"];
    let panics: u64 = fns.iter().map(|f| u(&f["panics"])).sum();
    c.expect(u(&g["panics"]) == panics, || {
        format!("{name}: panics {} != its fns' {panics}", g["panics"])
    });
    for (field, key, value) in [
        ("largest_fn", "lines", "lines"),
        (
            "max_cyclomatic_complexity",
            "cyclomatic_complexity",
            "value",
        ),
        ("max_nesting", "nesting", "value"),
        ("max_cognitive_complexity", "cognitive_complexity", "value"),
    ] {
        let Some(top) = first_max(fns, key) else {
            continue;
        };
        let peak = &g[field];
        c.expect(
            peak[value] == top[key] && peak["line"] == top["line"],
            || {
                format!(
                    "{name}: {field} {peak} != first max fn {} {}",
                    top["name"], top[key]
                )
            },
        );
    }
    let most = &g["most_panics"];
    if panics == 0 {
        c.expect(most.is_null(), || {
            format!("{name}: most_panics without panics")
        });
        return;
    }
    let top = first_max(fns, "panics").unwrap();
    c.expect(
        most["line"] == top["line"]
            && most["value"] == top["panics"]
            && most["sites"] == top["panic_sites"]
            && u(&most["value"]) == arr(&most["sites"]).len() as u64,
        || {
            format!(
                "{name}: most_panics {most} != {} {}",
                top["name"], top["panics"]
            )
        },
    );
}

/// One smell dim of group `g` against the group and its fns.
fn check_dim(g: &Value, d: &Value, fns: &[&Value], c: &mut Checked) {
    c.dims += 1;
    let name = &g["name"];
    let metric = d["metric"].as_str().unwrap_or("");
    let excused = &d["excused"];
    let want_dim = match metric {
        "largest_fn" | "impl_lines" => "size",
        "cognitive_complexity" => "complexity",
        "panic_density" => "fragility",
        _ => "?",
    };
    c.expect(d["dim"] == want_dim, || {
        format!("{name}: dim {} for metric {metric}", d["dim"])
    });
    let Some(key) = fn_key(metric) else {
        // File-wide signals: never excused, the group's own; the panic
        // density is a culprit, impl lines aren't.
        let culprit = metric == "panic_density";
        c.expect(
            excused.is_null() && (d["culprit"] == true) == culprit,
            || format!("{name}: {metric} excused, or culprit isn't {culprit}: {d}"),
        );
        let same = match metric {
            "impl_lines" => d["value"] == g["impl"],
            _ => d["value"] == g["panic_density"] && d["panics"] == g["panics"],
        };
        c.expect(same, || format!("{name}: {metric} {d} != group's"));
        return;
    };
    let culprit = excused.is_null() && bad_arm(metric).is_some_and(|b| u(&d["value"]) >= b);
    c.expect(d["culprit"].as_bool().unwrap_or(false) == culprit, || {
        format!("{name}: culprit should be {culprit}: {d}")
    });
    let Some(f) = fns.iter().find(|f| f["line"] == d["line"]) else {
        c.fail(format!("{name}: dim names no measured fn: {d}"));
        return;
    };
    // `flat`: a culprit-sized largest fn with nothing nested (for an arm or
    // a dispatch, as scored: metrics doesn't show those increments).
    let flat = d.get("flat").is_some();
    c.expect(!flat || (culprit && metric == "largest_fn"), || {
        format!("{name}: flat on a non-culprit or non-size dim: {d}")
    });
    if culprit
        && metric == "largest_fn"
        && d.get("arm").is_none()
        && f["shape"]["kind"] != "dispatch"
    {
        let worst = f["worst_increment"]["value"].as_u64().unwrap_or(0);
        c.expect(flat == (worst <= flat_max_increment()), || {
            format!(
                "{name}: flat should be {}: worst increment {worst}: {d}",
                !flat
            )
        });
    }
    let (own, value) = (u(&f[key]), u(&d["value"]));
    let (peak_field, peak_key) = group_peak(metric);
    let peak = u(&g[peak_field][peak_key]);
    c.expect(value <= own && value <= peak, || {
        format!("{name}: {metric} {value} > fn's own {own} or group peak {peak}")
    });
    if let Some(arm) = d.get("arm") {
        let worst = &f["shape"]["worst"][key];
        c.expect(
            f["shape"]["kind"] == "dispatch"
                && worst["name"] == arm["name"]
                && worst["line"] == arm["line"]
                && worst["value"] == arm["value"]
                && u(&arm["value"]) <= own,
            || format!("{name}: arm {arm} != shape.worst.{key} {worst}"),
        );
    }
    if excused.is_null() {
        return;
    }
    c.expect(value == own, || {
        format!("{name}: excused {metric} {value} != fn's own {own}")
    });
    // Noted: an excused signal that would have been a culprit.
    c.expect(bad_arm(metric).is_some_and(|b| value >= b), || {
        format!("{name}: excused {metric} {value} under the culprit bar")
    });
    let ok = match excused["kind"].as_str() {
        Some("table") => metric == "largest_fn" && f["shape"]["kind"] == "table",
        Some("dispatch") => f["shape"]["kind"] == "dispatch",
        Some("allowed") => {
            let lint = &excused["lint"];
            let covers = match metric {
                "largest_fn" => lint == "too_many_lines",
                "cognitive_complexity" => lint == "cognitive_complexity",
                _ => false,
            };
            covers && arr(&f["allowed"]).contains(lint)
        }
        _ => false,
    };
    c.expect(ok, || {
        format!(
            "{name}: excuse {excused} doesn't fit {metric} of {}",
            f["name"]
        )
    });
}

fn check_smell(g: &Value, fns: &[&Value], c: &mut Checked) {
    let smell = &g["smell"];
    let name = &g["name"];
    let level = smell["level"].as_str();
    let dims = arr(&smell["dims"]);
    *c.levels.entry(level.unwrap_or("none").into()).or_default() += 1;
    let Some(level) = level else {
        c.expect(
            smell.get("dims").is_none() && smell.get("why").is_none(),
            || format!("{name}: unlevelled with dims or why"),
        );
        return;
    };
    c.expect(!dims.is_empty() && smell["why"].is_string(), || {
        format!("{name}: {level} without dims or why")
    });
    for d in dims {
        check_dim(g, d, fns, c);
    }
    let real = dims.iter().filter(|d| d["excused"].is_null()).count();
    match level {
        "noted" => c.expect(real == 0, || format!("{name}: noted with a real reason")),
        _ => {
            c.expect(real > 0, || {
                format!("{name}: {level} without a real reason")
            });
            c.expect(dims.iter().any(|d| d["culprit"] == true), || {
                format!("{name}: {level} without a culprit")
            });
            // Long flat fns alone never make a file hot.
            let full = dims
                .iter()
                .any(|d| d["culprit"] == true && d.get("flat").is_none());
            c.expect(level != "hot" || full, || {
                format!("{name}: hot with only long flat culprits")
            });
            // A flat largest fn doesn't hide a culprit-sized nested one
            // (checked on plain fns: no shape or allow changes their lines).
            let flat_size = dims.iter().any(|d| d.get("flat").is_some());
            let hidden = fns.iter().any(|f| {
                f.get("shape").is_none()
                    && arr(&f["allowed"]).is_empty()
                    && u(&f["lines"]) >= bad_arm("largest_fn").unwrap_or(0)
                    && f["worst_increment"]["value"].as_u64().unwrap_or(0) > flat_max_increment()
            });
            c.expect(!flat_size || !hidden || full, || {
                format!("{name}: a flat largest fn hides a nested culprit-sized fn")
            });
        }
    }
}

fn check_groups(o: &Outputs, c: &mut Checked) {
    let fns = o.fns_by_path();
    for g in arr(&o.summary["groups"]) {
        let path = g["path"].as_str().unwrap_or("");
        let file_fns = fns.get(path).map_or(&[][..], Vec::as_slice);
        if g.get("panics").is_some() {
            check_rollup(g, file_fns, c);
        }
        if g.get("smell").is_some() {
            c.groups += 1;
            check_smell(g, file_fns, c);
        }
    }
}

/// Smell by absolute path.
fn smells(summary: &Value) -> HashMap<String, Value> {
    arr(&summary["groups"])
        .iter()
        .filter(|g| g.get("smell").is_some())
        .map(|g| (g["path"].as_str().unwrap().to_string(), g["smell"].clone()))
        .collect()
}

/// Each of `sub` (relative to `dir`) gives its files the whole run's smell.
fn check_subpaths(dir: &Path, whole: &Value, subs: &[String], c: &mut Checked) {
    let want = smells(whole);
    for sub in subs {
        c.subpaths += 1;
        let part = run_json(dir, &["summary", sub, "--by", "file", "--json"]);
        for (path, smell) in smells(&part) {
            c.expect(want.get(&path) == Some(&smell), || {
                format!(
                    "PATH {sub}: {path} smell {smell} != whole run's {:?}",
                    want.get(&path)
                )
            });
        }
    }
}

/// Up to three directories and one file holding levelled files.
fn subpaths_of(dir: &Path, summary: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let levelled = arr(&summary["groups"])
        .iter()
        .filter(|g| g["smell"]["level"].is_string())
        .filter_map(|g| g["path"].as_str())
        .filter_map(|p| Path::new(p).strip_prefix(dir).ok().map(Path::to_path_buf));
    for rel in levelled {
        if out.is_empty() {
            out.push(rel.display().to_string());
        }
        let Some(parent) = rel.parent().filter(|p| !p.as_os_str().is_empty()) else {
            continue;
        };
        let parent = parent.display().to_string();
        if !out.contains(&parent) && out.len() < 4 {
            out.push(parent);
        }
    }
    out
}

fn check_all(dir: &Path, subs: Option<Vec<String>>) -> Checked {
    let mut c = Checked::default();
    let o = Outputs::of(dir);
    check_schema(&o, &mut c);
    check_metrics(&o, &mut c);
    check_findings(&o, &mut c);
    check_groups(&o, &mut c);
    let dir = dir.canonicalize().unwrap();
    let subs = subs.unwrap_or_else(|| subpaths_of(&dir, &o.summary));
    check_subpaths(&dir, &o.summary, &subs, &mut c);
    c
}

fn report(label: &str, c: &Checked) -> bool {
    let mut levels: Vec<_> = c.levels.iter().collect();
    levels.sort();
    eprintln!(
        "{label}: {} scored files {levels:?}, {} dims, {} fns, {} findings, {} sub-paths: {} violations",
        c.groups,
        c.dims,
        c.fns,
        c.findings,
        c.subpaths,
        c.violations.len()
    );
    for v in c.violations.iter().take(40) {
        eprintln!("  {v}");
    }
    if c.violations.len() > 40 {
        eprintln!("  … {} more", c.violations.len() - 40);
    }
    c.violations.is_empty()
}

/// The Neovim side refuses output older than its `SCHEMA`: bump both
/// together.
#[test]
fn lua_schema_matches() {
    assert_eq!(
        constant("../../lua/mbloaty/init.lua", "local SCHEMA = "),
        schema(),
        "lua/mbloaty/init.lua SCHEMA != src/main.rs SCHEMA"
    );
}

#[test]
fn fixture() {
    let ws = Ws::new("invariants");
    let subs = ["core/src", "core/src/badarm.rs", "app", "core/src/hot.rs"];
    let c = check_all(&ws.0, Some(subs.map(String::from).to_vec()));
    // Each kind of level and dim is exercised.
    for level in ["hot", "warm", "noted"] {
        assert!(c.levels.get(level).is_some_and(|&n| n > 0), "no {level}");
    }
    assert!(report("fixture", &c), "invariants broken on the fixture");
}

#[test]
fn corpus() {
    let Ok(corpus) = std::env::var("BLOATY_CORPUS") else {
        eprintln!("skipping the corpus invariants: BLOATY_CORPUS is unset");
        return;
    };
    let mut ok = true;
    for dir in corpus.split(':').filter(|d| !d.is_empty()) {
        let c = check_all(Path::new(dir), None);
        ok &= report(dir, &c);
    }
    assert!(ok, "invariants broken on the corpus");
}
