//! Per-line classification of Rust source. syn drops comments, so this is a
//! small hand-rolled lexer that only knows enough Rust (strings, raw strings,
//! char literals vs lifetimes, nested block comments) to not be fooled by `//`
//! inside a string.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineClass {
    Blank,
    Comment,
    Doc,
    Code,
}

#[derive(Default, Clone, Copy)]
struct Flags {
    code: bool,
    doc: bool,
    comment: bool,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub struct Classified {
    pub lines: Vec<LineClass>,
    /// Deepest bracket nesting outside strings and comments. syn recurses
    /// once per level, so absurd depths (generated code) aren't parsed.
    pub max_depth: usize,
}

pub fn classify(src: &str) -> Classified {
    let s: Vec<char> = src.chars().collect();
    let n = s.len();
    let at = |i: usize| if i < n { s[i] } else { '\0' };
    let mut flags = vec![Flags::default(); src.lines().count() + 1];
    let mut line = 0;
    let (mut depth, mut max_depth) = (0usize, 0usize);
    let mut i = usize::from(at(0) == '\u{feff}');

    // Mark the current line; `flags` may be one short if the file ends mid-line.
    macro_rules! mark {
        ($field:ident) => {
            if let Some(f) = flags.get_mut(line) {
                f.$field = true;
            }
        };
    }

    // A `#!` first line that isn't an inner attribute `#![` is a shebang.
    if at(i) == '#' && at(i + 1) == '!' {
        let mut j = i + 2;
        while j < n && s[j].is_whitespace() && s[j] != '\n' {
            j += 1;
        }
        if at(j) != '[' {
            mark!(comment);
            while i < n && s[i] != '\n' {
                i += 1;
            }
        }
    }

    while i < n {
        let c = s[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            // `///x` and `//!x` are docs, `////` is a plain comment.
            if (at(i + 2) == '/' && at(i + 3) != '/') || at(i + 2) == '!' {
                mark!(doc);
            } else {
                mark!(comment);
            }
            while i < n && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == '*' {
            // `/**x` and `/*!x` are docs; `/**/` and `/***` are not.
            let doc =
                (at(i + 2) == '*' && at(i + 3) != '*' && at(i + 3) != '/') || at(i + 2) == '!';
            let mut depth = 1;
            i += 2;
            if doc {
                mark!(doc)
            } else {
                mark!(comment)
            }
            while i < n && depth > 0 {
                if s[i] == '\n' {
                    line += 1;
                    i += 1;
                    continue;
                }
                if !s[i].is_whitespace() {
                    if doc { mark!(doc) } else { mark!(comment) }
                }
                if s[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == '"' {
            i = skip_string(&s, i + 1, &mut line, &mut flags);
        } else if c == '\'' {
            mark!(code);
            if at(i + 1) == '\\' {
                // Escaped char literal: '\n', '\'', '\u{1F600}'.
                i += 3;
                while i < n && s[i] != '\'' && s[i] != '\n' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == '\'' {
                i += 3; // plain char literal
            } else {
                i += 1; // lifetime or label
            }
        } else if is_ident(c) {
            mark!(code);
            let start = i;
            // String prefixes: b"", c"", r"", r#""#, br"", cr#""#, and b'x'.
            let mut j = i;
            if matches!(s[j], 'b' | 'c') {
                j += 1;
            }
            if at(j) == 'r' && (at(j + 1) == '"' || at(j + 1) == '#') {
                let mut hashes = 0;
                let mut k = j + 1;
                while at(k) == '#' {
                    hashes += 1;
                    k += 1;
                }
                if at(k) == '"' {
                    i = skip_raw_string(&s, k + 1, hashes, &mut line, &mut flags);
                    continue;
                }
            } else if j > start && at(j) == '"' {
                i = skip_string(&s, j + 1, &mut line, &mut flags);
                continue;
            } else if s[start] == 'b' && at(start + 1) == '\'' {
                i = start + 1; // let the char-literal branch take it
                continue;
            }
            while i < n && is_ident(s[i]) {
                i += 1;
            }
        } else {
            mark!(code);
            match c {
                '(' | '[' | '{' => {
                    depth += 1;
                    max_depth = max_depth.max(depth);
                }
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            i += 1;
        }
    }

    flags.truncate(src.lines().count());
    let lines = flags
        .into_iter()
        .map(|f| {
            if f.code {
                LineClass::Code
            } else if f.doc {
                LineClass::Doc
            } else if f.comment {
                LineClass::Comment
            } else {
                LineClass::Blank
            }
        })
        .collect();
    Classified { lines, max_depth }
}

/// Skips past the closing `"` of a normal string starting at `i` (just after
/// the opening quote). Every line the string touches counts as code.
fn skip_string(s: &[char], mut i: usize, line: &mut usize, flags: &mut [Flags]) -> usize {
    mark_code(flags, *line);
    while i < s.len() {
        match s[i] {
            '\\' => {
                if s.get(i + 1) == Some(&'\n') {
                    *line += 1;
                    mark_code(flags, *line);
                }
                i += 2;
            }
            '\n' => {
                *line += 1;
                mark_code(flags, *line);
                i += 1;
            }
            '"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

fn skip_raw_string(
    s: &[char],
    mut i: usize,
    hashes: usize,
    line: &mut usize,
    flags: &mut [Flags],
) -> usize {
    mark_code(flags, *line);
    while i < s.len() {
        if s[i] == '\n' {
            *line += 1;
            mark_code(flags, *line);
        } else if s[i] == '"' && (1..=hashes).all(|h| s.get(i + h) == Some(&'#')) {
            return i + 1 + hashes;
        }
        i += 1;
    }
    i
}

fn mark_code(flags: &mut [Flags], line: usize) {
    if let Some(f) = flags.get_mut(line) {
        f.code = true;
    }
}

#[cfg(test)]
mod tests {
    use super::LineClass::*;
    use super::*;

    #[test]
    fn classifies_lines() {
        let src = r####"/// docs
//! inner docs
//// not docs
// comment

fn main() { // trailing comment is still code
    let s = "// not a comment";
    let r = r#"
/* still a string */
"#;
    /* block
       /* nested */
    */
    let c = '"'; let l: &'static str = "x";
}
"####;
        assert_eq!(
            classify("\u{feff}#!/usr/bin/env run-cargo-script\n//! doc\n#![allow(x)]\n").lines,
            vec![Comment, Doc, Code]
        );
        assert_eq!(
            classify(src).lines,
            vec![
                Doc, Doc, Comment, Comment, Blank, Code, Code, Code, Code, Code, Comment, Comment,
                Comment, Code, Code
            ]
        );
        assert_eq!(classify("fn f() { g([1, (2)]) } // ((((").max_depth, 4);
    }
}
