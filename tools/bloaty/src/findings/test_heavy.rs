//! `test_heavy`: files with far more test lines than impl lines.

use super::{Category, Cx, Finding, Kind};
use crate::names::Loc;

pub(super) fn test_heavy(cx: &Cx, out: &mut Vec<Finding>) {
    for f in cx.focus().filter(|f| !f.whole_test) {
        let c = f.counts();
        if c.implementation > 0 && c.test >= 200 && c.test > 3 * c.implementation {
            let name = f
                .path
                .file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let at = Loc::new(cx.scan, f, &name, (1, 1, f.lines.len()));
            out.push(
                Finding {
                    lines: c.test,
                    detail: format!("{} test lines for {} impl lines", c.test, c.implementation),
                    ..Finding::new(Kind::TestHeavy, Category::Test, at)
                }
                .rank(0, c.test),
            );
        }
    }
}
