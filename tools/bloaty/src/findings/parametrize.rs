//! `parametrize`: tests of one scope whose bodies are identical except for
//! literals, as written: one test over a table of those literals (or a
//! helper taking them) could replace them. An exact syntactic claim: the
//! same file, inline `mod`, attributes and signature, distinct names (a
//! name written twice is cfg variants: at most one compiles), bodies with
//! equal names hashes (literals erased) and equal columns: per position
//! the same literal kind (`'a'` and `"a"` can't share a column) and the
//! same text where a literal isn't data (`p.0` tuple indices, `@"…"`
//! inline snapshots, input of a macro outside `EXPRESSION_MACROS`: bloaty's
//! one rule for opaque macros). Tests written
//! by macros other than item macros bloaty reads as items aren't seen;
//! `true`/`false` are identifiers to the tokenizer, so tests differing only
//! in booleans don't group.

use super::{Category, Cx, Finding, Kind};
use crate::names::Loc;
use crate::scan::FileAnalysis;
use crate::visit::FnFact;
use crate::visit::body::Literal;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A group this large counts at any size; a pair needs --min-dup lines
/// after the signature in each test (lines with a token: not those wholly
/// inside a multi-line literal).
pub(crate) const MIN_PARAMETRIZE_TESTS: usize = 3;

/// One test of a candidate group (parametrize, fixture).
#[derive(Serialize, Debug)]
pub struct Row {
    #[serde(flatten)]
    pub at: Loc,
    /// Lines after its signature with code on them, not those wholly
    /// inside a literal (at least 1).
    pub lines: usize,
    /// Parametrize: its literals are all the same as another test's of the
    /// group: the same code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identical: Option<bool>,
}

/// Literal positions of the bodies: where they differ, of how many.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct Literals {
    pub differ: usize,
    pub of: usize,
}

/// What tests of one file group by: scope, attributes, the body's names
/// hash (literals erased) and its columns (`columns`).
type Key<'a> = (&'a [String], &'a [String], u64, u64);

/// The literals as table columns: each one's kind, and the text of those
/// that aren't data (tuple indices, inline snapshots). Tests whose columns
/// differ aren't rows of one table: `'B'` vs `b'B'`, `p.0` vs `p.1`,
/// `@"3"` vs `@"9"`.
fn columns(literals: &[Literal]) -> u64 {
    let mut h = DefaultHasher::new();
    for l in literals {
        l.kind.hash(&mut h);
        (!l.data).then_some(l.text).hash(&mut h);
    }
    h.finish()
}

/// A test as compared: its fn and lines after its signature.
struct Test<'a> {
    func: &'a FnFact,
    lines: usize,
}

pub(super) fn parametrize(cx: &Cx, out: &mut Vec<Finding>) {
    for f in cx.focus() {
        // Same scope, attributes, body up to literals and columns;
        // signatures split below (not hashable, rarely differ).
        let mut groups: HashMap<Key, Vec<Test>> = HashMap::new();
        for func in &f.facts.fns {
            let (Some(t), Some(h)) = (&func.test_fn, func.body_hash) else {
                continue;
            };
            let lines = t.lines.max(1);
            groups
                .entry((&t.scope, &t.attrs, h.names, columns(&t.literals)))
                .or_default()
                .push(Test { func, lines });
        }
        for tests in groups.into_values().filter(|g| g.len() > 1) {
            for mut same in by_signature(tests) {
                same.sort_by_key(|t| t.func.line);
                if let Some(finding) = candidate(cx, f, &same) {
                    out.push(finding);
                }
            }
        }
    }
}

/// Tests split by equal signatures, groups of at least two.
fn by_signature(tests: Vec<Test>) -> Vec<Vec<Test>> {
    let mut out: Vec<Vec<Test>> = Vec::new();
    for t in tests {
        match out
            .iter_mut()
            .find(|g| g[0].func.signature == t.func.signature)
        {
            Some(g) => g.push(t),
            None => out.push(vec![t]),
        }
    }
    out.retain(|g| g.len() > 1);
    out
}

impl<'a> Test<'a> {
    fn literals(&self) -> &'a [Literal] {
        &self.func.test_fn.as_ref().expect("test fns").literals
    }
}

/// The finding for tests sharing scope, attributes, signature and body up
/// to literals and columns, if some literal differs, the names are distinct
/// and the group is large enough.
fn candidate(cx: &Cx, f: &FileAnalysis, tests: &[Test]) -> Option<Finding> {
    let big = |t: &Test| t.lines >= cx.opts.min_dup;
    let enough = tests.len() >= MIN_PARAMETRIZE_TESTS || tests.iter().all(big);
    let mut names: Vec<&str> = tests.iter().map(|t| t.func.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if !enough || names.len() < tests.len() {
        return None;
    }
    let first = tests[0].literals();
    // Equal names hashes mean equal literal counts, barring a collision.
    if tests.iter().any(|t| t.literals().len() != first.len()) {
        return None;
    }
    // Grouped by columns: kinds match, and only data can differ.
    let differ = (0..first.len())
        .filter(|&i| tests.iter().any(|t| t.literals()[i].text != first[i].text))
        .count();
    if differ == 0 {
        return None;
    }
    let same = |a: &Test, b: &Test| {
        let text = |t: &Test| t.literals().iter().map(|l| l.text).collect::<Vec<_>>();
        text(a) == text(b)
    };
    let rows: Vec<Row> = tests
        .iter()
        .enumerate()
        .map(|(i, t)| Row {
            at: Loc::of_fn(cx.scan, f, t.func),
            lines: t.lines,
            identical: Some(tests.iter().enumerate().any(|(j, o)| j != i && same(o, t))),
        })
        .collect();
    let literals = Literals {
        differ,
        of: first.len(),
    };
    let total = tests.iter().map(|t| t.lines).sum();
    Some(
        Finding {
            lines: total,
            detail: detail(&rows, literals),
            tests: Some(rows),
            literals: Some(literals),
            ..Finding::new(
                Kind::Parametrize,
                Category::Test,
                Loc::of_fn(cx.scan, f, tests[0].func),
            )
        }
        .rank(0, total),
    )
}

/// `4 tests differ only in 2 of 6 literals, 2 lines each, 2 identical: one
/// test over a table (or a helper taking them)`.
fn detail(rows: &[Row], literals: Literals) -> String {
    let (lo, hi) = rows.iter().fold((usize::MAX, 0), |(lo, hi), r| {
        (lo.min(r.lines), hi.max(r.lines))
    });
    let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    let mut facts = if lo == hi {
        format!("{} each", super::plural(n(hi), "line"))
    } else {
        format!("{lo}–{hi} lines each")
    };
    let identical = rows.iter().filter(|r| r.identical == Some(true)).count();
    if identical > 0 {
        facts.push_str(&format!(", {identical} identical"));
    }
    format!(
        "{} differ only in {} of {}, {facts}: one test over a table (or a helper taking them)",
        super::plural(n(rows.len()), "test"),
        literals.differ,
        super::plural(n(literals.of), "literal"),
    )
}

#[cfg(test)]
mod tests {
    use crate::findings::tests::*;

    fn found(src: &str) -> Vec<Finding> {
        let all = unused_in(
            &format!("fn main() {{}}\n#[cfg(test)]\nmod tests {{\n{src}}}\n"),
            &[],
        );
        all.all
            .into_iter()
            .filter(|f| f.kind == Kind::Parametrize)
            .collect()
    }

    /// A test `name` whose body is `body` with `{}` replaced by `lit`.
    fn test(attrs: &str, name: &str, body: &str, lit: &str) -> String {
        format!("{attrs}\nfn {name}() {{\n{}\n}}\n", body.replace("{}", lit))
    }

    const ONE: &str = "    assert_eq!(f({}), 2);";

    #[test]
    fn three_tests_differing_in_literals_are_a_table() {
        let src = [("a", "1"), ("b", "2"), ("c", "3")]
            .map(|(n, l)| test("#[test]", n, ONE, l))
            .concat();
        let fs = found(&src);
        assert_eq!(fs.len(), 1);
        let f = &fs[0];
        assert_eq!(f.at.name, "a");
        assert_eq!(f.category, Category::Test);
        let rows = f.tests.as_ref().unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.at.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        let l = f.literals.unwrap();
        assert_eq!((l.differ, l.of), (1, 2));
        // Lines after the signature: the statement and the closing brace.
        assert_eq!(f.lines, 6);
        assert_eq!(
            f.detail,
            "3 tests differ only in 1 of 2 literals, 2 lines each: one test over a table (or a helper taking them)"
        );
    }

    #[test]
    fn identical_copies_count_but_need_a_difference() {
        let same = [("a", "1"), ("b", "1"), ("c", "1")]
            .map(|(n, l)| test("#[test]", n, ONE, l))
            .concat();
        assert!(found(&same).is_empty(), "no literal differs");
        let src = [("a", "1"), ("b", "1"), ("c", "3")]
            .map(|(n, l)| test("#[test]", n, ONE, l))
            .concat();
        let fs = found(&src);
        let identical: Vec<bool> = fs[0]
            .tests
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| r.identical == Some(true))
            .collect();
        assert_eq!(identical, [true, true, false]);
        assert!(
            fs[0]
                .detail
                .contains("literals, 2 lines each, 2 identical:"),
            "{}",
            fs[0].detail
        );
    }

    #[test]
    fn a_pair_needs_min_dup_lines_each() {
        let short = [("a", "1"), ("b", "2")]
            .map(|(n, l)| test("#[test]", n, ONE, l))
            .concat();
        assert!(found(&short).is_empty());
        // min_dup is 4 in tests: 4 statements + the closing brace.
        let long = "    let x = f({});\n    g(x);\n    h(x);\n    assert!(x > 0);";
        let src = [("a", "1"), ("b", "2")]
            .map(|(n, l)| test("#[test]", n, long, l))
            .concat();
        let fs = found(&src);
        assert_eq!(fs.len(), 1);
        assert!(
            fs[0]
                .detail
                .starts_with("2 tests differ only in 1 of 2 literals, 5 lines each:"),
            "{}",
            fs[0].detail
        );
    }

    #[test]
    fn attributes_scope_and_signature_split_groups() {
        let three = |attrs: &str, prefix: &str| {
            [("a", "1"), ("b", "2"), ("c", "3")]
                .map(|(n, l)| test(attrs, &format!("{prefix}{n}"), ONE, l))
                .concat()
        };
        let src = format!(
            "{}{}{}mod inner {{\n{}}}\n",
            three("#[test]", "p_"),
            three("#[test]\n#[should_panic]", "s_"),
            three("#[test]", "a_").replace("fn a_", "async fn a_"),
            three("#[test]", "i_"),
        );
        let mut firsts: Vec<String> = found(&src).into_iter().map(|f| f.at.name).collect();
        firsts.sort();
        assert_eq!(firsts, ["a_a", "i_a", "p_a", "s_a"]);
        // Cfg-exclusive mods of one name are different scopes.
        let src = format!(
            "#[cfg(unix)]\nmod imp {{\n{}{}}}\n#[cfg(windows)]\nmod imp {{\n{}}}\n",
            test("#[test]", "a", ONE, "1"),
            test("#[test]", "b", ONE, "2"),
            test("#[test]", "c", ONE, "3"),
        );
        assert!(found(&src).is_empty());
        // Tests taking params (rstest cases) aren't tests as written.
        let p = "#[test]\nfn p(x: u8) {\n    assert_eq!(f(1), 2);\n}\n";
        let src = format!(
            "{p}{}{}",
            test("#[test]", "q", ONE, "2"),
            test("#[test]", "r", ONE, "3")
        );
        assert!(found(&src).is_empty());
    }

    /// None of these is a table: the literals that differ aren't data of
    /// one kind, or the names repeat (cfg variants).
    #[test]
    fn literals_that_arent_a_table_column() {
        let three = |body: &str, lits: [&str; 3]| {
            ["a", "b", "c"]
                .iter()
                .zip(lits)
                .map(|(n, l)| test("#[test]", n, body, l))
                .collect::<String>()
        };
        // Inline snapshots (insta): `@"…"` is the expected output.
        let snap = "    assert_snapshot!(f(1), @{});";
        assert!(found(&three(snap, ["\"1\"", "\"2\"", "\"3\""])).is_empty());
        // `@` alone (in an expression macro): the snapshot still isn't
        // data; the same snapshot for other inputs is a table.
        let same = "    assert!(f({}), @\"err\");";
        assert_eq!(found(&three(same, ["1", "2", "3"])).len(), 1);
        let snap = "    assert!(f(1), @{});";
        assert!(found(&three(snap, ["\"1\"", "\"2\"", "\"3\""])).is_empty());
        // insta's macro is opaque: its inputs aren't data either.
        let same = "    assert_snapshot!(f({}), @\"err\");";
        assert!(found(&three(same, ["1", "2", "3"])).is_empty());
        // Opaque macros' literals are their input, not data; the
        // well-known expression macros' are data.
        let input = "    assert_eq!(f(include_str!({})), 2);";
        assert!(found(&three(input, ["\"a\"", "\"b\"", "\"c\""])).is_empty());
        let nested = "    check!(assert_eq!(f({}), 2));";
        assert!(found(&three(nested, ["1", "2", "3"])).is_empty());
        let message = "    assert!(f(1), {});";
        assert_eq!(found(&three(message, ["\"a\"", "\"b\"", "\"c\""])).len(), 1);
        // `return !(x)` negates: no macro.
        let not = "    return !(f({}) > 2);";
        assert_eq!(found(&three(not, ["1", "2", "3"])).len(), 1);
        // Tuple indices are fields, not data.
        let field = "    let p = make();\n    assert_eq!(p.{}, 0);";
        assert!(found(&three(field, ["0", "1", "2"])).is_empty());
        // Kinds differ: a char and a byte, a string and a char.
        assert!(found(&three(ONE, ["'B'", "b'B'", "'C'"])).is_empty());
        assert!(found(&three(ONE, ["\"a\"", "'a'", "\"b\""])).is_empty());
        assert!(found(&three(ONE, ["1u8", "1i64", "2u8"])).is_empty());
        // Rows of other kinds split off: three chars stay a table.
        let four = format!(
            "{}{}",
            three(ONE, ["'a'", "'b'", "'c'"]),
            test("#[test]", "d", ONE, "b'd'")
        );
        let fs = found(&four);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].tests.as_ref().unwrap().len(), 3);
        // The same kind and suffix: a column.
        assert_eq!(found(&three(ONE, ["1u8", "2u8", "3u8"])).len(), 1);
        assert_eq!(found(&three(ONE, ["0x1f32", "0x10", "0b1"])).len(), 1);
        assert_eq!(
            found(&three(ONE, ["r\"a\\b\"", "\"b\"", "r#\"c\"#"])).len(),
            1
        );
        assert!(found(&three(ONE, ["b\"a\"", "\"b\"", "br\"c\""])).is_empty());
        // Only doc comments differ: nothing does.
        let doc = "    /// {}\n    assert_eq!(f(1), 2);";
        assert!(found(&three(doc, ["one", "two", "three"])).is_empty());
        // A name written in cfg-exclusive mods: at most one compiles.
        let cfg = ["unix", "windows", "wasi"]
            .iter()
            .enumerate()
            .map(|(i, os)| {
                format!(
                    "#[cfg({os})]\nmod imp {{\n{}}}\n",
                    test("#[test]", "sep", ONE, &i.to_string())
                )
            })
            .collect::<String>();
        assert!(found(&cfg).is_empty());
    }

    #[test]
    fn lines_have_code_on_them() {
        // Each test: a 6-line string, 7 code lines but 3 with a token
        // (min_dup is 4 in tests: in only by its string's text).
        let long = "    let s = \"{}\n1\n2\n3\n4\n5\";";
        let src = format!(
            "{}{}",
            test("#[test]", "a", long, "x"),
            test("#[test]", "b", long, "y")
        );
        assert!(found(&src).is_empty());
        // Nested items count in lines as they do in literals.
        let nested =
            "    fn h() -> u32 {\n        let a = 1;\n        a + {}\n    }\n    assert!(h() > 0);";
        let src = [("a", "1"), ("b", "2")]
            .map(|(n, l)| test("#[test]", n, nested, l))
            .concat();
        let fs = found(&src);
        assert_eq!(fs.len(), 1);
        assert!(
            fs[0].detail.contains("1 of 3 literals, 6 lines each"),
            "{}",
            fs[0].detail
        );
    }

    #[test]
    fn booleans_are_names() {
        let src = [("a", "true"), ("b", "false"), ("c", "true")]
            .map(|(n, l)| test("#[test]", n, ONE, l))
            .concat();
        assert!(found(&src).is_empty());
    }
}
