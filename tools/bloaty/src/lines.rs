//! Per-line classification of Rust source. syn drops comments, so this is a
//! small lexer that only knows enough Rust (strings, raw strings, char
//! literals vs lifetimes, nested block comments) to not be fooled by `//`
//! inside a string. Two stages: `tokens` splits the source into tokens with
//! their byte and line spans; `Fold` consumes them, marking each line and
//! tracking how deep syn would recurse.

mod tokens;

use tokens::{Kind, Token, is_ident, tokens};

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
    let mut fold = Fold::new(src);
    for t in tokens(src) {
        fold.token(t);
    }
    let n = src.lines().count();
    fold.flags.truncate(n);
    let lines = fold
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
        max_depth: fold.max_depth,
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

impl Mark {
    fn comment(doc: bool) -> Mark {
        if doc { Mark::Doc } else { Mark::Comment }
    }
}

/// What the last significant token was, as far as nesting cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    /// An identifier or literal.
    Operand,
    /// `)`, `]` or `}`.
    Close,
    /// Anything else: operators, keywords, lifetimes, the start.
    Other,
}

/// Marks lines and tracks nesting over the tokens.
struct Fold<'a> {
    src: &'a str,
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
    /// chain, angle and postfix of the enclosing bracket levels, and
    /// whether each is a `{` block.
    saved: Vec<(usize, usize, usize, bool)>,
    after_brace: bool,
    prev: Prev,
    max_depth: usize,
}

impl<'a> Fold<'a> {
    fn new(src: &'a str) -> Self {
        Fold {
            src,
            flags: vec![Flags::default(); src.lines().count() + 1],
            depth: 0,
            chain: 0,
            postfix: 0,
            angle: 0,
            outer: 0,
            saved: Vec::new(),
            after_brace: false,
            prev: Prev::Other,
            max_depth: 0,
        }
    }

    fn mark(&mut self, line: usize, m: Mark) {
        if let Some(f) = self.flags.get_mut(line) {
            match m {
                Mark::Code => f.code = true,
                Mark::Doc => f.doc = true,
                Mark::Comment => f.comment = true,
            }
        }
    }

    fn token(&mut self, t: Token) {
        let text = &self.src[t.start..t.end];
        match t.kind {
            Kind::Space => {}
            Kind::Shebang => self.mark(t.line, Mark::Comment),
            Kind::LineComment { doc } => self.mark(t.line, Mark::comment(doc)),
            // Lines inside count only if they have more than whitespace.
            Kind::BlockComment { doc } => {
                for (k, part) in text.split('\n').enumerate() {
                    if k == 0 || !part.trim().is_empty() {
                        self.mark(t.line + k, Mark::comment(doc));
                    }
                }
            }
            _ => self.code(t, text),
        }
    }

    /// Every line a code token touches is code (several only for strings).
    fn code(&mut self, t: Token, text: &str) {
        if std::mem::take(&mut self.after_brace) && !self.continues(t, text) {
            (self.chain, self.angle, self.postfix) = (0, 0, 0);
        }
        for line in t.line..=t.last_line {
            self.mark(line, Mark::Code);
        }
        self.prev = match t.kind {
            Kind::Ident => self.word(text),
            Kind::Punct => {
                self.punct(t, text);
                match text {
                    ")" | "]" | "}" => Prev::Close,
                    _ => Prev::Other,
                }
            }
            // A lifetime isn't an operand: `&'a &'a T` nests.
            Kind::Lifetime => Prev::Other,
            _ => Prev::Operand,
        };
    }

    /// `{..} + |x| ..` continues an expression; anything else after a
    /// closing brace starts a new statement or item.
    fn continues(&self, t: Token, text: &str) -> bool {
        match t.kind {
            Kind::Ident => text == "as",
            Kind::Punct => {
                let next = self.src.as_bytes().get(t.start + 1).copied();
                let c = text.as_bytes()[0];
                b"+-*/%^&|<>=.?".contains(&c) || (c == b'!' && next == Some(b'='))
            }
            _ => false,
        }
    }

    fn bump(&mut self) {
        self.max_depth = self
            .max_depth
            .max(self.depth + self.outer + self.chain + self.angle + self.postfix / 500);
    }

    /// Keywords starting a nested expression recurse in syn like prefix
    /// operators: `return return ..`, `if if ..`, `match match ..`. After
    /// them and a few others, `*`/`&`/`-` start an operand: `*const *const
    /// T`.
    fn word(&mut self, word: &str) -> Prev {
        const NESTING: &[&str] = &[
            "return", "break", "if", "match", "while", "yield", "become", "let", "in", "move",
            "async", "unsafe",
        ];
        const OTHER: &[&str] = &[
            "as", "const", "mut", "dyn", "impl", "else", "for", "loop", "where", "static", "ref",
            "fn",
        ];
        if NESTING.contains(&word) {
            self.chain += 1;
            self.bump();
            Prev::Other
        } else if OTHER.contains(&word) {
            Prev::Other
        } else {
            Prev::Operand
        }
    }

    /// Tracks nesting for one punctuation token.
    fn punct(&mut self, t: Token, op: &str) {
        let bytes = self.src.as_bytes();
        let at = |i: Option<usize>| i.and_then(|i| bytes.get(i)).copied().unwrap_or(0);
        let before = self.src[..t.start].chars().next_back().unwrap_or('\0');
        let next = at(Some(t.start + 1));
        // In operand position (`!x`, `&x`, `*const *const T`, `|x| ..`), not
        // after an operand (`a & b`).
        let prefix = !matches!(self.prev, Prev::Operand | Prev::Close);
        if op == "?" || (matches!(op.as_bytes()[0], b'.' | b'(' | b'[') && !prefix) {
            self.postfix += 1;
        }
        let chained = match op {
            "(" | "[" | "{" => self.open(op == "{"),
            ")" | "]" | "}" => self.close(op == "}"),
            "||" | "&&" => {
                if !prefix {
                    self.angle = 0; // `a < b && ..`: that `<` was a comparison
                }
                prefix
            }
            ".." | "&" | "*" | "|" => prefix,
            "-" => prefix || next == b'>',
            "!" => prefix && next != b'=',
            "@" | ">>=" => true,
            // Assignment, also compound (`+=`, `<<=`); not `<=`, `>=`, `!=`.
            "=" => {
                !matches!(before, '=' | '<' | '>' | '!' | '.')
                    || (matches!(before, '<' | '>') && at(t.start.checked_sub(2)) == before as u8)
            }
            "<" => {
                self.angle_open(before, next, prefix);
                false
            }
            ">" => {
                self.angle_close(before, next);
                false
            }
            _ => false,
        };
        if chained {
            self.chain += 1;
        }
        if matches!(op, ";" | ",") {
            self.chain = 0;
            self.postfix = 0;
        }
        if op == ";" {
            self.angle = 0;
        }
        self.bump();
    }

    fn open(&mut self, block: bool) -> bool {
        self.bump();
        self.saved
            .push((self.chain, self.angle, self.postfix, block));
        // A block's own nesting restarts (`if x {`, `fn f() -> &T {`); what
        // it is part of resumes after it (`|x| {..} + |y| ..`).
        if !block {
            self.outer += self.chain + self.angle;
        }
        (self.chain, self.angle, self.postfix) = (0, 0, 0);
        self.depth += 1;
        false
    }

    fn close(&mut self, brace: bool) -> bool {
        if let Some((ch, an, pf, block)) = self.saved.pop() {
            if !block {
                self.outer -= ch + an;
            }
            (self.chain, self.angle, self.postfix) = (ch, an, pf);
        }
        self.depth = self.depth.saturating_sub(1);
        self.after_brace = brace;
        false
    }

    /// Generics (`Vec<..`, `Vec < ..`) and qualified paths (`<<T as A>::B as
    /// C>::D`); not shifts (`a << b`), `<=` or `f() < x`.
    fn angle_open(&mut self, before: char, next: u8, prefix: bool) {
        let generic = is_ident(before)
            || before == ':'
            || prefix
            || (self.prev == Prev::Operand && !matches!(next, b'<' | b'=') && before != '<');
        if generic {
            self.angle += 1;
        }
    }

    /// Closing a generic ends what was nested in it (`Item = ..`, markup
    /// attributes); a comparison (`|x| x > 0 || ..`) doesn't. Not `->`, `=>`
    /// or `>=`.
    fn angle_close(&mut self, before: char, next: u8) {
        if !matches!(before, '-' | '=') && next != b'=' && self.angle > 0 {
            self.angle -= 1;
            self.chain = 0;
        }
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

    #[test]
    fn reads_non_ascii_as_characters() {
        let class = |src: &str| classify(src).lines;
        let depth = |src: &str| classify(src).max_depth;
        // A leading byte order mark isn't code; Unicode spaces are blank.
        assert_eq!(class("\u{feff}fn f() {}\n"), vec![Code]);
        assert_eq!(depth("\u{feff}fn f() {}\n"), 1);
        assert_eq!(
            class("let é = 1;\n\u{a0}\n// ü\n"),
            vec![Code, Blank, Comment]
        );
        assert_eq!(
            class("/* ✓\n\u{3000}\n*/ let c = 'é';\n"),
            vec![Comment, Blank, Code]
        );
        assert_eq!(class("x\u{2028}// c\n"), vec![Code]);
        assert_eq!(depth("let s = \"→ /* no */\";\n"), 2);
        assert_eq!(depth("let l: &'é str = x; let c = b'\\'';\n"), 3);
        // `é<` opens a generic like `e<`.
        assert_eq!(depth("fn f() { Vecé<Vecé<u8>> }\n"), 3);
    }

    #[test]
    fn deep_comment_nesting_is_counted_not_recursed() {
        let src = format!("fn f() {{}}\n{}\nx", "/*".repeat(1_000_000));
        let c = classify(&src);
        assert_eq!(c.lines, vec![Code, Comment, Comment]);
        assert_eq!(c.max_depth, 1);
    }
}
