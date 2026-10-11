//! Filler 6: ordinary code to rank the others against.

pub fn f6(xs: &[u32]) -> u32 {
    let mut best = 0;
    let mut seen = 0;
    let mut i = 0;
    while i < xs.len() {
        best = best.max(xs[i] + 6);
        seen += 1;
        i += 1;
    }
    let _p0 = 6 * 0 + 43;
    best * 43 + seen
}
