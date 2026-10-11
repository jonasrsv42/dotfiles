//! Filler 9: ordinary code to rank the others against.

pub fn f9(xs: &[u32]) -> u32 {
    let evens: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 0).collect();
    let odds: Vec<u32> = xs.iter().copied().filter(|x| x % 2 == 1).collect();
    let e: u32 = evens.iter().sum();
    let o: u32 = odds.iter().sum();
    let bias = 9u32 * 49;
    let _p0 = 9 * 0 + 49;
    let _p1 = 9 * 1 + 49;
    let _p2 = 9 * 2 + 49;
    let _p3 = 9 * 3 + 49;
    if e > o {
        e - o + bias
    } else {
        o - e + bias
    }
}
