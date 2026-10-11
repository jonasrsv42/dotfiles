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

pub struct Classified {
    pub lines: Vec<LineClass>,
    /// Deepest nesting outside strings and comments: brackets, plus runs of
    /// prefix operators within one statement (`!!!x`, `a = a = ..`, `|x|
    /// |x| ..`), which syn also recurses on. Absurd depths (generated code)
    /// aren't parsed.
    pub max_depth: usize,
}

pub fn classify(src: &str) -> Classified {
    let mut lx = Lexer {
        s: src.chars().collect(),
        i: 0,
        line: 0,
        flags: vec![Flags::default(); src.lines().count() + 1],
        depth: 0,
        chain: 0,
        postfix: 0,
        angle: 0,
        outer: 0,
        saved: Vec::new(),
        after_brace: false,
        prev: '\0',
        max_depth: 0,
    };
    lx.i = usize::from(lx.at(0) == '\u{feff}');
    lx.shebang();
    while lx.i < lx.s.len() {
        lx.token();
    }
    lx.flags.truncate(src.lines().count());
    let lines = lx
        .flags
        .iter()
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
    Classified {
        lines,
        max_depth: lx.max_depth,
    }
}

/// What a line contains; the strongest wins (code, then doc, then comment).
#[derive(Default, Clone, Copy)]
struct Flags {
    code: bool,
    doc: bool,
    comment: bool,
}

#[derive(Clone, Copy)]
enum Mark {
    Code,
    Doc,
    Comment,
}

struct Lexer {
    s: Vec<char>,
    i: usize,
    /// 0-based line of `s[i]`.
    line: usize,
    /// One more than the line count: a file may end mid-line.
    flags: Vec<Flags>,
    depth: usize,
    /// Recursion-prone tokens since the last `;`, `,`, brace or closing
    /// `>`: prefix operators, assignments, generics and keywords that start
    /// a nested expression. Binary operators don't count: syn parses
    /// `a || b || ..` and `"a" | "b" | ..` patterns flat.
    chain: usize,
    /// Postfix operators (`.f`, `()`, `[i]`, `?`) since the same resets.
    /// syn recurses on those too, but only overflows past ~1M in one
    /// expression, so they weigh 1/500 of a level.
    postfix: usize,
    /// Open `<` at this bracket level.
    angle: usize,
    /// chain + angle of the enclosing bracket levels.
    outer: usize,
    saved: Vec<(usize, usize, usize)>,
    after_brace: bool,
    /// The last significant character: `a` after an identifier or literal,
    /// `k` after a keyword.
    prev: char,
    max_depth: usize,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Lexer {
    fn at(&self, i: usize) -> char {
        self.s.get(i).copied().unwrap_or('\0')
    }

    fn mark(&mut self, m: Mark) {
        if let Some(f) = self.flags.get_mut(self.line) {
            match m {
                Mark::Code => f.code = true,
                Mark::Doc => f.doc = true,
                Mark::Comment => f.comment = true,
            }
        }
    }

    /// A `#!` first line that isn't an inner attribute `#![` is a shebang.
    fn shebang(&mut self) {
        if self.at(self.i) != '#' || self.at(self.i + 1) != '!' {
            return;
        }
        let mut j = self.i + 2;
        while self.at(j).is_whitespace() && self.at(j) != '\n' {
            j += 1;
        }
        if self.at(j) != '[' {
            self.mark(Mark::Comment);
            self.skip_to_eol();
        }
    }

    fn skip_to_eol(&mut self) {
        while self.i < self.s.len() && self.s[self.i] != '\n' {
            self.i += 1;
        }
    }

    /// Consumes one token (or newline/whitespace) at `i`.
    fn token(&mut self) {
        let (c, next) = (self.s[self.i], self.at(self.i + 1));
        let trivia = c.is_whitespace() || (c == '/' && matches!(next, '/' | '*'));
        if self.after_brace && !trivia {
            self.after_brace = false;
            // `{..} + |x| ..` continues an expression; anything else after a
            // closing brace starts a new statement or item.
            let continues = matches!(
                c,
                '+' | '-' | '*' | '/' | '%' | '^' | '&' | '|' | '<' | '>' | '=' | '.' | '?'
            ) || (c == '!' && next == '=')
                || (c == 'a' && next == 's' && !is_ident(self.at(self.i + 2)));
            if !continues {
                self.chain = 0;
                self.angle = 0;
                self.postfix = 0;
            }
        }
        match c {
            '\n' => {
                self.line += 1;
                self.i += 1;
            }
            c if c.is_whitespace() => self.i += 1,
            '/' if next == '/' => self.line_comment(),
            '/' if next == '*' => self.block_comment(),
            '"' => {
                self.i += 1;
                self.string();
                self.prev = 'a';
            }
            '\'' => {
                // A lifetime isn't an operand: `&'a &'a T` nests.
                self.prev = if self.char_or_lifetime() { 'a' } else { 'l' };
            }
            c if is_ident(c) => {
                self.prev = if self.ident_or_prefixed_literal() {
                    'k'
                } else {
                    'a'
                };
            }
            c => {
                self.mark(Mark::Code);
                self.punct(c, next);
                self.prev = c;
                self.i += 1;
            }
        }
    }

    fn bump(&mut self) {
        self.max_depth = self
            .max_depth
            .max(self.depth + self.outer + self.chain + self.angle + self.postfix / 500);
    }

    /// Tracks nesting for one punctuation character (two for `||`, `&&`,
    /// `==`, `=>`).
    fn punct(&mut self, c: char, next: char) {
        let before = self.at(self.i.wrapping_sub(1));
        // In operand position (`!x`, `&x`, `*const *const T`, `|x| ..`), not
        // after an operand (`a & b`).
        let prefix = !matches!(self.prev, 'a' | ')' | ']' | '}');
        if c == '?' || (matches!(c, '.' | '(' | '[') && !prefix) {
            self.postfix += 1;
        }
        let chained = match c {
            '(' | '[' | '{' => {
                self.bump();
                self.saved.push((self.chain, self.angle, self.postfix));
                // A block's own nesting restarts (`if x {`, `fn f() -> &T {`);
                // what it is part of resumes after it (`|x| {..} + |y| ..`).
                if c == '{' {
                    self.saved.last_mut().expect("just pushed").0 |= 1 << 63;
                } else {
                    self.outer += self.chain + self.angle;
                }
                (self.chain, self.angle, self.postfix) = (0, 0, 0);
                self.depth += 1;
                false
            }
            ')' | ']' | '}' => {
                if let Some((ch, an, pf)) = self.saved.pop() {
                    let block = ch & (1 << 63) != 0;
                    let ch = ch & !(1 << 63);
                    if !block {
                        self.outer -= ch + an;
                    }
                    (self.chain, self.angle, self.postfix) = (ch, an, pf);
                }
                self.depth = self.depth.saturating_sub(1);
                self.after_brace = c == '}';
                false
            }
            '|' | '&' if next == c => {
                self.i += 1; // `||` / `&&`: one token
                if !prefix {
                    self.angle = 0; // `a < b && ..`: that `<` was a comparison
                }
                prefix
            }
            '=' if matches!(next, '=' | '>') => {
                self.i += 1; // `==` / `=>`
                false
            }
            '=' => {
                !matches!(before, '=' | '<' | '>' | '!' | '.')
                    || (matches!(before, '<' | '>') && self.at(self.i.wrapping_sub(2)) == before)
            }
            '.' if next == '.' => {
                self.i += 1;
                prefix
            }
            '@' => true,
            '>' if next == '>' && self.at(self.i + 2) == '=' => {
                self.i += 2;
                true
            }
            '-' if next == '>' => true,
            '!' if next == '=' => false,
            '-' | '!' | '&' | '*' | '|' => prefix,
            // Generics (`Vec<..`, `Vec < ..`) and qualified paths (`<<T as
            // A>::B as C>::D`); not shifts (`a << b`), `<=` or `f() < x`.
            '<' => {
                let generic = is_ident(before)
                    || before == ':'
                    || prefix
                    || (self.prev == 'a' && !matches!(next, '<' | '=') && before != '<');
                if generic {
                    self.angle += 1;
                }
                false
            }
            '>' if !matches!(before, '-' | '=') && next != '=' => {
                // Closing a generic ends what was nested in it (`Item = ..`,
                // markup attributes); a comparison (`|x| x > 0 || ..`) doesn't.
                if self.angle > 0 {
                    self.angle -= 1;
                    self.chain = 0;
                }
                false
            }
            _ => false,
        };
        if chained {
            self.chain += 1;
        }
        if matches!(c, ';' | ',') {
            self.chain = 0;
            self.postfix = 0;
        }
        if c == ';' {
            self.angle = 0;
        }
        self.bump();
    }

    /// `///x` and `//!x` are docs, `////` is a plain comment.
    fn line_comment(&mut self) {
        let (c2, c3) = (self.at(self.i + 2), self.at(self.i + 3));
        let doc = (c2 == '/' && c3 != '/') || c2 == '!';
        self.mark(if doc { Mark::Doc } else { Mark::Comment });
        self.skip_to_eol();
    }

    /// Nested block comments. `/**x` and `/*!x` are docs; `/**/` and `/***`
    /// are not. Lines inside count only if they have more than whitespace.
    fn block_comment(&mut self) {
        let (c2, c3) = (self.at(self.i + 2), self.at(self.i + 3));
        let kind = if (c2 == '*' && c3 != '*' && c3 != '/') || c2 == '!' {
            Mark::Doc
        } else {
            Mark::Comment
        };
        self.mark(kind);
        self.i += 2;
        let mut depth = 1;
        while self.i < self.s.len() && depth > 0 {
            let (c, next) = (self.s[self.i], self.at(self.i + 1));
            if c == '\n' {
                self.line += 1;
                self.i += 1;
                continue;
            }
            if !c.is_whitespace() {
                self.mark(kind);
            }
            match (c, next) {
                ('/', '*') => {
                    depth += 1;
                    self.i += 2;
                }
                ('*', '/') => {
                    depth -= 1;
                    self.i += 2;
                }
                _ => self.i += 1,
            }
        }
    }

    /// From just after the opening `"` to just past the closing one. Every
    /// line the string touches counts as code.
    fn string(&mut self) {
        self.mark(Mark::Code);
        while self.i < self.s.len() {
            match self.s[self.i] {
                '\\' => {
                    if self.at(self.i + 1) == '\n' {
                        self.line += 1;
                        self.mark(Mark::Code);
                    }
                    self.i += 2;
                }
                '\n' => {
                    self.line += 1;
                    self.mark(Mark::Code);
                    self.i += 1;
                }
                '"' => {
                    self.i += 1;
                    return;
                }
                _ => self.i += 1,
            }
        }
    }

    /// From just after the opening `"` of `r##"..."##` to past its end.
    fn raw_string(&mut self, hashes: usize) {
        self.mark(Mark::Code);
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c == '\n' {
                self.line += 1;
                self.mark(Mark::Code);
            } else if c == '"' && (1..=hashes).all(|h| self.at(self.i + h) == '#') {
                self.i += 1 + hashes;
                return;
            }
            self.i += 1;
        }
    }

    /// `'a'`, `'\n'`, `'\u{1F600}'`, or a lifetime/label `'a`.
    fn char_or_lifetime(&mut self) -> bool {
        self.mark(Mark::Code);
        if self.at(self.i + 1) == '\\' {
            self.i += 3;
            while self.i < self.s.len() && !matches!(self.s[self.i], '\'' | '\n') {
                self.i += 1;
            }
            self.i += 1;
            true
        } else if self.at(self.i + 2) == '\'' {
            self.i += 3;
            true
        } else {
            self.i += 1;
            while self.i < self.s.len() && is_ident(self.s[self.i]) {
                self.i += 1;
            }
            // Not a lifetime after all (`'ab'`): close it as a literal.
            let literal = self.at(self.i) == '\'';
            self.i += usize::from(literal);
            literal
        }
    }

    /// An identifier, or a prefixed literal: `b".."`, `c".."`, `r".."`,
    /// `r#".."#`, `br".."`, `cr#".."#`, `b'x'`. Returns whether it was a
    /// keyword.
    fn ident_or_prefixed_literal(&mut self) -> bool {
        self.mark(Mark::Code);
        let start = self.i;
        let mut j = start;
        if matches!(self.s[j], 'b' | 'c') {
            j += 1;
        }
        if self.at(j) == 'r' && matches!(self.at(j + 1), '"' | '#') {
            let mut k = j + 1;
            while self.at(k) == '#' {
                k += 1;
            }
            if self.at(k) == '"' {
                self.i = k + 1;
                self.raw_string(k - j - 1);
                return false;
            }
        } else if j > start && self.at(j) == '"' {
            self.i = j + 1;
            self.string();
            return false;
        } else if self.s[start] == 'b' && self.at(start + 1) == '\'' {
            self.i = start + 1;
            self.char_or_lifetime();
            return false;
        }
        while self.i < self.s.len() && is_ident(self.s[self.i]) {
            self.i += 1;
        }
        // Keywords starting a nested expression recurse in syn like prefix
        // operators: `return return ..`, `if if ..`, `match match ..`.
        const NESTING: &[&str] = &[
            "return", "break", "if", "match", "while", "yield", "become", "let", "in", "move",
            "async", "unsafe",
        ];
        // After these, `*`/`&`/`-` start an operand: `*const *const T`.
        const OTHER: &[&str] = &[
            "as", "const", "mut", "dyn", "impl", "else", "for", "loop", "where", "static", "ref",
            "fn",
        ];
        let word = &self.s[start..self.i];
        let is = |list: &[&str]| {
            word.len() <= 6 && list.iter().any(|k| k.chars().eq(word.iter().copied()))
        };
        if is(NESTING) {
            self.chain += 1;
            self.bump();
            return true;
        }
        is(OTHER)
    }
}

/// A file's lines by class, test code apart.
#[derive(Clone, Copy, Default, Debug)]
pub struct LineCounts {
    /// Non-test code lines.
    pub implementation: usize,
    pub test: usize,
    pub doc: usize,
    pub comment: usize,
    pub blank: usize,
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

    #[test]
    fn nesting_counts_what_syn_recurses_on() {
        let depth = |src: &str| classify(src).max_depth;
        // Flat for syn: binary operators, or-patterns, markup.
        let ors = (0..500)
            .map(|i| format!("x == {i}"))
            .collect::<Vec<_>>()
            .join(" || ");
        assert!(depth(&format!("fn f() {{ {ors} }}")) < 10);
        let pats = (0..500)
            .map(|i| format!("\"k{i}\""))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(depth(&format!("fn f() {{ matches!(s, {pats}) }}")) < 10);
        assert!(
            depth(&format!(
                "view! {{ {} }}",
                "<li class=\"a\">x</li>".repeat(500)
            )) < 10
        );
        // Recursive: prefix operators, assignments, keywords, closures.
        assert!(depth(&format!("fn f() {{ {}x }}", "!".repeat(500))) > 500);
        assert!(depth(&format!("fn f() {{ {}1 }}", "a = ".repeat(500))) > 500);
        assert!(depth(&format!("fn f() {{ {}1 }}", "return ".repeat(500))) > 500);
        assert!(depth(&format!("fn f() {{ {}0 }}", "|x| ".repeat(500))) > 500);
        assert!(
            depth(&format!(
                "type T = {}u8{};",
                "Vec<".repeat(500),
                ">".repeat(500)
            )) > 500
        );
        assert!(depth(&format!("type T = {}u8;", "*const ".repeat(500))) > 500);
        assert!(depth(&format!("fn f() {{ {}1; }}", "a += ".repeat(500))) > 500);
        assert!(depth(&format!("fn f() {{ {}1; }}", "a >>= ".repeat(500))) > 500);
        assert!(depth(&format!("fn f() {{ {}1; }}", ".. ".repeat(500))) > 500);
        assert!(
            depth(&format!(
                "type T = {}u8{};",
                "<".repeat(500),
                " as X>::Y".repeat(500)
            )) > 500
        );
        // Range patterns are flat.
        let ranges = (0..500)
            .map(|i| format!("'{i}'..='z'"))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            depth(&format!(
                "fn f(c: char) -> bool {{ matches!(c, {ranges}) }}"
            )) < 10
        );
        assert!(depth(&format!("type T = {}u8;", "fn() -> ".repeat(500))) > 500);
        assert!(depth(&format!("type T = Box<{}u8>;", "dyn Fn() -> ".repeat(500))) > 500);
    }

    #[test]
    fn handles_literal_edge_cases() {
        let src = "let a = '\\'';\nlet b = b'\"';\nlet c = br#\"\n// in raw\n\"#;\nlet d = c\"x\";\n/**/ let e = r#type;\n/***/\n";
        assert_eq!(
            classify(src).lines,
            vec![Code, Code, Code, Code, Code, Code, Code, Comment]
        );
    }
}
