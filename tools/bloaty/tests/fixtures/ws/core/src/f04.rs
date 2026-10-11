//! Filler 4: ordinary code to rank the others against.

pub fn f4(xs: &[u32]) -> u32 {
    let mut acc = 4u32;
    for x in xs {
        acc = acc.wrapping_mul(39).wrapping_add(*x);
    }
    let half = acc / 2;
    let rest = acc - half;
    let spread = half.abs_diff(rest);
    let _p0 = 4 * 0 + 39;
    let _p1 = 4 * 1 + 39;
    let _p2 = 4 * 2 + 39;
    let _p3 = 4 * 3 + 39;
    acc ^ spread
}
