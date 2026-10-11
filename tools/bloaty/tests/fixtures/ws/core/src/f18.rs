//! Filler 18: ordinary code to rank the others against.

pub fn f18(xs: &[u32]) -> u32 {
    let mut best = 0;
    let mut seen = 0;
    let mut i = 0;
    while i < xs.len() {
        best = best.max(xs[i] + 18);
        seen += 1;
        i += 1;
    }
    let _p0 = 18 * 0 + 67;
    let _p1 = 18 * 1 + 67;
    let _p2 = 18 * 2 + 67;
    best * 67 + seen
}
