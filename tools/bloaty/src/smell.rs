//! Relative smell score per file: each signal becomes a percentile within
//! the workspace, signals collapse into three dimensions (size, complexity,
//! fragility), and non-dominated sorting ranks files on those. No weights:
//! levels walk the Pareto fronts, so a file dominated on every axis by
//! another never ranks above it.

use crate::findings::collapse_generics;
use crate::metrics::{FileMetrics, file_metrics};
use crate::scan::Scan;
use crate::summary::file_counts;
use serde::Serialize;
use std::cmp::Reverse;

/// Files smaller than this aren't scored: too little code to smell.
const MIN_IMPL_LINES: usize = 10;
/// Projects with fewer scored files get no `warm`: too few to rank finely.
const MIN_FILES_FOR_WARM: usize = 20;
/// Directories show their worst child, so even 10% hot files would light
/// up nearly every directory.
const HOT_SHARE: f64 = 0.05;
const WARM_SHARE: f64 = 0.10;
/// A dimension at or above this is in the project's top decile.
const HOT_DIM: f64 = 0.9;
const WARM_DIM: f64 = 0.75;
/// Nesting below this is ordinary code (ties at depths 2-3 would otherwise
/// read as top decile): it adds nothing to the complexity dimension.
const MIN_NESTING: u32 = 4;
/// A file has a fragility signal only with this many panic sites and a
/// density (it has >= 30 impl lines) of at least `MIN_DENSITY` per 100.
const MIN_PANICS: u32 = 2;
const MIN_DENSITY: f64 = 1.0;
/// With fewer qualifying files than this, ranks are noise: fixed bands by
/// density are used instead.
const MIN_RANKED_FRAGILE: usize = 5;
/// The absolute bar for any level: at least one raw signal past these (or
/// a qualifying fragility), so a tidy project has no hot files.
const BAR_FN_LINES: usize = 40;
const BAR_COMPLEXITY: u32 = 10;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Hot,
    Warm,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Smell {
    pub level: Option<Level>,
    /// Pareto front: 1 means no other file is at least as bad on every
    /// dimension and worse on one.
    pub front: usize,
    /// Percentiles within the workspace, in [0, 1].
    pub size: f64,
    pub complexity: f64,
    pub fragility: f64,
    /// For files with a level: the dimensions that put it there, with their
    /// raw numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One scored file's raw signals.
pub struct Input<'a> {
    pub impl_lines: usize,
    pub metrics: &'a FileMetrics,
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
        let impl_lines = file_counts(f).implementation;
        if impl_lines < MIN_IMPL_LINES {
            continue;
        }
        if let Some(m) = file_metrics(f, impl_lines).filter(|m| m.largest_fn.is_some()) {
            idx.push(i);
            metrics.push((impl_lines, m));
        }
    }
    let inputs: Vec<Input> = metrics
        .iter()
        .map(|(impl_lines, m)| Input {
            impl_lines: *impl_lines,
            metrics: m,
        })
        .collect();
    // Ties in the ranking go by path.
    let paths: Vec<&str> = idx.iter().map(|&i| scan.files[i].rel.as_str()).collect();
    let mut out = vec![None; scan.files.len()];
    for (i, s) in idx.into_iter().zip(score(&inputs, &paths)) {
        out[i] = Some(s);
    }
    out
}

/// Per-signal percentiles of every input, before they collapse.
struct Pcts {
    largest: Vec<f64>,
    lines: Vec<f64>,
    cc: Vec<f64>,
    nest: Vec<f64>,
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

/// Whether any raw signal is bad in absolute terms: percentiles alone would
/// call the largest file of a tidy project hot.
fn past_bar(i: &Input) -> bool {
    let m = i.metrics;
    m.largest_fn
        .as_ref()
        .is_some_and(|f| f.lines >= BAR_FN_LINES)
        || m.max_complexity
            .as_ref()
            .is_some_and(|f| f.value >= BAR_COMPLEXITY)
        || m.max_nesting
            .as_ref()
            .is_some_and(|f| f.value >= MIN_NESTING)
        || qualifies_fragile(i)
}

pub fn score(inputs: &[Input], paths: &[&str]) -> Vec<Smell> {
    let pct =
        |f: &dyn Fn(&Input) -> Option<f64>| percentiles(&inputs.iter().map(f).collect::<Vec<_>>());
    let fragile = inputs.iter().filter(|i| qualifies_fragile(i)).count();
    let p = Pcts {
        largest: pct(&|i| i.metrics.largest_fn.as_ref().map(|f| f.lines as f64)),
        lines: pct(&|i| Some(i.impl_lines as f64)),
        cc: pct(&|i| {
            i.metrics
                .max_complexity
                .as_ref()
                .map(|f| f64::from(f.value))
        }),
        // Ranked among all files, then zeroed below `MIN_NESTING`.
        nest: pct(&|i| i.metrics.max_nesting.as_ref().map(|f| f64::from(f.value)))
            .into_iter()
            .zip(inputs)
            .map(|(p, i)| {
                let deep = i
                    .metrics
                    .max_nesting
                    .as_ref()
                    .is_some_and(|f| f.value >= MIN_NESTING);
                if deep { p } else { 0.0 }
            })
            .collect(),
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
    let dims: Vec<[f64; 3]> = (0..inputs.len())
        .map(|k| {
            [
                round2(p.largest[k].max(p.lines[k])),
                round2(p.cc[k].max(p.nest[k])),
                round2(p.fragility[k]),
            ]
        })
        .collect();
    let bar: Vec<bool> = inputs.iter().map(past_bar).collect();
    let fronts = fronts(&dims);
    let levels = levels(&dims, &fronts, &bar, paths);
    (0..inputs.len())
        .map(|k| {
            let [size, complexity, fragility] = dims[k];
            Smell {
                level: levels[k],
                front: fronts[k],
                size,
                complexity,
                fragility,
                why: levels[k].map(|l| why(&inputs[k], &p, k, l)),
            }
        })
        .collect()
}

/// The dimensions at or above the level's threshold, each with the signals
/// behind it: `size: largest fn 225 lines (build); fragility: 3.1
/// panics/100 lines (12 panics)`.
fn why(input: &Input, p: &Pcts, k: usize, level: Level) -> String {
    let threshold = match level {
        Level::Hot => HOT_DIM,
        Level::Warm => WARM_DIM,
    };
    let above = |p: f64| round2(p) >= threshold;
    let m = input.metrics;
    let mut size = Vec::new();
    if let Some(f) = m.largest_fn.as_ref().filter(|_| above(p.largest[k])) {
        let name = collapse_generics(&f.name);
        size.push(format!("largest fn {} lines ({name})", f.lines));
    }
    if above(p.lines[k]) {
        size.push(format!("{} impl lines", input.impl_lines));
    }
    let mut complexity = Vec::new();
    if let Some(f) = m.max_complexity.as_ref().filter(|_| above(p.cc[k])) {
        complexity.push(format!("cc {} ({})", f.value, collapse_generics(&f.name)));
    }
    if let Some(f) = m.max_nesting.as_ref().filter(|_| above(p.nest[k])) {
        complexity.push(format!(
            "nesting {} ({})",
            f.value,
            collapse_generics(&f.name)
        ));
    }
    let mut fragility = Vec::new();
    if let Some(d) = m.panic_density.filter(|_| above(p.fragility[k])) {
        fragility.push(format!("{d:.1} panics/100 lines ({} panics)", m.panics));
    }
    [
        ("size", size),
        ("complexity", complexity),
        ("fragility", fragility),
    ]
    .into_iter()
    .filter(|(_, e)| !e.is_empty())
    .map(|(dim, e)| format!("{dim}: {}", e.join(", ")))
    .collect::<Vec<_>>()
    .join("; ")
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
    use crate::metrics::{FnPeak, FnSize};

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
    }

    fn file(lines: usize, cc: u32, panics: u32, impl_lines: usize) -> FileMetrics {
        let peak = |value| {
            Some(FnPeak {
                name: "<Node<A, B> as Build<C>>::build".into(),
                line: 1,
                value,
            })
        };
        FileMetrics {
            largest_fn: Some(FnSize {
                name: "build".into(),
                line: 1,
                lines,
            }),
            max_complexity: peak(cc),
            max_nesting: peak(1),
            panics,
            panic_density: (impl_lines >= 30).then(|| panics as f64 * 100.0 / impl_lines as f64),
            clones: 0,
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
                "size: largest fn 225 lines (build), 400 impl lines; complexity: cc 41 (<Node<…> as Build<…>>::build); fragility: 3.0 panics/100 lines (12 panics)"
            )
        );
        assert!(s[0].why.is_none() && s[0].level.is_none());
    }
}
