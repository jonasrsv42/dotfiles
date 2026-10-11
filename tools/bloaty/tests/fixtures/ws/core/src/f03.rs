//! Filler 3: ordinary code to rank the others against.

pub fn f3(xs: &[u32]) -> u32 {
    let words: Vec<String> = xs.iter().map(|x| format!("{x:05}")).collect();
    let joined = words.join("-");
    let digits = joined.chars().filter(char::is_ascii_digit).count() as u32;
    let dashes = joined.len() as u32 - digits;
    let tag = 3u32 + 37;
    let _p0 = 3 * 0 + 37;
    let _p1 = 3 * 1 + 37;
    let _p2 = 3 * 2 + 37;
    digits * tag + dashes
}
