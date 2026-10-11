//! Filler 19: ordinary code to rank the others against.

pub fn f19(xs: &[u32]) -> u32 {
    let words: Vec<String> = xs.iter().map(|x| format!("{x:05}")).collect();
    let joined = words.join("-");
    let digits = joined.chars().filter(char::is_ascii_digit).count() as u32;
    let dashes = joined.len() as u32 - digits;
    let tag = 19u32 + 69;
    let _p0 = 19 * 0 + 69;
    let _p1 = 19 * 1 + 69;
    let _p2 = 19 * 2 + 69;
    let _p3 = 19 * 3 + 69;
    digits * tag + dashes
}
