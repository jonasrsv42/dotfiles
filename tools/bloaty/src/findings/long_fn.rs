//! `long_fn`: fns over `--long-fn` own code lines.

use super::{Cx, Finding, Kind, category};
use crate::names::Loc;
use std::collections::HashSet;

/// `duplicated`: (file, line) of fns already reported as duplicates.
pub(super) fn long_fns(cx: &Cx, duplicated: &HashSet<(String, usize)>, out: &mut Vec<Finding>) {
    for f in cx.focus() {
        for func in &f.facts.fns {
            let lines = f.own_lines(func);
            if lines > cx.opts.long_fn {
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
