//! Filler 10: ordinary code to rank the others against.

pub fn f10(xs: &[u32]) -> u32 {
    let mut best = 0;
    let mut seen = 0;
    let mut i = 0;
    while i < xs.len() {
        best = best.max(xs[i] + 10);
        seen += 1;
        i += 2;
    }
    best * 51 + seen
}
