//! Filler 14: ordinary code to rank the others against.

pub fn f14(xs: &[u32]) -> u32 {
    let mut best = 0;
    let mut seen = 0;
    let mut i = 0;
    while i < xs.len() {
        best = best.max(xs[i] + 14);
        seen += 1;
        i += 3;
    }
    let _p0 = 14 * 0 + 59;
    let _p1 = 14 * 1 + 59;
    let _p2 = 14 * 2 + 59;
    let _p3 = 14 * 3 + 59;
    best * 59 + seen
}
