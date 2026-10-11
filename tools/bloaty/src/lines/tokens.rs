//! The tokenizer: just enough of Rust's lexical grammar (comments, strings,
//! raw strings, char literals vs lifetimes) to tell code from comments,
//! written as parsicomb parsers over the source's bytes. Each rule's doc
//! states its grammar; where a rule is hand-written, its doc says why.

use parsicomb::and::AndExt;
use parsicomb::and_then::AndThenExt;
use parsicomb::atomic::is_sequence;
use parsicomb::byte::byte;
use parsicomb::many::many;
use parsicomb::map::MapExt;
use parsicomb::map_err::MapErrExt;
use parsicomb::not::not;
use parsicomb::or::OrExt;
use parsicomb::position::PositionExt;
use parsicomb::utf8::char::char;
use parsicomb::utf8::{skip_chars_while, skip_chars_while1};
use parsicomb::{BoxedExt, BoxedParser, ByteCursor, Cursor, Parser, ParsicombError};
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
    let token = token();
    // token*, streamed: collecting it with `many` made lexing 2x slower.
    let rest = std::iter::from_fn(move || {
        if cursor.eos() {
            return None;
        }
        let parsed = token.parse(cursor);
        debug_assert!(parsed.is_ok(), "no token at {}", cursor.position());
        let (kind, next) = parsed.ok()?;
        // Not `token().with_position()`: ~10% slower.
        let span = cursor.span_to(next);
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
            start: span.start,
            end: span.end,
            line: first_line,
            last_line: line,
        }
    })
}

type Cur<'a> = ByteCursor<'a>;

/// A rule behind one box, so the dispatcher's arms share a type. It fails
/// only where the whole token does, so wrapping its error (an allocation)
/// costs nothing in practice.
type Rule<'a> = BoxedParser<'a, Cur<'a>, Kind, ParsicombError<'a>>;

fn rule<'a, P>(p: P) -> Rule<'a>
where
    P: Parser<Cursor = Cur<'a>, Output = Kind> + 'a,
    P::Error: 'a,
{
    p.map_err(ParsicombError::wrap).boxed()
}

/// token = space | line comment | block comment | string | char or
/// lifetime | prefixed literal | ident | punct
///
/// Only the alternatives a token's first byte allows are tried, as in
/// rustc's lexer: trying each in turn made tokenizing ~30x slower. Every
/// arm takes at least one character, so `token*` reaches the end.
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
        let rule = match cursor.value()? {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c' => &space_rule,
            b'/' => &slash,
            b'"' => &string,
            b'\'' => &quote,
            b'b' | b'c' | b'r' if LITERAL_PREFIXES.iter().any(literal) => &prefixed,
            b if b.is_ascii_alphanumeric() || b == b'_' => &ident_rule,
            b'|' | b'&' | b'=' | b'.' | b'>' => &joined,
            b if b.is_ascii() => &single,
            _ => &other,
        };
        rule.parse(cursor)
    })
}

/// How prefixed literals (and raw identifiers, `r#type`) start.
const LITERAL_PREFIXES: &[&str] = &[
    "b\"", "b'", "br\"", "br#", "c\"", "cr\"", "cr#", "r\"", "r#",
];

/// shebang = `#!` .. line end, unless spaces then `[` follow the `#!`
fn shebang<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind> {
    let attribute = skip_chars_while(|c| c.is_whitespace() && c != '\n').and(tag("["));
    tag("#!")
        .and(not(attribute))
        .and(skip_while(|b: u8| b != b'\n'))
        .map(|_| Kind::Shebang)
}

/// line comment = `//` doc? .. line end, doc = `/` not `/` | `!`: `///x`
/// and `//!x` are docs, `////` isn't.
fn line_comment<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind> {
    let doc = tag("/").and(not(tag("/"))).map(drop).or(tag("!").map(drop));
    tag("//")
        .and(optional(peek(doc)))
        .and(skip_while(|b: u8| b != b'\n'))
        .map(|((_, doc), _)| Kind::LineComment { doc: doc.is_some() })
}

/// block comment = `/*` doc? body, doc = `*` not (`*` | `/`) | `!`: `/**x`
/// and `/*!x` are docs, `/**/` and `/***` aren't.
fn block_comment<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind> {
    let doc = tag("*")
        .and(not(tag("*").or(tag("/"))))
        .map(drop)
        .or(tag("!").map(drop));
    tag("/*")
        .and(optional(peek(doc)))
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
fn string<'a>() -> impl Parser<Cursor = Cur<'a>, Output = ()> {
    let plain = skip_while1(|b: u8| !matches!(b, b'"' | b'\\'), "string text");
    let escape = tag("\\").and(optional(byte())).map(drop);
    tag("\"")
        .and(skip_many(plain.or(escape)))
        .and(optional(tag("\"")))
        .map(drop)
}

/// raw string = `r` `#`{n} `"` (plain | not close `"`)* close?, close =
/// `"` `#`{n}
fn raw_string<'a>() -> impl Parser<Cursor = Cur<'a>, Output = ()> {
    let hashes = many(tag("#")).map(|h| h.len());
    let open = tag("r").and(hashes).and(tag("\"")).map(|((_, n), _)| n);
    open.and_then(|n| {
        let close = move || tag("\"").and(ntimes(n, tag("#")));
        let plain = skip_while1(|b: u8| b != b'"', "string text");
        skip_many(plain.or(not(close()).and(byte()).map(drop)))
            .and(optional(close()))
            .map(drop)
    })
}

/// char or lifetime = `'` (escaped | char `'` | ident* `'`?), escaped = `\`
/// char? not (`'` | newline)* byte?: `'\n'`, `'x'`, `'a`, and `'ab'` closed
/// as a literal.
fn char_or_lifetime<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind> {
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
fn prefixed_literal<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind> {
    let prefix = || tag("b").or(tag("c"));
    let raw = optional(prefix()).and(raw_string()).map(|_| Kind::Str);
    let string = prefix().and(string()).map(|_| Kind::Str);
    let byte_char = tag("b").and(char_or_lifetime()).map(|_| Kind::Char);
    raw.or(string).or(byte_char)
}

/// punct = `||` | `&&` | `==` | `=>` | `..` | `>>=` | any char
fn punct<'a>() -> impl Parser<Cursor = Cur<'a>, Output = Kind> {
    let joined = tag("||")
        .or(tag("&&"))
        .or(tag("=="))
        .or(tag("=>"))
        .or(tag(".."))
        .or(tag(">>="));
    joined.map(drop).or(char().map(drop)).map(|_| Kind::Punct)
}

/// Exactly `s`.
fn tag<'a>(s: &'static str) -> impl Parser<Cursor = Cur<'a>> {
    is_sequence(s.as_bytes())
}

/// Succeeds where `p` does, consuming nothing.
fn peek<P: Parser>(p: P) -> impl Parser<Cursor = P::Cursor, Output = ()> {
    not(not(p))
}
