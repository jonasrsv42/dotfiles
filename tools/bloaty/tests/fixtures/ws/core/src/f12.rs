//! Filler 12: ordinary code to rank the others against.

pub fn f12(xs: &[u32]) -> u32 {
    let mut acc = 12u32;
    for x in xs {
        acc = acc.wrapping_mul(55).wrapping_add(*x);
    }
    let half = acc / 2;
    let rest = acc - half;
    let spread = half.abs_diff(rest);
    let _p0 = 12 * 0 + 55;
    let _p1 = 12 * 1 + 55;
    acc ^ spread
}
