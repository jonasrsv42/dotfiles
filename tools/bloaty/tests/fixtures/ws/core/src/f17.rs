//! Filler 17: ordinary code to rank the others against.

pub fn f17(xs: &[u32]) -> u32 {
    let evens: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 0).collect();
    let odds: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 1).collect();
    let e: u32 = evens.iter().sum();
    let o: u32 = odds.iter().sum();
    let bias = 17u32 * 65;
    let _p0 = 17 * 0 + 65;
    let _p1 = 17 * 1 + 65;
    if e > o {
        e - o + bias
    } else {
        o - e + bias
    }
}
