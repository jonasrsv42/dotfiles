//! `fixture`: tests of one scope that open with the same statements. The
//! claim is a fact: the same file, inline `mod` (with its cfgs), attributes
//! and signature (so `.await` and `?` in the opening mean the same in a
//! setup fn: async with the tests, failing with their error type),
//! distinct names, and the same tokens statement by statement; the advice:
//! one setup fn they call, returning what the opening binds (a guard like
//! `let _g = lock()` stays alive only that way). The opening stops before
//! the first statement a fn couldn't take (`visit::opening`: an item, a
//! `let` without a value, a `return`, a macro bloaty doesn't read, `pin!`,
//! a `#[cfg]`, a `let` that may borrow an earlier binding on its own
//! path); a test declaring items at its top level has none. Where a later
//! binding borrows an earlier one through a call (only types can tell),
//! the setup fn stops before it or returns an owner: advice, not claimed.
//!
//! Openings are compared at every length (a prefix trie), so a test sharing
//! only the first statements doesn't hide a longer opening other tests
//! share. Each test is in at most one finding: the cluster replacing the
//! most statements (members × statements) first, then more statements,
//! then the earliest; its tests leave every other cluster, which is kept
//! only while it still qualifies.

use super::parametrize::Row;
use super::{Category, Cx, Finding, Kind};
use crate::names::Loc;
use crate::scan::FileAnalysis;
use crate::shape::MIN_PRELUDE_TOKENS;
use crate::visit::FnFact;
use crate::visit::opening::{MIN_FIXTURE_STATEMENTS, TestStmt};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// At least this many tests (opening with `MIN_FIXTURE_STATEMENTS` equal
/// statements).
pub(crate) const MIN_FIXTURE_TESTS: usize = 3;

/// The opening the tests share.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct Prefix {
    pub statements: usize,
    /// The first test's lines in it on which a token starts or a delimiter
    /// closes.
    pub lines: usize,
}

/// Tests opening with the same `statements`: indices into a population.
#[derive(Clone, Debug, PartialEq)]
struct Cluster {
    statements: usize,
    members: Vec<usize>,
}

pub(super) fn fixture(cx: &Cx, out: &mut Vec<Finding>) {
    // Each parametrize candidate's tests: a cluster wholly inside one is
    // its table already.
    let mut tables: HashMap<(String, usize), usize> = HashMap::new();
    for (i, f) in out.iter().enumerate() {
        if f.kind == Kind::Parametrize {
            for r in f.tests.iter().flatten() {
                tables.insert((r.at.file.clone(), r.at.line), i);
            }
        }
    }
    let mut found = Vec::new();
    for f in cx.focus() {
        let mut groups: HashMap<(&[String], &[String]), Vec<&FnFact>> = HashMap::new();
        for func in &f.facts.fns {
            let Some(t) = &func.test_fn else { continue };
            // `test_opening` leaves one shorter than a setup fn empty.
            if !t.opening.is_empty() {
                groups.entry((&t.scope, &t.attrs)).or_default().push(func);
            }
        }
        for tests in groups.into_values() {
            for mut same in by_signature(tests) {
                same.sort_by_key(|func| func.line);
                let same = distinct(same);
                let table = |func: &FnFact| tables.get(&(f.rel.clone(), func.line)).copied();
                for c in report(&same, table) {
                    found.push(finding(cx, f, &c, &same));
                }
            }
        }
    }
    out.extend(found);
}

/// Tests split by equal signatures, groups of at least `MIN_FIXTURE_TESTS`.
fn by_signature(tests: Vec<&FnFact>) -> Vec<Vec<&FnFact>> {
    let mut out: Vec<Vec<&FnFact>> = Vec::new();
    for t in tests {
        match out.iter_mut().find(|g| g[0].signature == t.signature) {
            Some(g) => g.push(t),
            None => out.push(vec![t]),
        }
    }
    out.retain(|g| g.len() >= MIN_FIXTURE_TESTS);
    out
}

/// The tests whose name no other test of the group has: a name written
/// twice is cfg variants (at most one compiles).
fn distinct(tests: Vec<&FnFact>) -> Vec<&FnFact> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for t in &tests {
        *seen.entry(t.name.as_str()).or_default() += 1;
    }
    tests
        .into_iter()
        .filter(|t| seen[t.name.as_str()] == 1)
        .collect()
}

/// A test's opening: something follows it (`test_opening`).
fn opening(func: &FnFact) -> &[TestStmt] {
    &func.test_fn.as_ref().expect("test fns").opening
}

/// Every opening at least `MIN_FIXTURE_STATEMENTS` long that at least
/// `MIN_FIXTURE_TESTS` tests share and that amounts to something
/// (`MIN_PRELUDE_TOKENS`, a call or control flow): the nodes of a prefix
/// trie, members ascending.
fn clusters(seqs: &[&[TestStmt]]) -> Vec<Cluster> {
    let mut chain: Vec<DefaultHasher> = seqs.iter().map(|_| DefaultHasher::new()).collect();
    let longest = seqs.iter().map(|s| s.len()).max().unwrap_or(0);
    let mut out = Vec::new();
    for k in 1..=longest {
        let mut nodes: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, s) in seqs.iter().enumerate() {
            if s.len() >= k {
                s[k - 1].hash.hash(&mut chain[i]);
                nodes.entry(chain[i].finish()).or_default().push(i);
            }
        }
        if k < MIN_FIXTURE_STATEMENTS {
            continue;
        }
        for mut members in nodes.into_values() {
            // Equal statement by statement, not just by chained hash.
            let first = &seqs[members[0]][..k];
            let same = |i: &usize| {
                seqs[*i][..k]
                    .iter()
                    .map(|s| s.hash)
                    .eq(first.iter().map(|s| s.hash))
            };
            members.retain(same);
            let tokens: usize = first.iter().map(|s| s.tokens).sum();
            let substance = tokens >= MIN_PRELUDE_TOKENS && first.iter().any(|s| s.active);
            if members.len() >= MIN_FIXTURE_TESTS && substance {
                members.sort_unstable();
                out.push(Cluster {
                    statements: k,
                    members,
                });
            }
        }
    }
    out
}

/// The clusters to report, no test in two: repeatedly the one replacing
/// the most statements (members × statements), then the longer, then the
/// earliest; its members leave the others, which stay while they still
/// have enough members and aren't wholly inside one parametrize candidate
/// (`table`: the candidate a test is a row of).
fn report(tests: &[&FnFact], table: impl Fn(&FnFact) -> Option<usize>) -> Vec<Cluster> {
    let seqs: Vec<&[TestStmt]> = tests.iter().map(|t| opening(t)).collect();
    let mut left = clusters(&seqs);
    let mut out = Vec::new();
    loop {
        left.retain(|c| {
            let one_table = || {
                table(tests[c.members[0]])
                    .is_some_and(|t| c.members.iter().all(|&m| table(tests[m]) == Some(t)))
            };
            c.members.len() >= MIN_FIXTURE_TESTS && !one_table()
        });
        let Some(best) = left
            .iter()
            .max_by_key(|c| {
                (
                    c.members.len() * c.statements,
                    c.statements,
                    Reverse(c.members[0]),
                )
            })
            .cloned()
        else {
            break;
        };
        let taken: HashSet<usize> = best.members.iter().copied().collect();
        for c in &mut left {
            c.members.retain(|m| !taken.contains(m));
        }
        out.push(best);
    }
    out.sort_by_key(|c| c.members[0]);
    out
}

/// A test's lines in its first `k` statements (adjacent statements may
/// share a line).
fn prefix_lines(func: &FnFact, k: usize) -> usize {
    let s = &opening(func)[..k];
    let shared = s
        .windows(2)
        .filter(|w| w[0].last_line == w[1].first_line)
        .count();
    s.iter().map(|s| s.lines).sum::<usize>() - shared
}

fn finding(cx: &Cx, f: &FileAnalysis, c: &Cluster, tests: &[&FnFact]) -> Finding {
    let members: Vec<&FnFact> = c.members.iter().map(|&m| tests[m]).collect();
    let lines: Vec<usize> = members
        .iter()
        .map(|t| prefix_lines(t, c.statements))
        .collect();
    let rows: Vec<Row> = members
        .iter()
        .map(|t| Row {
            at: Loc::of_fn(cx.scan, f, t),
            lines: t.test_fn.as_ref().map_or(1, |t| t.lines.max(1)),
            identical: None,
        })
        .collect();
    let total = lines.iter().sum();
    let detail = detail(rows.len(), c.statements, &lines);
    let prefix = Prefix {
        statements: c.statements,
        lines: lines[0],
    };
    Finding {
        lines: total,
        detail,
        tests: Some(rows),
        prefix: Some(prefix),
        ..Finding::new(
            Kind::Fixture,
            Category::Test,
            Loc::of_fn(cx.scan, f, members[0]),
        )
    }
    .rank(0, total)
}

/// `3 tests open with the same 6 statements (8 lines): one setup fn they
/// call`; lines as a range when the tests lay the statements out
/// differently.
fn detail(tests: usize, statements: usize, lines: &[usize]) -> String {
    let lo = lines.iter().copied().min().unwrap_or(0);
    let hi = lines.iter().copied().max().unwrap_or(0);
    let lines = if lo == hi {
        super::plural(n(hi), "line")
    } else {
        format!("{lo}–{hi} lines")
    };
    format!(
        "{tests} tests open with the same {statements} statements ({lines}): one setup fn they call"
    )
}

fn n(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
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
            .filter(|f| f.kind == Kind::Fixture)
            .collect()
    }

    /// `#[test] fn name() { opening; rest }`.
    fn test(name: &str, opening: &str, rest: &str) -> String {
        format!("#[test]\nfn {name}() {{\n{opening}\n    {rest}\n}}\n")
    }

    /// Four statements a fixture could take.
    const SETUP: &str = "    let db = open(\"mem\");\n    let user = add(\"u\");\n    grant(&db, user, 5);\n    let wallet = wallet_of(user);";

    fn members(f: &Finding) -> Vec<&str> {
        f.tests
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| r.at.name.as_str())
            .collect()
    }

    #[test]
    fn three_tests_opening_alike_share_a_fixture() {
        let src = [
            ("a", "check(wallet, 1);"),
            ("b", "check(wallet, 2);"),
            ("c", "drop(db);"),
        ]
        .map(|(n, rest)| test(n, SETUP, rest))
        .concat();
        let fs = found(&src);
        assert_eq!(fs.len(), 1);
        let f = &fs[0];
        assert_eq!(f.at.name, "a");
        assert_eq!(f.category, Category::Test);
        assert_eq!(members(f), ["a", "b", "c"]);
        let p = f.prefix.as_ref().unwrap();
        assert_eq!((p.statements, p.lines), (4, 4));
        assert_eq!(f.lines, 12);
        assert_eq!(
            f.detail,
            "3 tests open with the same 4 statements (4 lines): one setup fn they call"
        );
        // A pair isn't enough; neither are three statements.
        let pair = [("a", "check(wallet, 1);"), ("b", "drop(db);")]
            .map(|(n, rest)| test(n, SETUP, rest))
            .concat();
        assert!(found(&pair).is_empty());
        let three = SETUP.rsplit_once('\n').unwrap().0;
        let src = ["a", "b", "c"]
            .map(|n| test(n, three, "check(user, 1);"))
            .concat();
        assert!(found(&src).is_empty());
    }

    #[test]
    fn something_must_follow_the_opening() {
        // The same four statements and nothing else: the whole body.
        let src = ["a", "b", "c"]
            .map(|n| format!("#[test]\nfn {n}() {{\n{SETUP}\n}}\n"))
            .concat();
        assert!(found(&src).is_empty());
    }

    #[test]
    fn a_stray_test_doesnt_hide_a_longer_opening() {
        let first = SETUP.split_once('\n').unwrap().0;
        let src = format!(
            "{}{}",
            ["a", "b", "c"]
                .map(|n| test(n, SETUP, "check(wallet, 1);"))
                .concat(),
            test("stray", first, "check(db, 2);")
        );
        let fs = found(&src);
        assert_eq!(fs.len(), 1);
        assert_eq!(members(&fs[0]), ["a", "b", "c"]);
        assert_eq!(fs[0].prefix.as_ref().unwrap().statements, 4);
    }

    /// Six tests open with SETUP, three of them with five statements more:
    /// 3 × 9 statements beat 6 × 4, and the other three still share 4.
    #[test]
    fn each_test_is_in_one_finding() {
        let longer = format!("{SETUP}{}", "\n    pay(&db, wallet, 1);".repeat(5));
        let src = format!(
            "{}{}",
            ["a", "b", "c"]
                .map(|n| test(n, SETUP, "check(wallet, 1);"))
                .concat(),
            ["d", "e", "f"]
                .map(|n| test(n, &longer, "check(wallet, 3);"))
                .concat(),
        );
        let fs = found(&src);
        let got: Vec<(Vec<&str>, usize)> = fs
            .iter()
            .map(|f| (members(f), f.prefix.as_ref().unwrap().statements))
            .collect();
        assert_eq!(got, [(vec!["d", "e", "f"], 9), (vec!["a", "b", "c"], 4)]);
        // With two longer tests, the six share four statements.
        let src = format!(
            "{}{}",
            ["a", "b", "c", "d"]
                .map(|n| test(n, SETUP, "check(wallet, 1);"))
                .concat(),
            ["e", "f"]
                .map(|n| test(n, &longer, "check(wallet, 3);"))
                .concat(),
        );
        let fs = found(&src);
        assert_eq!(fs.len(), 1);
        assert_eq!(members(&fs[0]), ["a", "b", "c", "d", "e", "f"]);
    }

    /// The opening stops before a statement that wouldn't do the same moved
    /// into a fn returning what it binds.
    #[test]
    fn openings_stop_where_a_fixture_couldnt_take_them() {
        let three = |extra: &str| {
            ["a", "b", "c"]
                .map(|n| test(n, &format!("{SETUP}\n{extra}"), "check(wallet, 1);"))
                .concat()
        };
        let statements = |src: &str| {
            found(src)
                .first()
                .map(|f| f.prefix.as_ref().unwrap().statements)
        };
        assert_eq!(statements(&three("    pay(&db, wallet, 1);")), Some(5));
        for cut in [
            "    if wallet == 0 { return; }",
            "    let _ = custom!(db);",
            "    tokio::pin!(wallet);",
            "    let fut = pin!(load(&db));",
            "    let late;",
            "    let name = &user.name;",
            "    let rows = db.rows();",
            "    let row = db[0];",
            "    let count = || db.len();",
            "    let n = async { db.len() };",
            "    let cfg = &Config::new(1);",
            "    let db2 = &mut open(1);",
            "    let r = { &db };",
            "    let r = if go() { &db } else { &db };",
            "    let r = match go() { _ => db.rows() };",
            "    let r = loop { break &db; };",
            "    #[cfg(unix)]\n    let b = make(2);",
            "    r = &db;",
            // Inside an expression macro, as tokens.
            "    assert!(check(custom!(db)));",
            "    assert!(f(|| { return 1; }));",
        ] {
            assert_eq!(statements(&three(cut)), Some(4), "{cut}");
        }
        // A method call on a binding only on a call's arguments, a clone, a
        // `move` closure: fine.
        for kept in [
            "    let rows = count(db.rows());",
            "    let copy = db.clone();",
            "    let count = move || 1;",
            "    let n = async move { return 1; };",
            "    assert_eq!(count(&db), 1, \"{}\", format!(\"x\"));",
            "    if !(db.ok()) { panic!(\"down\"); }",
        ] {
            assert_eq!(statements(&three(kept)), Some(5), "{kept}");
        }
        // An item in the body: the opening could name it.
        let src = ["a", "b", "c"]
            .map(|n| test(n, SETUP, "fn helper() {}\n    check(wallet, 1);"))
            .concat();
        assert!(found(&src).is_empty());
    }

    #[test]
    fn openings_need_substance() {
        // Fresh values only: nothing done yet.
        let fresh = "    let mut a = Vec::new();\n    let mut b = String::new();\n    let mut c = Vec::new();\n    let mut d = Vec::new();";
        let src = ["a", "b", "c"]
            .map(|n| test(n, fresh, "go(a, b, c, d);"))
            .concat();
        assert!(found(&src).is_empty());
        let acting = format!("{fresh}\n    a.push(1);");
        let src = ["a", "b", "c"]
            .map(|n| test(n, &acting, "go(a, b, c, d);"))
            .concat();
        assert_eq!(found(&src).len(), 1);
    }

    #[test]
    fn attributes_signature_names_and_tables_split() {
        let src = format!(
            "{}#[test]\n#[ignore]\n{}",
            ["a", "b"]
                .map(|n| test(n, SETUP, "check(wallet, 1);"))
                .concat(),
            test("c", SETUP, "check(wallet, 1);").trim_start_matches("#[test]\n"),
        );
        assert!(found(&src).is_empty(), "attributes differ");
        let src = format!(
            "{}{}",
            ["a", "b"]
                .map(|n| test(n, SETUP, "check(wallet, 1);"))
                .concat(),
            test("c", SETUP, "check(wallet, 1);").replace("fn c()", "fn c() -> Result<(), E>"),
        );
        assert!(found(&src).is_empty(), "signatures differ");
        // Cfg-exclusive mods of one name are different scopes: at most one
        // compiles.
        let ab = ["a", "b"].map(|n| test(n, SETUP, "drop(db);")).concat();
        let c = test("c", SETUP, "drop(db);");
        let src = format!("#[cfg(unix)]\nmod imp {{\n{ab}}}\n#[cfg(windows)]\nmod imp {{\n{c}}}\n");
        assert!(found(&src).is_empty(), "cfg-exclusive mods");
        let src = format!("#[cfg(unix)]\nmod imp {{\n{ab}{c}}}\n");
        assert_eq!(found(&src).len(), 1);
        // Rows of one table-driven-test candidate: that finding says it.
        let src = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, n)| test(n, SETUP, &format!("check(wallet, {i});")))
            .collect::<String>();
        assert!(found(&src).is_empty(), "a parametrize candidate");
    }

    #[test]
    fn lines_count_each_tests_layout() {
        let wide = SETUP.replace(
            "grant(&db, user, 5);",
            "grant(\n        &db,\n        user,\n        5\n    );",
        );
        let src = format!(
            "{}{}",
            ["a", "b"]
                .map(|n| test(n, SETUP, "check(wallet, 1);"))
                .concat(),
            test("c", &wide, "drop(db);"),
        );
        let fs = found(&src);
        assert_eq!(fs[0].prefix.as_ref().unwrap().lines, 4);
        assert_eq!(fs[0].lines, 4 + 4 + 8);
        assert!(fs[0].detail.contains("(4–8 lines)"), "{}", fs[0].detail);
    }
}
