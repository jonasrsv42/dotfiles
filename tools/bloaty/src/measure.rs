//! The per-fn measures the smell score uses, and everything that varies by
//! measure: JSON names, text, and the bars.

use crate::fnmetrics::FnMetrics;
use crate::shape::Lint;

/// A fn (or dispatch arm) is *flat* when nothing in it is nested: its
/// largest single cognitive increment (`FnMetrics::worst`) is at most this.
/// A culprit-sized flat fn is long but simple: it never makes a file hot.
pub const FLAT_MAX_INCREMENT: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Measure {
    /// Own code lines.
    Lines,
    /// `FnMetrics::cognitive_complexity`. Cyclomatic complexity and nesting
    /// are reported, never scored.
    CognitiveComplexity,
}

/// A smell dimension.
#[derive(serde::Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Dim {
    Size,
    Complexity,
    Fragility,
}

impl Dim {
    pub const ALL: [Dim; 3] = [Dim::Size, Dim::Complexity, Dim::Fragility];

    pub fn as_str(self) -> &'static str {
        match self {
            Dim::Size => "size",
            Dim::Complexity => "complexity",
            Dim::Fragility => "fragility",
        }
    }
}

impl Measure {
    pub const COUNT: usize = 2;
    pub const ALL: [Measure; Self::COUNT] = [Measure::Lines, Measure::CognitiveComplexity];

    pub fn index(self) -> usize {
        self as usize
    }

    /// A fn's (or arm's) number for this measure, given its code lines.
    pub fn of(self, lines: usize, m: &FnMetrics) -> u32 {
        match self {
            Measure::Lines => lines as u32,
            Measure::CognitiveComplexity => m.cognitive_complexity,
        }
    }

    pub fn dim(self) -> Dim {
        match self {
            Measure::Lines => Dim::Size,
            Measure::CognitiveComplexity => Dim::Complexity,
        }
    }

    /// The `metric` of its smell reasons.
    pub fn metric(self) -> &'static str {
        match self {
            Measure::Lines => "largest_fn",
            Measure::CognitiveComplexity => "cognitive_complexity",
        }
    }

    /// `largest fn 225 lines`, `cognitive complexity 41`.
    pub fn text(self, value: u32) -> String {
        match self {
            Measure::Lines => format!("largest fn {value} lines"),
            Measure::CognitiveComplexity => format!("cognitive complexity {value}"),
        }
    }

    /// The absolute bar: a file needs a fn at or past one (or panics) for
    /// any level.
    pub fn bar(self) -> u32 {
        match self {
            Measure::Lines => 40,
            Measure::CognitiveComplexity => 15,
        }
    }

    /// A dispatch arm at or past this is bad, ranking aside; also the
    /// culprit bar: a hot or warm file needs a real fn signal at or past it
    /// (or qualifying panics), so the lens has something to point at.
    pub fn bad_arm(self) -> u32 {
        match self {
            Measure::Lines => 60,
            Measure::CognitiveComplexity => 10,
        }
    }

    /// An allowed clippy lint (see `lint`) excuses a fn only below this:
    /// twice the bar.
    pub fn allow_ceiling(self) -> u32 {
        2 * self.bar()
    }

    /// The clippy lint whose `allow` excuses it.
    pub fn lint(self) -> Lint {
        match self {
            Measure::Lines => Lint::TooManyLines,
            Measure::CognitiveComplexity => Lint::CognitiveComplexity,
        }
    }
}
