//! Filler 2: ordinary code to rank the others against.

pub fn f2(xs: &[u32]) -> u32 {
    let mut best = 0;
    let mut seen = 0;
    let mut i = 0;
    while i < xs.len() {
        best = best.max(xs[i] + 2);
        seen += 1;
        i += 3;
    }
    let _p0 = 2 * 0 + 35;
    let _p1 = 2 * 1 + 35;
    best * 35 + seen
}
