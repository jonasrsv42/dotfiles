//! Filler 7: ordinary code to rank the others against.

pub fn f7(xs: &[u32]) -> u32 {
    let words: Vec<String> = xs.iter().map(|x| format!("{x:05}")).collect();
    let joined = words.join("-");
    let digits = joined.chars().filter(char::is_ascii_digit).count() as u32;
    let dashes = joined.len() as u32 - digits;
    let tag = 7u32 + 45;
    let _p0 = 7 * 0 + 45;
    let _p1 = 7 * 1 + 45;
    digits * tag + dashes
}
