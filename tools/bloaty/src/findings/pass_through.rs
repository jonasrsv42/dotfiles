//! `pass_through` (opt-in): fns whose body only forwards their inputs to
//! another workspace fn.

use super::{Category, Cx, Finding, Kind, Reason, plural};
use crate::names::Loc;
use crate::visit::body::{Callee, Forward};

/// Whether the call may be to the workspace's own fn: `helper`,
/// `crate::m::f`, `Self::f`, `Type::f` with a workspace type, `self.f`;
/// not `Arc::ptr_eq`, `u32::from` or `self.items.len`.
fn workspace_callee(cx: &Cx, target: &Forward) -> bool {
    match &target.callee {
        Callee::Method { on_self } => *on_self,
        Callee::Path(segments) => {
            let head = segments.first().map_or("", String::as_str);
            let std = matches!(head, "std" | "core" | "alloc");
            let local = matches!(head, "crate" | "self" | "super" | "Self")
                || (!std && cx.workspace_names.contains(head));
            segments.len() == 1 || local
        }
    }
}

/// Only forwarding to the workspace's own fns counts: a free fn, an
/// associated fn, or another method on `self`. Delegating to a field
/// (`self.items.len()`), to a parameter, to a constructor (`new`,
/// `from_*`) or to code outside the workspace is a wrapper's job, not
/// indirection to remove.
pub(super) fn pass_through(cx: &mut Cx, out: &mut Vec<Finding>) {
    for f in cx.focus().filter(|f| !f.whole_test) {
        for func in f.facts.fns.iter().filter(|func| !func.in_test) {
            let Some(target) = &func.forwards_to else {
                continue;
            };
            let callee = target.name.as_str();
            let constructor = callee == "new" || callee.starts_with("from_");
            if !workspace_callee(cx, target)
                || constructor
                || callee == func.ident
                || !cx.fn_defs.contains_key(callee)
            {
                continue;
            }
            // Which of several same-named fns it calls takes name resolution.
            if cx.fn_defs.get(callee).copied().unwrap_or(0) > 1 {
                cx.exclude(Kind::PassThrough, Reason::SameName);
                continue;
            }
            if cx.skip_pub(Kind::PassThrough, f, func.vis) {
                continue;
            }
            // Counts mean nothing for a name many fns share (`push`, `get`).
            let defs = cx.fn_defs.get(func.ident.as_str()).copied().unwrap_or(0);
            let (r, t) = cx.refs(&func.ident);
            let refs = (defs <= 1).then_some((r.saturating_sub(func.self_refs), t));
            let refs_text = match refs {
                Some((r, t)) => format!("{} (+{t} in tests)", plural(r, "ref")),
                None => format!("refs unknown ({defs} fns named {})", func.ident),
            };
            let lines = f.own_lines(func);
            let callers = refs.map_or(0xFFFF, |(r, t)| (r + t).min(0xFFFF));
            out.push(
                Finding {
                    vis: Some(func.vis),
                    lines,
                    target: Some(target.text.clone()),
                    refs: refs.map(|(r, _)| r),
                    test_refs: refs.map(|(_, t)| t),
                    detail: format!("forwards to {}; {refs_text}", target.text),
                    ..Finding::new(
                        Kind::PassThrough,
                        Category::Impl,
                        Loc::of_fn(cx.scan, f, func),
                    )
                }
                // Inline the least-called first.
                .rank(callers, lines),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::findings::tests::*;

    #[test]
    fn pass_through_only_flags_indirection_to_workspace_fns() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "fn main() { let s = S { v: vec![] }; s.len(); s.go(1); s.fwd(1); verify(1); s.make(1); }\nstruct S { v: Vec<u8> }\nimpl S {\n    fn len(&self) -> usize { self.v.len() }\n    fn go(&self, x: u8) -> u8 { self.real_go(x) }\n    fn real_go(&self, x: u8) -> u8 { x + 1 }\n    fn fwd(&self, x: u8) -> u8 { helper(x) }\n    fn make(&self, x: u8) -> S { S::new(x) }\n    fn new(x: u8) -> S { S { v: vec![x] } }\n}\nfn helper(x: u8) -> u8 { x }\nfn verify(t: u8) -> bool { checks::verify(t) }\nmod checks {\n    pub fn verify(t: u8) -> bool { t > 0 }\n}\n",
            ),
        ]);
        let mut pass = names(&run(&fx, false), Kind::PassThrough);
        pass.sort();
        assert_eq!(pass, vec!["S::fwd", "S::go"]);
    }

    #[test]
    fn pass_through_to_a_shared_name_makes_no_claim() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "fn main() { go(1); solo(1); }\nfn go(x: u8) -> u8 { helper(x) }\nfn solo(x: u8) -> u8 { only(x) }\nfn helper(x: u8) -> u8 { x }\nfn only(x: u8) -> u8 { x }\nmod m {\n    pub fn helper(x: u8) -> u8 { x }\n}\n",
            ),
        ]);
        let all = run(&fx, false);
        assert_eq!(names(&all, Kind::PassThrough), vec!["solo"]);
        let why = &all.unclaimed[&Kind::PassThrough];
        assert_eq!(why.get(&Reason::SameName), Some(&1));
    }

    #[test]
    fn pass_through_ignores_std_targets_and_is_opt_in() {
        let fx = Fixture::new(&[
            ("Cargo.toml", "[package]\nname = \"m\"\n"),
            (
                "src/main.rs",
                "use std::sync::Arc;\nfn main() { same(&Arc::new(1), &Arc::new(1)); widen(1); go(1); }\nfn same(a: &Arc<u8>, b: &Arc<u8>) -> bool { Arc::ptr_eq(a, b) }\nfn widen(x: u8) -> u32 { u32::from(x) }\nfn go(x: u8) -> u8 { real(x) }\nfn real(x: u8) -> u8 { x }\nfn ptr_eq() {}\nfn from() {}\n",
            ),
        ]);
        let all = run(&fx, false);
        assert_eq!(names(&all, Kind::PassThrough), vec!["go"]);
        let scan = fx.scan();
        let default = report(&scan, &all, &opts(false), &[], None, 0);
        assert!(default.findings.iter().all(|f| f.kind != Kind::PassThrough));
    }
}
