//! `long_fn`: fns over `--long-fn` own code lines, unless the smell score
//! excuses them under it (a table's literal, a dispatch's small arms, an
//! allowed `clippy::too_many_lines`).

use super::{Cx, Finding, Kind, Reason, category};
use crate::names::Loc;
use crate::rollup::lines_excuse;
use std::collections::HashSet;

/// `duplicated`: (file, line) of fns already reported as duplicates.
pub(super) fn long_fns(cx: &mut Cx, duplicated: &HashSet<(String, usize)>, out: &mut Vec<Finding>) {
    for f in cx.focus() {
        for func in &f.facts.fns {
            let lines = f.own_lines(func);
            if lines > cx.opts.long_fn {
                // Test or not is by path only in a file a macro may declare.
                if f.macro_declared {
                    cx.exclude(Kind::LongFn, Reason::NamedInMacro);
                    continue;
                }
                if let Some(x) = lines_excuse(func, lines, cx.opts.long_fn) {
                    cx.excuse(Kind::LongFn, x.kind());
                    continue;
                }
                let category = category(func.in_test || f.whole_test);
                out.push(
                    Finding {
                        lines,
                        detail: if duplicated.contains(&(f.rel.clone(), func.line)) {
                            format!("{lines} own code lines; also a duplicate")
                        } else {
                            format!("{lines} own code lines")
                        },
                        ..Finding::new(Kind::LongFn, category, Loc::of_fn(cx.scan, f, func))
                    }
                    .rank(0, lines),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::findings::tests::*;

    #[test]
    fn what_the_smell_score_excuses_isnt_long() {
        let arm = |i: u8| {
            format!("        {i} => {{\n            a();\n            b({i})\n        }}\n")
        };
        let src = format!(
            "pub fn plain() {{\n    a();\n    b();\n    c();\n    d();\n}}\n\
             #[allow(clippy::too_many_lines)]\npub fn allowed() {{\n    a();\n    b();\n    c();\n    d();\n}}\n\
             pub fn table() -> Vec<u32> {{\n    vec![\n        1,\n        2,\n        3,\n        4,\n    ]\n}}\n\
             pub fn dispatch(x: u8) -> u8 {{\n    match x {{\n{}{}{}{}    }}\n}}\n",
            arm(0),
            arm(1),
            arm(2),
            arm(3)
        );
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        // A dispatch's remainder (signature, `match`, braces) is 4 lines.
        let all = findings(
            &fx.scan(),
            &Options {
                long_fn: 4,
                ..opts(true)
            },
        );
        assert_eq!(names(&all, Kind::LongFn), vec!["plain"]);
        let excused: Vec<(&str, usize)> = all.excused[&Kind::LongFn]
            .iter()
            .map(|(k, n)| (*k, *n))
            .collect();
        assert_eq!(excused, [("allowed", 1), ("dispatch", 1), ("table", 1)]);
    }

    #[test]
    fn allowed_test_fns_arent_long_either() {
        let body: String = (0..6)
            .map(|i| format!("        let a{i} = {i};\n"))
            .collect();
        let src = format!(
            "pub fn f() {{}}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    #[allow(clippy::too_many_lines)]\n    fn long_test() {{\n{body}    }}\n    #[test]\n    fn plain_test() {{\n{body}    }}\n}}\n"
        );
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            ("src/lib.rs", &src),
        ]);
        let all = run(&fx, true);
        assert_eq!(names(&all, Kind::LongFn), vec!["plain_test"]);
        assert_eq!(all.excused[&Kind::LongFn][&"allowed"], 1);
    }
}
