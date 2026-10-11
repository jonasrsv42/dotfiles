//! Per-file roll-ups of the per-fn measures: the worst fns, panic sites and
//! clones of a file (`summary --by file`), as they are and as the smell
//! score sees them (excuses applied). Nothing is ranked here.

use crate::fnmetrics::{FnMetrics, PanicSite};
use crate::measure::Measure;
use crate::scan::FileAnalysis;
use crate::shape::{Arm, Dispatch, Lint, Repeat, Shape};
use crate::visit::FnFact;
use serde::Serialize;

/// Below this many impl lines a file gets no panic density: one `unwrap` in
/// a 10-line file isn't a hotspot.
const MIN_DENSITY_LINES: usize = 30;

/// The measured fns of a file: non-test, in a file bloaty could parse.
pub fn measured(f: &FileAnalysis) -> impl Iterator<Item = (&FnFact, FnMetrics)> {
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
    /// Scoring a dispatch: the arm that stands in for the fn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm: Option<ArmAt>,
}

#[derive(Serialize, Clone, Debug)]
pub struct FnPeak {
    pub name: String,
    pub line: usize,
    pub value: u32,
    /// Scoring a dispatch: the arm that stands in for the fn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm: Option<ArmAt>,
}

/// A dispatch's match arm: its pattern (`Call::Donate`) and line.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ArmAt {
    pub name: String,
    pub line: usize,
    /// The arm's own number for the measure at hand.
    pub value: u32,
}

impl ArmAt {
    fn of(a: &Arm, value: u32) -> Self {
        ArmAt {
            name: a.name.clone(),
            line: a.line,
            value,
        }
    }
}

/// The arm of a dispatch that answers for measure `m`: the worst of its
/// counted (not small) arms by lines or cyclomatic complexity, of all arms
/// by nesting. None when every arm is small.
pub fn blamed_arm(d: &Dispatch, m: Measure) -> Option<(u32, &Arm)> {
    let counts = |a: &&Arm| m == Measure::Nesting || !a.small();
    worst_arm(d.arms.iter().filter(counts), arm_key(m))
}

/// A dispatch's arms that count (`Arm::small` ones are excused): their
/// summed lines and cyclomatic complexity.
pub fn counted(d: &Dispatch) -> (usize, u32, u32) {
    d.arms
        .iter()
        .filter(|a| !a.small())
        .fold((0, 0, 0), |(n, l, c), a| {
            (n + 1, l + a.lines as u32, c + a.metrics.complexity)
        })
}

/// Why some of a fn's numbers don't count toward the smell score.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Excuse {
    /// A table: the literal's lines are excused.
    Table { entries: usize },
    /// A dispatch: scored as its remainder plus its worst arm.
    Dispatch {
        arms: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        repeat: Option<Repeat>,
    },
    /// `#[allow(clippy::too_many_lines)]` (lines) or
    /// `#[allow(clippy::cognitive_complexity)]` (cyclomatic complexity and
    /// nesting), below `Measure::allow_ceiling`.
    Allowed { lint: Lint },
}

impl Excuse {
    fn covers(&self, m: Measure) -> bool {
        match self {
            Excuse::Table { .. } => m == Measure::Lines,
            Excuse::Dispatch { .. } => true,
            Excuse::Allowed { lint } => m.lint() == Some(*lint),
        }
    }
}

/// What stands for a fn in a peak: a value, and the dispatch arm to blame
/// when the arm outweighs the rest of the fn.
#[derive(Clone, Debug, PartialEq)]
pub struct Stand {
    pub value: u32,
    pub arm: Option<ArmAt>,
}

/// A measured fn with excuses, as the smell score sees it.
#[derive(Clone, Debug)]
pub struct FnScore {
    pub name: String,
    pub line: usize,
    /// Its own numbers, by `Measure::index`.
    pub own: [u32; 3],
    /// What counts instead (`None`: excused away), by `Measure::index`.
    pub effective: [Option<Stand>; 3],
    /// Allowed lints below their ceiling, then its shape.
    pub excuses: Vec<Excuse>,
    /// A dispatch's blamed arm by each measure (`blamed_arm`).
    pub worst_arms: Option<[Option<ArmAt>; 3]>,
}

impl FnScore {
    /// The excuse that lowered measure `m`, if one did.
    pub fn excuse_for(&self, m: Measure) -> Option<&Excuse> {
        let i = m.index();
        let lowered = self.effective[i]
            .as_ref()
            .is_none_or(|s| s.value < self.own[i]);
        if !lowered {
            return None;
        }
        self.excuses.iter().find(|x| x.covers(m))
    }
}

/// The first arm with the greatest `key` (ties: the first in source order).
pub fn worst_arm<'a, T: Ord + Copy>(
    arms: impl IntoIterator<Item = &'a Arm>,
    key: impl Fn(&Arm) -> T,
) -> Option<(T, &'a Arm)> {
    let mut best: Option<(T, &Arm)> = None;
    for a in arms {
        let v = key(a);
        if best.is_none_or(|(b, _)| v > b) {
            best = Some((v, a));
        }
    }
    best
}

pub fn arm_key(m: Measure) -> impl Fn(&Arm) -> u32 {
    move |a: &Arm| match m {
        Measure::Lines => a.lines as u32,
        Measure::CyclomaticComplexity => a.metrics.complexity,
        Measure::Nesting => a.metrics.nesting,
    }
}

/// The fn's excuses and what its numbers become under them, or `None`
/// without excuses.
fn score_fn(func: &FnFact, lines: usize, m: &FnMetrics) -> Option<FnScore> {
    let own = [lines as u32, m.complexity, m.nesting];
    let mut excuses: Vec<Excuse> = func
        .allowed
        .iter()
        .filter(|&&lint| {
            Measure::ALL
                .iter()
                .any(|&x| x.lint() == Some(lint) && own[x.index()] < x.allow_ceiling())
        })
        .map(|&lint| Excuse::Allowed { lint })
        .collect();
    match &func.shape {
        Some(Shape::Table(t)) => excuses.push(Excuse::Table { entries: t.entries }),
        Some(Shape::Dispatch(d)) => excuses.push(Excuse::Dispatch {
            arms: d.arms.len(),
            repeat: d.repeat.clone(),
        }),
        None => {}
    }
    if excuses.is_empty() {
        return None;
    }
    let worst_arms = match &func.shape {
        Some(Shape::Dispatch(d)) => {
            Some(Measure::ALL.map(|x| blamed_arm(d, x).map(|(v, a)| ArmAt::of(a, v))))
        }
        _ => None,
    };
    let effective = Measure::ALL.map(|x| {
        let i = x.index();
        let allowed = func
            .allowed
            .iter()
            .any(|&l| x.lint() == Some(l) && own[i] < x.allow_ceiling());
        if allowed {
            return None;
        }
        let plain = Stand {
            value: own[i],
            arm: None,
        };
        Some(match (&func.shape, &worst_arms) {
            (Some(Shape::Table(t)), _) if x == Measure::Lines => Stand {
                value: own[i].saturating_sub(t.lines as u32),
                arm: None,
            },
            (Some(Shape::Dispatch(d)), Some(w)) => {
                let (n, lines, cc) = counted(d);
                let rest = match x {
                    Measure::Lines => own[i].saturating_sub(d.lines as u32),
                    Measure::CyclomaticComplexity => d.remainder.complexity,
                    Measure::Nesting => d.remainder.nesting,
                };
                let value = match x {
                    Measure::Lines => rest + lines,
                    // The match's own decision, and each counted arm's
                    // branches (an arm's base 1 is the match's): the fn's
                    // own number when every arm counts.
                    Measure::CyclomaticComplexity => rest + 1 + cc.saturating_sub(n as u32),
                    Measure::Nesting => rest.max(w[i].as_ref().map_or(0, |a| a.value)),
                };
                Stand {
                    value,
                    arm: w[i].clone().filter(|a| a.value >= rest),
                }
            }
            _ => plain,
        })
    });
    Some(FnScore {
        name: func.name.clone(),
        line: func.line,
        own,
        effective,
        excuses,
        worst_arms,
    })
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
    #[serde(
        rename = "max_cyclomatic_complexity",
        skip_serializing_if = "Option::is_none"
    )]
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
    /// Its fns with excuses (see `effective_metrics`).
    #[serde(skip)]
    pub excused: Vec<FnScore>,
}

/// `None` for whole-test files and files whose syntax wasn't analysed.
/// The fns' own numbers: what the file is, excuses aside.
pub fn file_metrics(f: &FileAnalysis, impl_lines: usize) -> Option<FileMetrics> {
    roll_up(f, impl_lines, false)
}

/// Like `file_metrics`, but as the smell score sees the file: a table's
/// literal lines don't count; a dispatch counts as its remainder plus its
/// worst arm (lines and cyclomatic complexity added, nesting the deeper),
/// with `arm` set when the arm outweighs the remainder; a fn allowing
/// `clippy::too_many_lines` doesn't count for `largest_fn`, one allowing
/// `clippy::cognitive_complexity` not for the complexity peaks, below
/// twice the bar.
pub fn effective_metrics(f: &FileAnalysis, impl_lines: usize) -> Option<FileMetrics> {
    roll_up(f, impl_lines, true)
}

fn roll_up(f: &FileAnalysis, impl_lines: usize, excuse: bool) -> Option<FileMetrics> {
    if f.whole_test || f.unanalysed {
        return None;
    }
    let mut out = FileMetrics::default();
    let mut peaks: [Option<(Stand, &FnFact)>; 3] = [None, None, None];
    for (func, m) in measured(f) {
        let lines = f.own_lines(func);
        let score = score_fn(func, lines, &m);
        let own = [lines as u32, m.complexity, m.nesting];
        for x in Measure::ALL {
            let i = x.index();
            let stand = match (&score, excuse) {
                (Some(s), true) => s.effective[i].clone(),
                _ => Some(Stand {
                    value: own[i],
                    arm: None,
                }),
            };
            // Ties go to the first fn in the file.
            if let Some(stand) = stand
                && peaks[i].as_ref().is_none_or(|(p, _)| stand.value > p.value)
            {
                peaks[i] = Some((stand, func));
            }
        }
        out.excused.extend(score);
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
                    arm: None,
                },
                sites: func.panic_sites.clone(),
            });
        }
        out.panics += m.panics;
        out.clones += m.clones;
    }
    let [lines, cc, nest] = peaks;
    let peak = |p: Option<(Stand, &FnFact)>| {
        p.map(|(s, func)| FnPeak {
            name: func.name.clone(),
            line: func.line,
            value: s.value,
            arm: s.arm,
        })
    };
    out.largest_fn = lines.map(|(s, func)| FnSize {
        name: func.name.clone(),
        line: func.line,
        lines: s.value as usize,
        arm: s.arm,
    });
    out.max_complexity = peak(cc);
    out.max_nesting = peak(nest);
    out.panic_density = (impl_lines >= MIN_DENSITY_LINES)
        .then(|| (out.panics as f64 * 10000.0 / impl_lines as f64).round() / 100.0);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

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
    fn allowed_lints_excuse_up_to_twice_the_bar() {
        let long = |name: &str, n: usize| {
            let body: String = (0..n - 2)
                .map(|i| format!("    let _x{i} = {i};\n"))
                .collect();
            format!("#[allow(clippy::too_many_lines)]\nfn {name}() {{\n{body}}}\n")
        };
        let src = long("under", 79) + &long("over", 80);
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        let scan = fx.scan();
        let f = &scan.files[0];
        let m = effective_metrics(f, 100).unwrap();
        // `under` is excused, `over` (80 lines: twice the bar) is not.
        assert_eq!(
            m.largest_fn.map(|l| (l.name, l.lines)),
            Some(("over".into(), 80))
        );
        let excused: Vec<_> = m.excused.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(excused, ["under"]);
    }

    #[test]
    fn an_allowed_table_at_the_ceiling_is_excused_as_a_table() {
        // 80 lines: twice the bar, so the allow no longer holds.
        let entries: String = (0..76).map(|i| format!("        {i},\n")).collect();
        let src = format!(
            "#[allow(clippy::too_many_lines)]\nfn t() -> Vec<u32> {{\n    vec![\n{entries}    ]\n}}\n"
        );
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        let scan = fx.scan();
        let eff = effective_metrics(&scan.files[0], 100).unwrap();
        let x = &eff.excused[0];
        assert_eq!(x.own[0], 80);
        assert_eq!(x.excuses, [Excuse::Table { entries: 76 }]);
        // The literal's 78 lines are excused; complexity is not.
        let value = |i: usize| x.effective[i].as_ref().map(|s| s.value);
        assert_eq!((value(0), value(1)), (Some(2), Some(1)));
    }

    #[test]
    fn excuse_for_names_the_excuse_that_lowered_a_measure() {
        let stand = |value| Some(Stand { value, arm: None });
        let score = |effective, excuses| FnScore {
            name: "f".into(),
            line: 1,
            own: [50, 20, 2],
            effective,
            excuses,
            worst_arms: None,
        };
        let x = score(
            [None, stand(5), stand(2)],
            vec![
                Excuse::Allowed {
                    lint: Lint::TooManyLines,
                },
                Excuse::Dispatch {
                    arms: 4,
                    repeat: None,
                },
            ],
        );
        assert_eq!(
            x.excuse_for(Measure::CyclomaticComplexity),
            Some(&Excuse::Dispatch {
                arms: 4,
                repeat: None
            })
        );
        // Not lowered.
        assert_eq!(x.excuse_for(Measure::Nesting), None);
        let t = score(
            [stand(50), stand(5), stand(2)],
            vec![Excuse::Table { entries: 4 }],
        );
        assert_eq!(t.excuse_for(Measure::Lines), None);
        // Lowered, but a table excuses only lines.
        assert_eq!(t.excuse_for(Measure::CyclomaticComplexity), None);
    }

    #[test]
    fn dispatches_count_every_arm_that_isnt_small() {
        // Three heavy arms (16 lines, cc 4) behind five one-line ones.
        let heavy = |i: usize| {
            let body: String = (0..10)
                .map(|k| format!("            let _v{k} = x + {k};\n"))
                .collect();
            format!(
                "        Op::H{i} => {{\n            if x > 1 {{ return 1; }}\n            if x > 2 {{ return 2; }}\n            if x > 3 {{ return 3; }}\n{body}            x\n        }}\n"
            )
        };
        let small: String = (0..5)
            .map(|i| format!("        Op::S{i} => {i},\n"))
            .collect();
        let src = format!(
            "fn run(op: Op, x: u32) -> u32 {{\n    match op {{\n{}{}{}{}    }}\n}}\n",
            heavy(0),
            heavy(1),
            heavy(2),
            small
        );
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        let scan = fx.scan();
        let f = &scan.files[0];
        let raw = file_metrics(f, 60).unwrap();
        let eff = effective_metrics(f, 60).unwrap();
        let own = raw.largest_fn.unwrap().lines;
        // The remainder (match line, braces: 4) and the three heavy arms;
        // the five small arms are excused.
        let l = eff.largest_fn.unwrap();
        assert_eq!((own, l.lines), (57, 4 + 3 * 16));
        assert_eq!(
            l.arm.map(|a| (a.name, a.value)),
            Some(("Op::H0".into(), 16))
        );
        // Nesting counts from inside the deepest arm.
        assert_eq!(eff.max_nesting.map(|n| n.value), Some(1));
    }

    #[test]
    fn allowing_cognitive_complexity_leaves_nesting() {
        let src = "#[allow(clippy::cognitive_complexity)]\nfn deep(x: u8) -> u8 {\n    if x > 1 { if x > 2 { if x > 3 { if x > 4 { if x > 5 { return 5; } } } } }\n    0\n}\n";
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", src),
        ]);
        let scan = fx.scan();
        let eff = effective_metrics(&scan.files[0], 20).unwrap();
        // Cyclomatic complexity is excused, nesting (no clippy lint) isn't.
        assert!(eff.max_complexity.is_none());
        assert_eq!(eff.max_nesting.map(|n| n.value), Some(5));
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
