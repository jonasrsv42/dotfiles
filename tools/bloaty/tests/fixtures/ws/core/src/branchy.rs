//! A branchy fn, just under the bar (cyclomatic complexity 10): ordinary.

pub fn classify(c: char, strict: bool) -> u8 {
    if c.is_ascii_digit() {
        return 1;
    }
    if c.is_ascii_lowercase() {
        return 2;
    }
    if c.is_ascii_uppercase() {
        return 3;
    }
    match c {
        ' ' | '\t' => 4,
        '(' | ')' => 5,
        '+' | '-' if !strict => 6,
        '*' => 7,
        _ => {
            if c.is_whitespace() || c.is_control() {
                8
            } else if strict && c.is_alphabetic() {
                9
            } else {
                0
            }
        }
    }
}
