//! The per-fn measures the smell score uses, and everything that varies by
//! measure: JSON names, text, and the bars.

use crate::shape::Lint;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Measure {
    /// Own code lines.
    Lines,
    CyclomaticComplexity,
    Nesting,
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
    pub const ALL: [Measure; 3] = [
        Measure::Lines,
        Measure::CyclomaticComplexity,
        Measure::Nesting,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn dim(self) -> Dim {
        match self {
            Measure::Lines => Dim::Size,
            Measure::CyclomaticComplexity | Measure::Nesting => Dim::Complexity,
        }
    }

    /// The `metric` of its smell reasons.
    pub fn metric(self) -> &'static str {
        match self {
            Measure::Lines => "largest_fn",
            Measure::CyclomaticComplexity => "cyclomatic_complexity",
            Measure::Nesting => "nesting",
        }
    }

    /// `largest fn 225 lines`, `cyclomatic complexity 41`, `nesting 5`.
    pub fn text(self, value: u32) -> String {
        match self {
            Measure::Lines => format!("largest fn {value} lines"),
            Measure::CyclomaticComplexity => format!("cyclomatic complexity {value}"),
            Measure::Nesting => format!("nesting {value}"),
        }
    }

    /// The absolute bar: a file needs a fn at or past one (or panics) for
    /// any level. For nesting, also where it starts to count.
    pub fn bar(self) -> u32 {
        match self {
            Measure::Lines => 40,
            Measure::CyclomaticComplexity => 10,
            Measure::Nesting => 4,
        }
    }

    /// A dispatch arm at or past this is bad, ranking aside; also the lens's
    /// floor for marking a culprit.
    pub fn bad_arm(self) -> u32 {
        match self {
            Measure::Lines => 60,
            Measure::CyclomaticComplexity => 8,
            Measure::Nesting => 4,
        }
    }

    /// An allowed clippy lint (see `lint`) excuses a fn only below this:
    /// twice the bar.
    pub fn allow_ceiling(self) -> u32 {
        2 * self.bar()
    }

    /// The clippy lint whose `allow` excuses it, if any: nesting has none.
    pub fn lint(self) -> Option<Lint> {
        match self {
            Measure::Lines => Some(Lint::TooManyLines),
            Measure::CyclomaticComplexity => Some(Lint::CognitiveComplexity),
            Measure::Nesting => None,
        }
    }
}
