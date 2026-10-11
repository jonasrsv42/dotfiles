//! Filler 20: ordinary code to rank the others against.

pub fn f20(xs: &[u32]) -> u32 {
    let mut acc = 20u32;
    for x in xs {
        acc = acc.wrapping_mul(71).wrapping_add(*x);
    }
    let half = acc / 2;
    let rest = acc - half;
    let spread = half.abs_diff(rest);
    acc ^ spread
}
