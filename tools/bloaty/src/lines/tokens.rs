//! The tokenizer: just enough of Rust's lexical grammar (comments, strings,
//! raw strings, char literals vs lifetimes) to tell code from comments,
//! written as parsicomb parsers over the source's bytes. Each rule's doc
//! states its grammar; where a rule is hand-written, its doc says why.

use parsicomb::and::AndExt;
use parsicomb::and_then::AndThenExt;
use parsicomb::atomic::is_sequence;
use parsicomb::byte::byte;
use parsicomb::map::MapExt;
use parsicomb::map_err::MapErrExt;
use parsicomb::not::not;
use parsicomb::or::OrExt;
use parsicomb::position::PositionExt;
use parsicomb::recognize::RecognizeExt;
use parsicomb::utf8::char::char;
use parsicomb::utf8::{skip_chars_while, skip_chars_while1};
use parsicomb::{ByteCursor, Cursor, Parser, ParsicombError, Span};
use parsicomb::{ntimes, optional, parser_fn, skip_many, skip_while, skip_while1};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    /// A run of whitespace, newlines included.
    Space,
    /// A `#!` first line that isn't an inner attribute `#![`.
    Shebang,
    LineComment {
        doc: bool,
    },
    BlockComment {
        doc: bool,
    },
    /// A string literal, plain, raw or prefixed: `".."`, `b".."`, `r#".."#`.
    Str,
    /// A char literal `'x'` or `b'x'`. Its newlines aren't counted as line
    /// breaks: only malformed source has any (an unterminated escape such as
    /// `'\x` or `'\u{41` before a line break), and later lines then shift up.
    Char,
    /// A lifetime or label `'a`.
    Lifetime,
    /// An identifier, keyword or number.
    Ident,
    /// One character, or one of `||`, `&&`, `==`, `=>`, `..`, `>>=`.
    Punct,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Token {
    pub kind: Kind,
    /// Byte span in the source.
    pub start: usize,
    pub end: usize,
    /// 0-based lines of its first and last character.
    pub line: usize,
    pub last_line: usize,
}

pub(super) fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// file = byte order mark? shebang? token*
pub(super) fn tokens(src: &str) -> impl Iterator<Item = Token> + '_ {
    let start = ByteCursor::new(src.as_bytes());
    let head = optional(tag("\u{feff}")).and(optional(shebang().with_position()));
    // Both parts are optional, so this never fails.
    let ((_, shebang), mut cursor) = head.parse(start).unwrap_or(((None, None), start));
    let token = token().with_position();
    // token*, streamed: collecting it with `many` made lexing 2x slower.
    let rest = std::iter::from_fn(move || {
        if cursor.eos() {
            return None;
        }
        let parsed = token.parse(cursor);
        debug_assert!(parsed.is_ok(), "no token at {}", cursor.position());
        let ((kind, span), next) = parsed.ok()?;
        cursor = next;
        Some((kind, span))
    });
    let mut line = 0;
    shebang.into_iter().chain(rest).map(move |(kind, span)| {
        let first_line = line;
        if kind != Kind::Char {
            line += span.slice().iter().filter(|&&b| b == b'\n').count();
        }
        Token {
            kind,
            start: span.start(),
            end: span.end(),
            line: first_line,
            last_line: line,
        }
    })
}

type Cur<'a> = ByteCursor<'a>;

/// A parser over the source whose error converts into `ParsicombError`
/// without allocating, as the error of any parsicomb combinator does.
trait Rule<'a, O>: Parser<Cursor = Cur<'a>, Output = O, Error: Into<ParsicombError<'a>>> {}

impl<'a, O, P> Rule<'a, O> for P where
    P: Parser<Cursor = Cur<'a>, Output = O, Error: Into<ParsicombError<'a>>>
{
}

/// A rule with its error converted (without allocating), so the
/// dispatcher's arms share an error type; arms aren't boxed: ~7% faster.
fn rule<'a>(
    p: impl Rule<'a, Kind>,
) -> impl Parser<Cursor = Cur<'a>, Output = Kind, Error = ParsicombError<'a>> {
    p.map_err(Into::into)
}

/// token = space | line comment | block comment | string | char or
/// lifetime | prefixed literal | ident | punct
///
/// Only the alternatives a token's first byte allows are tried, as in
/// rustc's lexer: an `or` chain trying each in turn made lexing ~9x
/// slower, still ~4x with each alternative guarded by its first byte
/// (parsicomb 9678f4b). Every arm takes at least one character, so
/// `token*` reaches the end.
fn token<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind, Error = ParsicombError<'a>> {
    let space = || skip_chars_while1(char::is_whitespace, "whitespace").map(|_| Kind::Space);
    let ident = || skip_chars_while1(is_ident, "an identifier").map(|_| Kind::Ident);
    let space_rule = rule(space());
    let slash = rule(line_comment().or(block_comment()).or(punct()));
    let string = rule(string().map(|_| Kind::Str));
    let quote = rule(char_or_lifetime());
    let prefixed = rule(prefixed_literal().or(ident()));
    let ident_rule = rule(ident());
    // Characters a joined operator (`||`, `..`) starts with.
    let joined = rule(punct());
    let single = rule(char().map(|_| Kind::Punct));
    // Non-ASCII: Unicode whitespace, identifiers, anything else.
    let other = rule(space().or(ident()).or(punct()));
    parser_fn(move |cursor: Cur<'a>| {
        let literal = |p: &&'static str| tag(p).parse(cursor).is_ok();
        match cursor.value()? {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c' => space_rule.parse(cursor),
            b'/' => slash.parse(cursor),
            b'"' => string.parse(cursor),
            b'\'' => quote.parse(cursor),
            b'b' | b'c' | b'r' if LITERAL_PREFIXES.iter().any(literal) => prefixed.parse(cursor),
            b if b.is_ascii_alphanumeric() || b == b'_' => ident_rule.parse(cursor),
            b'|' | b'&' | b'=' | b'.' | b'>' => joined.parse(cursor),
            b if b.is_ascii() => single.parse(cursor),
            _ => other.parse(cursor),
        }
    })
}

/// How prefixed literals (and raw identifiers, `r#type`) start. Checking
/// this before trying them keeps `return`, `crate`.. off three failing
/// alternatives: ~13% of lexing time on real code.
const LITERAL_PREFIXES: &[&str] = &[
    "b\"", "b'", "br\"", "br#", "c\"", "cr\"", "cr#", "r\"", "r#",
];

/// shebang = not attribute `#!` .. line end, attribute = `#!` spaces `[`
fn shebang<'a>() -> impl Rule<'a, Kind> {
    let spaces = skip_chars_while(|c| c.is_whitespace() && c != '\n');
    let attribute = tag("#!").and(spaces).and(tag("["));
    not(attribute)
        .and(tag("#!"))
        .and(skip_while(|b: u8| b != b'\n'))
        .map(|_| Kind::Shebang)
}

/// line comment = `//` doc? .. line end, doc = not `//` `/` | `!`: `///x`
/// and `//!x` are docs, `////` isn't.
fn line_comment<'a>() -> impl Rule<'a, Kind> {
    let doc = not(tag("//"))
        .and(tag("/"))
        .map(drop)
        .or(tag("!").map(drop));
    tag("//")
        .and(optional(doc))
        .and(skip_while(|b: u8| b != b'\n'))
        .map(|((_, doc), _)| Kind::LineComment { doc: doc.is_some() })
}

/// block comment = `/*` doc? body, doc = not (`**` | `*/`) `*` | `!`:
/// `/**x` and `/*!x` are docs, `/**/` and `/***` aren't.
fn block_comment<'a>() -> impl Rule<'a, Kind> {
    let doc = not(tag("**").or(tag("*/")))
        .and(tag("*"))
        .map(drop)
        .or(tag("!").map(drop));
    tag("/*")
        .and(optional(doc))
        .and(block_body())
        .map(|((_, doc), _)| Kind::BlockComment { doc: doc.is_some() })
}

/// body = (plain | `/*` body | not `*/` byte)* `*/`?: comments nest.
/// Hand-written to count the nesting: recursing into it overflowed the
/// stack at ~60k nested `/*`.
fn block_body<'a>() -> impl Parser<Cursor = Cur<'a>, Output = (), Error = ParsicombError<'a>> {
    let plain = skip_while1(|b: u8| !matches!(b, b'/' | b'*'), "comment text");
    parser_fn(move |mut cursor: Cur<'a>| {
        let mut depth = 1;
        while depth > 0 && !cursor.eos() {
            (depth, cursor) = match (tag("/*").parse(cursor), tag("*/").parse(cursor)) {
                (Ok((_, next)), _) => (depth + 1, next),
                (_, Ok((_, next))) => (depth - 1, next),
                _ => (depth, plain.parse(cursor).map_or(cursor.next(), |(_, c)| c)),
            };
        }
        Ok(((), cursor))
    })
}

/// string = `"` (plain | `\` byte?)* `"`?
fn string<'a>() -> impl Rule<'a, ()> {
    let plain = skip_while1(|b: u8| !matches!(b, b'"' | b'\\'), "string text");
    let escape = tag("\\").and(optional(byte())).map(drop);
    tag("\"")
        .and(skip_many(plain.or(escape)))
        .and(optional(tag("\"")))
        .map(drop)
}

/// raw string = `r` `#`{n} `"` (plain | not close `"`)* close?, close =
/// `"` `#`{n}
fn raw_string<'a>() -> impl Rule<'a, ()> {
    let hashes = skip_while(|b: u8| b == b'#')
        .recognize()
        .map(|h: Span| h.len());
    let open = tag("r").and(hashes).and(tag("\"")).map(|((_, n), _)| n);
    open.and_then(|n| {
        let close = move || tag("\"").and(ntimes(tag("#"), n));
        let plain = skip_while1(|b: u8| b != b'"', "string text");
        skip_many(plain.or(not(close()).and(byte()).map(drop)))
            .and(optional(close()))
            .map(drop)
    })
}

/// char or lifetime = `'` (escaped | char `'` | ident* `'`?), escaped = `\`
/// char? not (`'` | newline)* byte?: `'\n'`, `'x'`, `'a`, and `'ab'` closed
/// as a literal.
fn char_or_lifetime<'a>() -> impl Rule<'a, Kind> {
    let escaped = tag("\\")
        .and(optional(char()))
        .and(skip_while(|b: u8| !matches!(b, b'\'' | b'\n')))
        .and(optional(byte()))
        .map(|_| Kind::Char);
    let quoted = char().and(tag("'")).map(|_| Kind::Char);
    let lifetime =
        skip_chars_while(is_ident)
            .and(optional(tag("'")))
            .map(|(_, close)| match close {
                Some(_) => Kind::Char,
                None => Kind::Lifetime,
            });
    tag("'")
        .and(escaped.or(quoted).or(lifetime))
        .map(|(_, kind)| kind)
}

/// prefixed literal = (`b` | `c`)? raw string | (`b` | `c`) string | `b`
/// char or lifetime
fn prefixed_literal<'a>() -> impl Rule<'a, Kind> {
    let prefix = || tag("b").or(tag("c"));
    let raw = optional(prefix()).and(raw_string()).map(|_| Kind::Str);
    let string = prefix().and(string()).map(|_| Kind::Str);
    let byte_char = tag("b").and(char_or_lifetime()).map(|_| Kind::Char);
    raw.or(string).or(byte_char)
}

/// punct = `||` | `&&` | `==` | `=>` | `..` | `>>=` | any char
fn punct<'a>() -> impl Rule<'a, Kind> {
    let joined = tag("||")
        .or(tag("&&"))
        .or(tag("=="))
        .or(tag("=>"))
        .or(tag(".."))
        .or(tag(">>="));
    joined.map(drop).or(char().map(drop)).map(|_| Kind::Punct)
}

/// Exactly `s`.
fn tag<'a>(
    s: &'static str,
) -> impl Parser<Cursor = Cur<'a>, Output = Span<'a>, Error = ParsicombError<'a>> {
    is_sequence(s.as_bytes())
}
