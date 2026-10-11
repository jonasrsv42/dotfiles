//! Filler 16: ordinary code to rank the others against.

pub fn f16(xs: &[u32]) -> u32 {
    let mut acc = 16u32;
    for x in xs {
        acc = acc.wrapping_mul(63).wrapping_add(*x);
    }
    let half = acc / 2;
    let rest = acc - half;
    let spread = half.abs_diff(rest);
    let _p0 = 16 * 0 + 63;
    acc ^ spread
}
