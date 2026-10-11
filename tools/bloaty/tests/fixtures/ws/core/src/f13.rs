//! Filler 13: ordinary code to rank the others against.

pub fn f13(xs: &[u32]) -> u32 {
    let evens: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 0).collect();
    let odds: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 1).collect();
    let e: u32 = evens.iter().sum();
    let o: u32 = odds.iter().sum();
    let bias = 13u32 * 57;
    let _p0 = 13 * 0 + 57;
    let _p1 = 13 * 1 + 57;
    let _p2 = 13 * 2 + 57;
    if e > o {
        e - o + bias
    } else {
        o - e + bias
    }
}
