//! Relative smell score per file: each signal becomes a percentile within
//! the workspace, signals collapse into three dimensions (size, complexity,
//! fragility), and non-dominated sorting ranks files on those. No weights:
//! levels walk the Pareto fronts, so a file dominated on every axis by
//! another never ranks above it.

use crate::measure::{Dim, Measure};
use crate::names::collapse_generics;
use crate::rollup::{ArmAt, Excuse, FileMetrics, FnScore, effective_metrics, file_metrics};
use crate::scan::Scan;
use serde::Serialize;
use std::cmp::Reverse;

/// Files smaller than this aren't scored: too little code to smell.
pub const MIN_IMPL_LINES: usize = 10;
/// Projects with fewer scored files get no `warm`: too few to rank finely.
pub const MIN_FILES_FOR_WARM: usize = 20;
/// Directories show their worst child, so even 10% hot files would light
/// up nearly every directory.
pub const HOT_SHARE: f64 = 0.05;
pub const WARM_SHARE: f64 = 0.10;
/// A dimension at or above this is in the project's top decile.
pub const HOT_DIM: f64 = 0.9;
pub const WARM_DIM: f64 = 0.75;
/// A file has a fragility signal only with this many panic sites and a
/// density (it has >= 30 impl lines) of at least `MIN_DENSITY` per 100.
const MIN_PANICS: u32 = 2;
const MIN_DENSITY: f64 = 1.0;
/// With fewer qualifying files than this, ranks are noise: fixed bands by
/// density are used instead.
const MIN_RANKED_FRAGILE: usize = 5;
/// Below `Measure::Nesting.bar()` nesting is ordinary code (ties at depths
/// 2-3 would otherwise read as top decile): it adds nothing to the
/// complexity dimension.
fn deep(n: u32) -> bool {
    n >= Measure::Nesting.bar()
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Hot,
    Warm,
    /// Not hot or warm, but would have been without its excused signals
    /// (a table, a dispatch's width, an allowed clippy lint).
    Noted,
}

/// The worse of two hot/warm levels.
fn worse(a: Option<Level>, b: Option<Level>) -> Option<Level> {
    match (a, b) {
        (Some(Level::Hot), _) | (_, Some(Level::Hot)) => Some(Level::Hot),
        (Some(l), _) | (None, Some(l)) => Some(l),
        (None, None) => None,
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Smell {
    pub level: Option<Level>,
    /// Pareto front: 1 means no other file is at least as bad on every
    /// dimension and worse on one.
    pub front: usize,
    /// Percentiles within the workspace, in [0, 1], excused signals left
    /// out.
    pub size: f64,
    pub complexity: f64,
    pub fragility: f64,
    /// For files with a level: the signals behind it with their raw
    /// numbers, as text (`dims` rendered; for display only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// For files with a level: the signals behind `why`, structured. Those
    /// with `excused` are what a noted file was noted for (in a hot or warm
    /// file too).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dims: Vec<Reason>,
}

/// A raw number behind a dimension. Serialized with its `metric` (see
/// `Measure::metric`, `impl_lines`, `panic_density`) and `value`; fn
/// signals carry the fn's (uncollapsed) `name` and `line`, and `arm` when
/// a dispatch's arm is to blame.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    Fn {
        measure: Measure,
        value: u32,
        name: String,
        line: usize,
        arm: Option<ArmAt>,
    },
    /// `value`: the file's impl lines.
    ImplLines { value: usize },
    /// `value`: panic sites per 100 impl lines; `panics`: their count.
    PanicDensity { value: f64, panics: u32 },
}

impl Serialize for Signal {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            Signal::Fn {
                measure,
                value,
                name,
                line,
                arm,
            } => {
                m.serialize_entry("metric", measure.metric())?;
                m.serialize_entry("value", value)?;
                m.serialize_entry("name", name)?;
                m.serialize_entry("line", line)?;
                if let Some(arm) = arm {
                    m.serialize_entry("arm", arm)?;
                }
            }
            Signal::ImplLines { value } => {
                m.serialize_entry("metric", "impl_lines")?;
                m.serialize_entry("value", value)?;
            }
            Signal::PanicDensity { value, panics } => {
                m.serialize_entry("metric", "panic_density")?;
                m.serialize_entry("value", value)?;
                m.serialize_entry("panics", panics)?;
            }
        }
        m.end()
    }
}

impl Signal {
    fn of_fn(measure: Measure, value: u32, name: &str, line: usize, arm: Option<ArmAt>) -> Self {
        Signal::Fn {
            measure,
            value,
            name: name.to_string(),
            line,
            arm,
        }
    }

    fn dim(&self) -> Dim {
        match self {
            Signal::Fn { measure, .. } => measure.dim(),
            Signal::ImplLines { .. } => Dim::Size,
            Signal::PanicDensity { .. } => Dim::Fragility,
        }
    }

    /// The fn and measure it is about, for fn signals.
    fn about(&self) -> Option<(usize, Measure)> {
        match self {
            Signal::Fn { line, measure, .. } => Some((*line, *measure)),
            Signal::ImplLines { .. } | Signal::PanicDensity { .. } => None,
        }
    }

    /// `largest fn 225 lines (build)`: the evidence as `why` words it.
    fn text(&self) -> String {
        match self {
            Signal::Fn {
                measure,
                value,
                name,
                arm,
                ..
            } => {
                let name = collapse_generics(name);
                let at = match arm {
                    Some(a) => format!("{name}, arm {}", a.name),
                    None => name,
                };
                format!("{} ({at})", measure.text(*value))
            }
            Signal::ImplLines { value } => format!("{value} impl lines"),
            Signal::PanicDensity { value, panics } => {
                format!("{value:.1} panics/100 lines ({panics} panics)")
            }
        }
    }
}

/// A signal that put a file's dimension at or above its level's threshold
/// (or, `excused`, would have).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Reason {
    pub dim: Dim,
    #[serde(flatten)]
    pub signal: Signal,
    /// A real fn signal at or past `Measure::bad_arm`: worth marking where
    /// it is (the lens's floor).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub culprit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excused: Option<Excuse>,
}

impl Reason {
    fn new(signal: Signal) -> Self {
        let culprit =
            matches!(signal, Signal::Fn { measure, value, .. } if value >= measure.bad_arm());
        Reason {
            dim: signal.dim(),
            signal,
            culprit,
            excused: None,
        }
    }

    fn excused(signal: Signal, excuse: &Excuse) -> Self {
        Reason {
            excused: Some(excuse.clone()),
            culprit: false,
            ..Reason::new(signal)
        }
    }

    /// Whether it is about the same fn and measure as `other`.
    fn same(&self, other: &Reason) -> bool {
        self.signal.about().is_some() && self.signal.about() == other.signal.about()
    }

    fn text(&self) -> String {
        let excuse = match &self.excused {
            None => return self.signal.text(),
            Some(Excuse::Table { entries }) => format!("table, {entries} entries"),
            Some(Excuse::Dispatch { arms, .. }) => format!("dispatch, {arms} arms"),
            Some(Excuse::Allowed { lint }) => format!("allows {}", lint.as_str()),
        };
        format!("{} [{excuse}]", self.signal.text())
    }
}

/// One scored file's signals.
pub struct Input<'a> {
    pub impl_lines: usize,
    pub metrics: &'a FileMetrics,
}

impl Input<'_> {
    /// The file's peak for a measure, as a signal.
    fn peak(&self, m: Measure) -> Option<Signal> {
        let fm = self.metrics;
        let fn_peak = |p: &Option<crate::rollup::FnPeak>| {
            p.as_ref()
                .map(|p| Signal::of_fn(m, p.value, &p.name, p.line, p.arm.clone()))
        };
        match m {
            Measure::Lines => fm
                .largest_fn
                .as_ref()
                .map(|f| Signal::of_fn(m, f.lines as u32, &f.name, f.line, f.arm.clone())),
            Measure::CyclomaticComplexity => fn_peak(&fm.max_complexity),
            Measure::Nesting => fn_peak(&fm.max_nesting),
        }
    }

    fn peak_value(&self, m: Measure) -> Option<u32> {
        match self.peak(m)? {
            Signal::Fn { value, .. } => Some(value),
            _ => None,
        }
    }
}

/// Smell of every scanned file (by index into `scan.files`), scored against
/// every other scanned file, not just the requested ones: a file's level
/// doesn't depend on the PATH it was asked about. Whole-test files, build
/// scripts and examples, files under `MIN_IMPL_LINES` and files without
/// measured fns get none.
pub fn for_scan(scan: &Scan) -> Vec<Option<Smell>> {
    let mut idx = Vec::new();
    let mut metrics = Vec::new();
    for (i, f) in scan.files.iter().enumerate() {
        if scan.is_support(f) {
            continue;
        }
        let impl_lines = f.counts().implementation;
        if impl_lines < MIN_IMPL_LINES {
            continue;
        }
        if let Some(m) = file_metrics(f, impl_lines).filter(|m| m.largest_fn.is_some())
            && let Some(e) = effective_metrics(f, impl_lines)
        {
            idx.push(i);
            metrics.push((impl_lines, m, e));
        }
    }
    let raw: Vec<Input> = metrics
        .iter()
        .map(|(impl_lines, m, _)| Input {
            impl_lines: *impl_lines,
            metrics: m,
        })
        .collect();
    let effective: Vec<Input> = metrics
        .iter()
        .map(|(impl_lines, _, e)| Input {
            impl_lines: *impl_lines,
            metrics: e,
        })
        .collect();
    // Ties in the ranking go by path.
    let paths: Vec<&str> = idx.iter().map(|&i| scan.files[i].rel.as_str()).collect();
    let mut out = vec![None; scan.files.len()];
    for (i, s) in idx
        .into_iter()
        .zip(score_excusing(&raw, &effective, &paths))
    {
        out[i] = Some(s);
    }
    out
}

/// Per-signal percentiles of every input, before they collapse.
struct Pcts {
    /// Fn peaks, by `Measure::index` (nesting zeroed when not deep).
    fns: [Vec<f64>; 3],
    lines: Vec<f64>,
    fragility: Vec<f64>,
}

fn qualifies_fragile(i: &Input) -> bool {
    let m = i.metrics;
    m.panics >= MIN_PANICS && m.panic_density.is_some_and(|d| d >= MIN_DENSITY)
}

/// Fixed fragility for when too few files qualify to rank.
fn fragility_band(density: f64) -> f64 {
    if density >= 3.0 {
        0.9
    } else if density >= 1.5 {
        0.75
    } else {
        0.5
    }
}

/// Whether any raw signal is bad in absolute terms (`Measure::bar`):
/// percentiles alone would call the largest file of a tidy project hot.
fn past_bar(i: &Input) -> bool {
    Measure::ALL
        .iter()
        .any(|&m| i.peak_value(m).is_some_and(|v| v >= m.bar()))
        || qualifies_fragile(i)
}

/// The inputs ranked: percentiles, dimensions, fronts and hot/warm levels.
struct Ranked {
    p: Pcts,
    dims: Vec<[f64; 3]>,
    fronts: Vec<usize>,
    levels: Vec<Option<Level>>,
}

fn rank(inputs: &[Input], paths: &[&str]) -> Ranked {
    let pct =
        |f: &dyn Fn(&Input) -> Option<f64>| percentiles(&inputs.iter().map(f).collect::<Vec<_>>());
    let fragile = inputs.iter().filter(|i| qualifies_fragile(i)).count();
    let fn_pct = |m: Measure| {
        let p = pct(&|i| i.peak_value(m).map(f64::from));
        if m != Measure::Nesting {
            return p;
        }
        // Ranked among all files, then zeroed when not deep.
        p.into_iter()
            .zip(inputs)
            .map(|(p, i)| {
                if i.peak_value(m).is_some_and(deep) {
                    p
                } else {
                    0.0
                }
            })
            .collect()
    };
    let p = Pcts {
        fns: Measure::ALL.map(fn_pct),
        lines: pct(&|i| Some(i.impl_lines as f64)),
        fragility: if fragile < MIN_RANKED_FRAGILE {
            inputs
                .iter()
                .map(|i| match i.metrics.panic_density {
                    Some(d) if qualifies_fragile(i) => fragility_band(d),
                    _ => 0.0,
                })
                .collect()
        } else {
            pct(&|i| i.metrics.panic_density.filter(|_| qualifies_fragile(i)))
        },
    };
    let [largest, cc, nest] = &p.fns;
    let dims: Vec<[f64; 3]> = (0..inputs.len())
        .map(|k| {
            [
                round2(largest[k].max(p.lines[k])),
                round2(cc[k].max(nest[k])),
                round2(p.fragility[k]),
            ]
        })
        .collect();
    let bar: Vec<bool> = inputs.iter().map(past_bar).collect();
    let fronts = fronts(&dims);
    let levels = levels(&dims, &fronts, &bar, paths);
    Ranked {
        p,
        dims,
        fronts,
        levels,
    }
}

/// Scores files with nothing excused.
#[cfg(test)]
pub fn score(inputs: &[Input], paths: &[&str]) -> Vec<Smell> {
    score_excusing(inputs, inputs, paths)
}

/// Scores the files by their `effective` signals (excused ones left out,
/// see `effective_metrics`). A file the `raw` ranking levels keeps the
/// worse of both levels when an excused dispatch has a bad arm
/// (`Measure::bad_arm`, ranking aside). Otherwise, a file that gets no
/// level by its effective signals but would by its raw ones because of an
/// excused fn is noted; hot and warm files carry such excused reasons too,
/// unless a real reason already names the fn for that measure.
pub fn score_excusing(raw: &[Input], effective: &[Input], paths: &[&str]) -> Vec<Smell> {
    let r = rank(raw, paths);
    let e = rank(effective, paths);
    (0..effective.len())
        .map(|k| {
            let excused = &raw[k].metrics.excused;
            let mut real =
                e.levels[k].map_or_else(Vec::new, |l| reasons(&effective[k], &e.p, k, l));
            let bad = r.levels[k].map_or_else(Vec::new, |_| bad_arms(excused));
            let kept = if bad.is_empty() {
                e.levels[k]
            } else {
                worse(e.levels[k], r.levels[k])
            };
            for b in bad {
                if !real.iter().any(|x| x.same(&b)) {
                    real.push(b);
                }
            }
            let mut noted: Vec<Reason> = r.levels[k]
                .map_or_else(Vec::new, |l| reasons(&raw[k], &r.p, k, l))
                .into_iter()
                .filter_map(|reason| {
                    let (line, m) = reason.signal.about()?;
                    let excuse = excused.iter().find(|x| x.line == line)?.excuse_for(m)?;
                    let reason = Reason::excused(reason.signal, excuse);
                    (!real.iter().any(|x| x.same(&reason))).then_some(reason)
                })
                .collect();
            if noted.is_empty() && kept.is_none() && r.levels[k].is_some() {
                // Levelled by file-wide signals (impl lines) only while an
                // excused fn met the bar.
                noted = bar_reasons(excused);
            }
            let level = kept.or((!noted.is_empty()).then_some(Level::Noted));
            let [size, complexity, fragility] = e.dims[k];
            Smell {
                level,
                front: e.fronts[k],
                size,
                complexity,
                fragility,
                why: level.map(|_| why(&real, &noted)),
                dims: real.into_iter().chain(noted).collect(),
            }
        })
        .collect()
}

/// The bad arms of excused dispatches (`Measure::bad_arm`), as real
/// reasons pointing at the arm.
fn bad_arms(excused: &[FnScore]) -> Vec<Reason> {
    let mut out = Vec::new();
    for x in excused {
        let Some(arms) = &x.worst_arms else { continue };
        for m in Measure::ALL {
            let i = m.index();
            let Some(arm) = arms[i].as_ref().filter(|a| a.value >= m.bad_arm()) else {
                continue;
            };
            // The fn's effective number (remainder and counted arms), the
            // arm's own in `arm.value`.
            let value = x.effective[i].as_ref().map_or(arm.value, |s| s.value);
            let signal = Signal::of_fn(m, value, &x.name, x.line, Some(arm.clone()));
            out.push(Reason::new(signal));
        }
    }
    out
}

/// The own numbers of excused fns past the bar (`Measure::bar`), as
/// excused reasons: what put a file levelled only by file-wide signals past
/// the bar.
fn bar_reasons(excused: &[FnScore]) -> Vec<Reason> {
    let mut out = Vec::new();
    for x in excused {
        for m in Measure::ALL {
            let value = x.own[m.index()];
            if let Some(excuse) = x.excuse_for(m).filter(|_| value >= m.bar()) {
                let signal = Signal::of_fn(m, value, &x.name, x.line, None);
                out.push(Reason::excused(signal, excuse));
            }
        }
    }
    out
}

/// The signals behind the dimensions at or above the level's threshold, by
/// dimension (size, complexity, fragility).
fn reasons(input: &Input, p: &Pcts, k: usize, level: Level) -> Vec<Reason> {
    let threshold = if level == Level::Hot {
        HOT_DIM
    } else {
        WARM_DIM
    };
    let above = |p: f64| round2(p) >= threshold;
    let mut out = Vec::new();
    let fn_signal = |m: Measure| input.peak(m).filter(|_| above(p.fns[m.index()][k]));
    out.extend(fn_signal(Measure::Lines));
    if above(p.lines[k]) {
        out.push(Signal::ImplLines {
            value: input.impl_lines,
        });
    }
    out.extend(fn_signal(Measure::CyclomaticComplexity));
    out.extend(fn_signal(Measure::Nesting));
    let m = input.metrics;
    if let Some(d) = m.panic_density.filter(|_| above(p.fragility[k])) {
        out.push(Signal::PanicDensity {
            value: d,
            panics: m.panics,
        });
    }
    out.into_iter().map(Reason::new).collect()
}

/// `real` and `noted` reasons as text, grouped by dimension: `size: largest
/// fn 225 lines (build); fragility: 3.1 panics/100 lines (12 panics);
/// noted: largest fn 107 lines (routes) [table, 18 entries]`.
fn why(real: &[Reason], noted: &[Reason]) -> String {
    let mut parts: Vec<String> = Dim::ALL
        .into_iter()
        .filter_map(|dim| {
            let e: Vec<String> = real
                .iter()
                .filter(|r| r.dim == dim)
                .map(Reason::text)
                .collect();
            (!e.is_empty()).then(|| format!("{}: {}", dim.as_str(), e.join(", ")))
        })
        .collect();
    if !noted.is_empty() {
        let e: Vec<String> = noted.iter().map(Reason::text).collect();
        parts.push(format!("noted: {}", e.join(", ")));
    }
    parts.join("; ")
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Mid-rank percentile in [0, 1] of each value among the present ones
/// (ties share their average rank). A zero or missing value is 0: no signal
/// is never a smell. A lone present value is 1.
fn percentiles(values: &[Option<f64>]) -> Vec<f64> {
    let mut sorted: Vec<f64> = values.iter().flatten().copied().collect();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    values
        .iter()
        .map(|v| match *v {
            Some(v) if v > 0.0 => {
                if n == 1 {
                    return 1.0;
                }
                let less = sorted.partition_point(|&x| x < v);
                let equal = sorted.partition_point(|&x| x <= v) - less;
                (less as f64 + (equal - 1) as f64 / 2.0) / (n - 1) as f64
            }
            _ => 0.0,
        })
        .collect()
}

/// `a` is at least as bad as `b` everywhere and worse somewhere.
fn dominates(a: &[f64; 3], b: &[f64; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x >= y) && a != b
}

/// Non-dominated sorting: front 1 is dominated by nothing, front k by
/// something in front k-1 at worst. A dominator always has a larger sum,
/// so one pass in order of decreasing sum settles each file from those
/// before it: O(n²) time, O(n) memory.
fn fronts(dims: &[[f64; 3]]) -> Vec<usize> {
    let sum = |d: &[f64; 3]| d.iter().sum::<f64>();
    let mut order: Vec<usize> = (0..dims.len()).collect();
    order.sort_by(|&a, &b| sum(&dims[b]).total_cmp(&sum(&dims[a])));
    let mut front = vec![0; dims.len()];
    for (k, &i) in order.iter().enumerate() {
        front[i] = 1 + order[..k]
            .iter()
            .filter(|&&j| dominates(&dims[j], &dims[i]))
            .map(|&j| front[j])
            .max()
            .unwrap_or(0);
    }
    front
}

/// hot: walking files by (front, top-decile dimensions, sum, path), the
/// first past the bar with a dimension >= 0.9, up to 5% of files. warm:
/// the rest past the bar with a dimension >= 0.75, by sum then path, up to
/// 10% more; none in projects under 20 files. Walking the fronts lets later
/// ones fill in when front 1 is small, as it is in three dimensions with
/// many files.
fn levels(dims: &[[f64; 3]], fronts: &[usize], bar: &[bool], paths: &[&str]) -> Vec<Option<Level>> {
    let n = dims.len();
    let max = |d: &[f64; 3]| d.iter().copied().fold(0.0, f64::max);
    let top = |d: &[f64; 3]| d.iter().filter(|&&x| x >= HOT_DIM).count();
    let sum = |d: &[f64; 3]| d.iter().sum::<f64>();
    let by_sum = |a: usize, b: usize| {
        sum(&dims[b])
            .total_cmp(&sum(&dims[a]))
            .then_with(|| paths[a].cmp(paths[b]))
    };
    let cap = |share: f64| (n as f64 * share).ceil() as usize;
    let mut out = vec![None; n];

    let mut order: Vec<usize> = (0..n)
        .filter(|&i| bar[i] && max(&dims[i]) >= HOT_DIM)
        .collect();
    order.sort_by(|&a, &b| {
        (fronts[a], Reverse(top(&dims[a])))
            .cmp(&(fronts[b], Reverse(top(&dims[b]))))
            .then_with(|| by_sum(a, b))
    });
    for &i in order.iter().take(cap(HOT_SHARE)) {
        out[i] = Some(Level::Hot);
    }

    let warm_cap = if n < MIN_FILES_FOR_WARM {
        0
    } else {
        cap(WARM_SHARE)
    };
    let mut order: Vec<usize> = (0..n)
        .filter(|&i| out[i].is_none() && bar[i] && max(&dims[i]) >= WARM_DIM)
        .collect();
    order.sort_by(|&a, &b| by_sum(a, b));
    for &i in order.iter().take(warm_cap) {
        out[i] = Some(Level::Warm);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollup::{FnPeak, FnSize};

    #[test]
    fn percentiles_mid_rank_and_zero_rule() {
        let p = percentiles(&[Some(0.0), Some(5.0), Some(5.0), Some(9.0), None, Some(1.0)]);
        // Sorted present values: 0 1 5 5 9; the 5s share ranks 3 and 4.
        assert_eq!(p, vec![0.0, 0.625, 0.625, 1.0, 0.0, 0.25]);
        assert_eq!(percentiles(&[Some(3.0), None]), vec![1.0, 0.0]);
        assert_eq!(percentiles(&[Some(0.0)]), vec![0.0]);
    }

    #[test]
    fn pareto_fronts() {
        let dims = [
            [1.0, 0.0, 0.0], // 1: nothing beats its size
            [0.0, 1.0, 0.0], // 1
            [0.5, 0.5, 0.0], // 1: neither extreme dominates it
            [0.4, 0.4, 0.0], // 2: below [0.5, 0.5, 0]
            [0.4, 0.4, 0.0], // 2: equal points don't dominate each other
            [0.1, 0.1, 0.0], // 3
            [0.0, 0.0, 0.0], // 4
        ];
        assert_eq!(fronts(&dims), vec![1, 1, 1, 2, 2, 3, 4]);
    }

    fn paths(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("f{i:03}.rs")).collect()
    }

    /// `levels` with every file past the bar and paths in index order.
    fn levels_of(dims: &[[f64; 3]]) -> Vec<Option<Level>> {
        let p = paths(dims.len());
        let p: Vec<&str> = p.iter().map(String::as_str).collect();
        levels(dims, &fronts(dims), &vec![true; dims.len()], &p)
    }

    #[test]
    fn levels_respect_caps_and_thresholds() {
        // 40 equally bad files: 5% hot (2), 10% warm (4), the rest none.
        let l = levels_of(&vec![[1.0, 1.0, 0.0]; 40]);
        let count = |lv| l.iter().filter(|&&x| x == Some(lv)).count();
        assert_eq!((count(Level::Hot), count(Level::Warm)), (2, 4));
        // Ties go by path: the first two files are hot, the next four warm.
        assert_eq!(&l[..2], &[Some(Level::Hot); 2]);
        assert_eq!(&l[2..6], &[Some(Level::Warm); 4]);

        // Nothing in the top decile: no hot, and below 0.75 no warm either.
        let l = levels_of(&vec![[0.8, 0.0, 0.0]; 25]);
        assert!(l.iter().all(|&x| x == Some(Level::Warm) || x.is_none()));
        assert_eq!(l.iter().filter(|x| x.is_some()).count(), 3);
        assert!(
            levels_of(&vec![[0.7, 0.0, 0.0]; 25])
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn later_fronts_fill_hot_and_warm_goes_by_sum() {
        // A chain: each file dominates the one before, so every file is on
        // its own front. Hot still gets its 2 from fronts 1-2.
        let n = 40;
        let mut dims: Vec<[f64; 3]> = (0..n)
            .map(|i| [0.5 + 0.5 * i as f64 / (n - 1) as f64, 0.0, 0.0])
            .collect();
        let fr = fronts(&dims);
        assert_eq!((fr[n - 1], fr[n - 2], fr[0]), (1, 2, n));
        let l = levels_of(&dims);
        assert_eq!(&l[n - 2..], &[Some(Level::Hot); 2]);
        assert_eq!(&l[n - 6..n - 2], &[Some(Level::Warm); 4]);
        assert!(l[..n - 6].iter().all(Option::is_none));
        // Warm is by sum, not front: a front-1 file with one 0.8 dimension
        // loses to dominated files with larger sums.
        dims.push([0.0, 0.8, 0.0]);
        let l = levels_of(&dims);
        assert_eq!(fronts(&dims)[n], 1);
        assert_eq!(l[n], None);
    }

    #[test]
    fn the_bar_keeps_tidy_files_unlevelled() {
        let dims = vec![[1.0, 1.0, 0.0]; 40];
        let p = paths(40);
        let p: Vec<&str> = p.iter().map(String::as_str).collect();
        let mut bar = vec![false; 40];
        bar[7] = true;
        let l = levels(&dims, &fronts(&dims), &bar, &p);
        assert_eq!(l[7], Some(Level::Hot));
        assert_eq!(l.iter().filter(|x| x.is_some()).count(), 1);
    }

    #[test]
    fn small_projects_get_hot_only() {
        let dims: Vec<[f64; 3]> = (0..19).map(|i| [i as f64 / 18.0, 0.8, 0.0]).collect();
        let l = levels_of(&dims);
        assert_eq!(l[18], Some(Level::Hot));
        assert_eq!(l.iter().filter(|x| x.is_some()).count(), 1);
        // 20 files are enough for warm.
        let l = levels_of(&vec![[0.8, 0.0, 0.0]; 20]);
        assert_eq!(l.iter().filter(|&&x| x == Some(Level::Warm)).count(), 2);
    }

    #[test]
    fn hot_goes_first_to_more_top_decile_dims() {
        // Same front; the first has two dims >= 0.9, the second one.
        let mut dims = vec![[0.9, 0.9, 0.0], [1.0, 0.0, 0.85]];
        dims.extend(vec![[0.0; 3]; 18]);
        let l = levels_of(&dims);
        assert_eq!((l[0], l[1]), (Some(Level::Hot), Some(Level::Warm)));
    }

    fn file(lines: usize, cc: u32, panics: u32, impl_lines: usize) -> FileMetrics {
        let peak = |value| {
            Some(FnPeak {
                name: "<Node<A, B> as Build<C>>::build".into(),
                line: 1,
                value,
                arm: None,
            })
        };
        FileMetrics {
            largest_fn: Some(FnSize {
                name: "build".into(),
                line: 1,
                lines,
                arm: None,
            }),
            max_complexity: peak(cc),
            max_nesting: peak(1),
            most_panics: None,
            panics,
            panic_density: (impl_lines >= 30).then(|| panics as f64 * 100.0 / impl_lines as f64),
            clones: 0,
            excused: Vec::new(),
        }
    }

    fn run(files: &[(FileMetrics, usize)]) -> Vec<Smell> {
        let inputs: Vec<Input> = files
            .iter()
            .map(|(m, l)| Input {
                impl_lines: *l,
                metrics: m,
            })
            .collect();
        let p = paths(inputs.len());
        let p: Vec<&str> = p.iter().map(String::as_str).collect();
        score(&inputs, &p)
    }

    #[test]
    fn fragility_needs_two_dense_panics_and_bands_when_few_qualify() {
        let mut files: Vec<(FileMetrics, usize)> =
            (0..20).map(|_| (file(10, 2, 0, 40), 40)).collect();
        files.push((file(10, 2, 1, 30), 30)); // 20: one dense `unwrap`
        files.push((file(10, 2, 2, 400), 400)); // 21: 0.5/100, too sparse
        files.push((file(10, 2, 4, 100), 100)); // 22: 4.0/100
        files.push((file(10, 2, 2, 100), 100)); // 23: 2.0/100
        let s = run(&files);
        assert_eq!((s[20].fragility, s[21].fragility), (0.0, 0.0));
        assert!(s[20].level.is_none() && s[20].why.is_none());
        // Two qualifying files: bands, not ranks.
        assert_eq!((s[22].fragility, s[23].fragility), (0.9, 0.75));
        assert_eq!(s[22].level, Some(Level::Hot));
        let why = s[22].why.as_deref().unwrap();
        assert!(
            why.ends_with("fragility: 4.0 panics/100 lines (4 panics)"),
            "{why}"
        );
        // Five or more qualifying files are ranked among themselves.
        for p in [3, 5, 6] {
            files.push((file(10, 2, p, 100), 100));
        }
        let s = run(&files);
        assert_eq!(s[23].fragility, 0.0);
        assert_eq!(s[26].fragility, 1.0);
        assert_eq!(s[20].fragility, 0.0);
    }

    #[test]
    fn a_lone_trivial_file_gets_no_level() {
        let s = run(&[(file(12, 2, 0, 15), 15)]);
        assert_eq!((s[0].size, s[0].complexity), (1.0, 1.0));
        assert_eq!((s[0].level, &s[0].why), (None, &None));
    }

    #[test]
    fn shallow_nesting_adds_no_complexity() {
        let mut files: Vec<(FileMetrics, usize)> =
            (0..10).map(|_| (file(10, 0, 0, 40), 40)).collect();
        files[3].0.max_nesting.as_mut().unwrap().value = 3;
        files[4].0.max_nesting.as_mut().unwrap().value = 5;
        let s = run(&files);
        assert_eq!((s[3].complexity, s[4].complexity), (0.0, 1.0));
    }

    #[test]
    fn why_names_the_dimensions_and_their_evidence() {
        let files: Vec<(FileMetrics, usize)> = (0..10)
            .map(|i| (file(10 + i, 2 + i as u32, 0, 40 + i), 40 + i))
            .chain([(file(225, 41, 12, 400), 400)])
            .collect();
        let s = run(&files);
        let worst = &s[10];
        assert_eq!(worst.level, Some(Level::Hot));
        assert_eq!(worst.front, 1);
        // The only fragile file: banded.
        assert_eq!(
            (worst.size, worst.complexity, worst.fragility),
            (1.0, 1.0, 0.9)
        );
        assert_eq!(
            worst.why.as_deref(),
            Some(
                "size: largest fn 225 lines (build), 400 impl lines; complexity: cyclomatic complexity 41 (<Node<…> as Build<…>>::build); fragility: 3.0 panics/100 lines (12 panics)"
            )
        );
        assert!(s[0].why.is_none() && s[0].level.is_none() && s[0].dims.is_empty());
        let v = serde_json::to_value(worst).unwrap();
        let metrics: Vec<(&str, &str)> = v["dims"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["dim"].as_str().unwrap(), d["metric"].as_str().unwrap()))
            .collect();
        assert_eq!(
            metrics,
            [
                ("size", "largest_fn"),
                ("size", "impl_lines"),
                ("complexity", "cyclomatic_complexity"),
                ("fragility", "panic_density"),
            ]
        );
        assert_eq!(v["dims"][0]["value"], 225);
        assert_eq!(v["dims"][2]["name"], "<Node<A, B> as Build<C>>::build");
        assert_eq!(v["dims"][2]["line"], 1);
        assert_eq!(
            (&v["dims"][3]["value"], &v["dims"][3]["panics"]),
            (&3.0.into(), &12.into())
        );
        assert!(v["dims"][3].get("name").is_none());
        assert_eq!(v["dims"][3]["dim"], "fragility");
    }

    fn to(v: &[(FileMetrics, usize)]) -> Vec<Input<'_>> {
        v.iter()
            .map(|(m, l)| Input {
                impl_lines: *l,
                metrics: m,
            })
            .collect()
    }

    fn stand(value: u32) -> Option<crate::rollup::Stand> {
        Some(crate::rollup::Stand { value, arm: None })
    }

    /// `build`, allowing too_many_lines.
    fn allowed(own: [u32; 3]) -> FnScore {
        FnScore {
            name: "build".into(),
            line: 1,
            own,
            effective: [None, stand(own[1]), stand(own[2])],
            excuses: vec![Excuse::Allowed {
                lint: crate::shape::Lint::TooManyLines,
            }],
            worst_arms: None,
        }
    }

    /// 20 ordinary files (largest fns from `lines` up) and `last`, whose
    /// fn `build` is `allowed(own)` and effectively 12 lines long; scored
    /// with the excuses.
    fn score_with_allowed(lines: usize, last: (FileMetrics, usize), own: [u32; 3]) -> Vec<Smell> {
        let mut raw: Vec<(FileMetrics, usize)> =
            (0..20).map(|i| (file(lines + i, 2, 0, 40), 40)).collect();
        raw.push(last);
        raw[20].0.excused.push(allowed(own));
        let mut eff = raw.clone();
        eff[20].0.largest_fn = Some(FnSize {
            name: "small".into(),
            line: 200,
            lines: 12,
            arm: None,
        });
        let p = paths(raw.len());
        let p: Vec<&str> = p.iter().map(String::as_str).collect();
        score_excusing(&to(&raw), &to(&eff), &p)
    }

    #[test]
    fn excused_fns_past_the_bar_note_a_file_levelled_by_impl_lines() {
        // Levelled by 2000 impl lines alone; its 45-line fn is excused and
        // ranks below the other files' fns, so only the bar names it.
        let s = score_with_allowed(50, (file(45, 2, 0, 2000), 2000), [45, 2, 1]);
        assert_eq!(s[20].level, Some(Level::Noted));
        assert_eq!(
            s[20].why.as_deref(),
            Some("noted: largest fn 45 lines (build) [allows too_many_lines]")
        );
    }

    #[test]
    fn hot_files_keep_noted_reasons_about_other_measures() {
        let s = score_with_allowed(10, (file(120, 41, 0, 130), 130), [120, 41, 1]);
        assert_eq!(s[20].level, Some(Level::Hot));
        assert_eq!(
            s[20].why.as_deref(),
            Some(
                "size: 130 impl lines; complexity: cyclomatic complexity 41 (<Node<…> as Build<…>>::build); noted: largest fn 120 lines (build) [allows too_many_lines]"
            )
        );
    }

    #[test]
    fn excused_signals_make_a_file_noted_not_hot() {
        use crate::rollup::FnSize;
        use crate::shape::Lint;
        // 20 ordinary files and two with a 120-line fn: one allows
        // too_many_lines, so effectively its largest fn is 12 lines.
        let mut raw: Vec<(FileMetrics, usize)> =
            (0..20).map(|i| (file(10 + i, 2, 0, 40), 40)).collect();
        raw.push((file(120, 2, 0, 130), 130));
        raw.push((file(120, 2, 0, 130), 130));
        raw[21].0.excused.push(FnScore {
            name: "build".into(),
            line: 1,
            own: [120, 2, 1],
            effective: [stand(12), stand(2), stand(1)],
            excuses: vec![Excuse::Allowed {
                lint: Lint::TooManyLines,
            }],
            worst_arms: None,
        });
        let mut eff: Vec<(FileMetrics, usize)> = raw.clone();
        eff[21].0.largest_fn = Some(FnSize {
            name: "small".into(),
            line: 200,
            lines: 12,
            arm: None,
        });
        let p = paths(raw.len());
        let p: Vec<&str> = p.iter().map(String::as_str).collect();
        let s = score_excusing(&to(&raw), &to(&eff), &p);
        assert_eq!(s[20].level, Some(Level::Hot));
        assert!(s[20].dims.iter().all(|d| d.excused.is_none()));
        assert_eq!(s[21].level, Some(Level::Noted));
        let noted = &s[21].dims[0];
        assert_eq!(noted.dim, Dim::Size);
        assert_eq!(
            noted.excused,
            Some(Excuse::Allowed {
                lint: Lint::TooManyLines
            })
        );
        assert_eq!(
            s[21].why.as_deref(),
            Some("noted: largest fn 120 lines (build) [allows too_many_lines]")
        );
        let v = serde_json::to_value(&s[21]).unwrap();
        assert_eq!(v["level"], "noted");
        assert_eq!(v["dims"][0]["excused"]["kind"], "allowed");
        assert_eq!(v["dims"][0]["excused"]["lint"], "too_many_lines");
        // Excused fns are never culprits, however long.
        assert!(v["dims"][0].get("culprit").is_none());
        // Impl lines aren't excused: not among the noted reasons.
        assert!(s[21].dims.iter().all(|d| d.excused.is_some()));

        // The same file as a dispatch whose worst arm has 70 lines: the
        // arm keeps it hot, whatever the ranking without it.
        let arm = |value| {
            Some(crate::rollup::ArmAt {
                name: "Op::Big".into(),
                line: 30,
                value,
            })
        };
        let x = &mut raw[21].0.excused[0];
        x.excuses = vec![Excuse::Dispatch {
            arms: 5,
            repeat: None,
        }];
        x.worst_arms = Some([arm(70), arm(3), arm(1)]);
        // Effectively the remainder (5 lines) and the counted arm.
        x.effective[0] = stand(75);
        let s = score_excusing(&to(&raw), &to(&eff), &p);
        assert_eq!(s[21].level, Some(Level::Hot));
        assert_eq!(
            s[21].why.as_deref(),
            Some("size: largest fn 75 lines (build, arm Op::Big)")
        );
        let v = serde_json::to_value(&s[21]).unwrap();
        assert_eq!(v["dims"][0]["arm"]["value"], 70);
        assert_eq!(v["dims"][0]["arm"]["line"], 30);
    }
}
