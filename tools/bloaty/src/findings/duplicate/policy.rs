//! How a group of copies is reported: one row of a table picked by the
//! group's facts, giving its tier (an actionable finding, or a note shown
//! only in lists) and a hint. Hints are advice, never a claim that a
//! refactor compiles: bloaty doesn't know visibility, types or re-exports.

use crate::visit::sig::Part;
use serde::Serialize;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// The same code more than once (equal body, same written signature): worth acting on (marked in the editor).
    Finding,
    /// Equal bodies that are plausibly fine: listed after findings, unmarked.
    Note,
}

/// Declaration order is the order `Rule::of` tries them in; the first whose
/// fact holds applies.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// Equal bodies, different signatures (`data(self) -> Vec<u8>` /
    /// `as_data(&self) -> &[u8]`, `Node<A>::f` / `Node<B>::f`).
    Signatures,
    /// Same shape under different cfgs in one crate: platform variants.
    CfgVariants,
    /// Identical under different cfgs (deliberate, but still the same
    /// code twice: a finding).
    CfgIdentical,
    /// The same code in several crates.
    Crates,
    /// The same code, identical body.
    Identical,
    /// The same code, same-shape body (names or literals differ).
    SameShape,
}

/// What a group of copies is, as single facts.
pub(super) struct Facts {
    pub identical: bool,
    /// Signature parts that differ between copies (empty: one written signature).
    pub differs: Vec<Part>,
    /// Not every copy has the same cfgs (as written); only within one crate.
    pub cfgs_differ: bool,
    pub crates: usize,
}

type Applies = fn(&Facts) -> bool;

/// The table: each rule's fact. `SameShape` holds for anything left.
const TABLE: [(Rule, Applies); 6] = [
    (Rule::Signatures, |f| !f.differs.is_empty()),
    (Rule::CfgVariants, |f| f.cfgs_differ && !f.identical),
    (Rule::CfgIdentical, |f| f.cfgs_differ),
    (Rule::Crates, |f| f.crates > 1),
    (Rule::Identical, |f| f.identical),
    (Rule::SameShape, |_| true),
];

impl Rule {
    pub(super) fn of(facts: &Facts) -> Rule {
        TABLE
            .iter()
            .find(|(_, applies)| applies(facts))
            .map_or(Rule::SameShape, |&(r, _)| r)
    }

    pub fn tier(self) -> Tier {
        match self {
            Rule::Signatures | Rule::CfgVariants => Tier::Note,
            Rule::CfgIdentical | Rule::Crates | Rule::Identical | Rule::SameShape => Tier::Finding,
        }
    }

    /// What the group is, after its number of copies (the detail's lead).
    pub fn text(self, identical: bool) -> &'static str {
        match self {
            Rule::Crates if !identical => "fns with the same shape (names/literals differ)",
            Rule::Signatures => "equal bodies under different signatures",
            Rule::CfgVariants => "same-shape bodies under different cfgs (cfg variants)",
            Rule::CfgIdentical => "identical fns under different cfgs",
            Rule::Crates | Rule::Identical => "identical fns",
            Rule::SameShape => "fns with the same shape (names/literals differ)",
        }
    }

    /// Advice, if any (for `Signatures`, see `part_hint`).
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Rule::Signatures | Rule::CfgVariants => None,
            Rule::CfgIdentical => Some("one fn without the cfg split may do"),
            Rule::Crates => Some("move it into a shared crate, or reuse it through a dependency"),
            Rule::Identical => Some("a helper both can call"),
            Rule::SameShape => Some("a parameterised helper"),
        }
    }
}

/// Advice for a body repeated under signatures differing in `part`.
pub(super) fn part_hint(part: Part) -> Option<&'static str> {
    match part {
        Part::SelfType => Some(
            "same body on different types: a shared helper or a generic impl may fit; often by design",
        ),
        Part::Trait => Some("maybe one impl delegating to the other"),
        Part::Receiver => Some("an owned/borrowed pair, often by design"),
        Part::ImplGenerics | Part::Generics | Part::Params | Part::Return | Part::Qualifiers => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::tests::*;
    use crate::findings::{Finding, Kind};
    use clap::ValueEnum;

    /// A workspace where the rule applies to a reported group (`src`), and
    /// a near miss where it applies to none (`counter`).
    struct Example {
        src: &'static [(&'static str, &'static str)],
        counter: &'static [(&'static str, &'static str)],
    }

    const PKG: &str = "[package]\nname = \"m\"\n";

    /// A body long enough to be compared (min_dup is 4 in tests).
    macro_rules! body {
        () => {
            "{\n    let a = x + 1;\n    let b = a * 2;\n    b - a\n}\n"
        };
    }
    macro_rules! two {
        ($a:literal, $b:literal) => {
            &[
                ("Cargo.toml", PKG),
                ("src/lib.rs", concat!($a, " ", body!(), $b, " ", body!())),
            ]
        };
    }

    /// A fn of 15 lines (same shape needs that many), named by `$v`, `$lit`.
    macro_rules! long {
        ($fn:literal, $v:literal, $lit:literal) => {
            concat!(
                $fn,
                "(x: u32) -> u32 {\n    let ",
                $v,
                " = x + ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " * ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " - ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " + ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " * ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " - ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " + ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " * ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " - ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " + ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " * ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " - ",
                $lit,
                ";\n",
                "    let ",
                $v,
                " = ",
                $v,
                " + ",
                $lit,
                ";\n",
                "    ",
                $v,
                "\n}\n"
            )
        };
    }

    /// No wildcard: a new rule doesn't compile without its examples.
    fn example(r: Rule) -> Example {
        match r {
            Rule::Signatures => Example {
                src: two!("fn a(x: u32) -> u32", "fn b(x: u64) -> u64"),
                counter: two!("fn a(x: u32) -> u32", "fn b(x: u32) -> u32"),
            },
            Rule::CfgVariants => Example {
                src: &[
                    ("Cargo.toml", PKG),
                    (
                        "src/lib.rs",
                        concat!(
                            long!("#[cfg(unix)]\nfn j", "a", "1"),
                            long!("#[cfg(windows)]\nfn j", "b", "2")
                        ),
                    ),
                ],
                counter: two!(
                    "#[cfg(unix)]\nfn a(x: u32) -> u32",
                    "#[cfg(windows)]\nfn b(x: u32) -> u32"
                ),
            },
            Rule::CfgIdentical => Example {
                src: two!(
                    "#[cfg(unix)]\nfn a(x: u32) -> u32",
                    "#[cfg(windows)]\nfn b(x: u32) -> u32"
                ),
                counter: two!(
                    "#[cfg(unix)]\nfn a(x: u32) -> u32",
                    "#[cfg(unix)]\nfn b(x: u32) -> u32"
                ),
            },
            Rule::Crates => Example {
                src: &[
                    ("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n"),
                    ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
                    ("a/src/lib.rs", concat!("fn f(x: u32) -> u32 ", body!())),
                    ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
                    ("b/src/lib.rs", concat!("fn g(x: u32) -> u32 ", body!())),
                ],
                counter: two!("fn a(x: u32) -> u32", "fn b(x: u32) -> u32"),
            },
            Rule::Identical => Example {
                src: two!("fn a(x: u32) -> u32", "fn b(x: u32) -> u32"),
                counter: two!("fn a(x: u32) -> u32", "fn b(x: u64) -> u64"),
            },
            Rule::SameShape => Example {
                src: &[
                    ("Cargo.toml", PKG),
                    (
                        "src/lib.rs",
                        concat!(long!("fn j", "a", "1"), long!("fn k", "b", "2")),
                    ),
                ],
                counter: two!("fn a(x: u32) -> u32", "fn b(x: u32) -> u32"),
            },
        }
    }

    fn dups(files: &[(&str, &str)]) -> Vec<Finding> {
        run(&Fixture::new(files), true)
            .all
            .into_iter()
            .filter(|f| f.kind == Kind::Duplicate)
            .collect()
    }

    #[test]
    fn every_rule_has_a_case_and_a_near_miss() {
        for &r in Rule::value_variants() {
            let ex = example(r);
            let found = dups(ex.src);
            assert!(
                found
                    .iter()
                    .any(|f| f.dup.as_ref().map(|d| d.rule) == Some(r)),
                "{r:?}: {found:#?}"
            );
            let found = dups(ex.counter);
            assert!(
                !found.is_empty()
                    && found
                        .iter()
                        .all(|f| f.dup.as_ref().map(|d| d.rule) != Some(r)),
                "{r:?} counter: {found:#?}"
            );
        }
    }
}
