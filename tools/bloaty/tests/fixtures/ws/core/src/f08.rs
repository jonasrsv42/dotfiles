//! Filler 8: ordinary code to rank the others against.

pub fn f8(xs: &[u32]) -> u32 {
    let mut acc = 8u32;
    for x in xs {
        acc = acc.wrapping_mul(47).wrapping_add(*x);
    }
    let half = acc / 2;
    let rest = acc - half;
    let spread = half.abs_diff(rest);
    let _p0 = 8 * 0 + 47;
    let _p1 = 8 * 1 + 47;
    let _p2 = 8 * 2 + 47;
    acc ^ spread
}
