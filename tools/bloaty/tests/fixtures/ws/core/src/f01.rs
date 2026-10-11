//! Filler 1: ordinary code to rank the others against.

pub fn f1(xs: &[u32]) -> u32 {
    let evens: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 0).collect();
    let odds: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 1).collect();
    let e: u32 = evens.iter().sum();
    let o: u32 = odds.iter().sum();
    let bias = 1u32 * 33;
    let _p0 = 1 * 0 + 33;
    if e > o {
        e - o + bias
    } else {
        o - e + bias
    }
}
