//! Lexer: turns Luna source text into tokens.
//!
//! Newlines are significant (they end statements) and are emitted as
//! `Newline` tokens, except inside parentheses where they are skipped.
//! Consecutive newlines and blank/comment-only lines collapse into one token.
//!
//! Why a separate lexing stage? Source text is a flat sequence of characters,
//! but the grammar talks about *words*: numbers, names, operators. Grouping
//! characters into tokens first means the parser never has to think about
//! whitespace, comments, escape sequences or whether `>=` is one operator or
//! two. Each stage solves one problem, which is the main idea of the whole
//! compiler pipeline.
//!
//! Every token remembers its `Span` (byte range in the source). Nothing later
//! needs the text itself, but error messages do: a type error found three
//! stages later can still point at the exact characters that caused it.

use crate::diag::{Diagnostic, ErrorKind, Span};

/// What kind of token, plus its value for literals and names. The lexer
/// already converts `42` to the integer 42 and `"a\n"` to the real string,
/// so later stages work with values instead of re-parsing text.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    // keywords
    Check,
    Else,
    True,
    False,
    Echo,
    Loop,
    In,
    Ret,
    // punctuation
    Assign,     // =
    EqEq,       // ==
    NotEq,      // !=
    Lt,         // <
    Gt,         // >
    Le,         // <=
    Ge,         // >=
    Plus,       // +
    PlusPlus,   // ++
    StarStar,   // **
    BangBang,   // !!
    DotDot,     // ..
    Bar,        // |
    Minus,      // -
    Star,       // *
    Slash,      // /
    LParen,     // (
    RParen,     // )
    LBracket,   // [
    RBracket,   // ]
    ColonColon, // ::
    Colon,      // :
    Arrow,      // ->
    Comma,      // ,
    Semi,       // ;
    At,         // @
    Newline,
    Eof,
}

impl TokenKind {
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Int(v) => format!("number `{v}`"),
            TokenKind::Float(v) => format!("number `{v}`"),
            TokenKind::Str(_) => "string literal".to_string(),
            TokenKind::Ident(n) => format!("identifier `{n}`"),
            TokenKind::Check => "`check`".into(),
            TokenKind::Else => "`else`".into(),
            TokenKind::True => "`true`".into(),
            TokenKind::False => "`false`".into(),
            TokenKind::Echo => "`echo`".into(),
            TokenKind::Loop => "`loop`".into(),
            TokenKind::In => "`in`".into(),
            TokenKind::Ret => "`ret`".into(),
            TokenKind::Assign => "`=`".into(),
            TokenKind::EqEq => "`==`".into(),
            TokenKind::NotEq => "`!=`".into(),
            TokenKind::Lt => "`<`".into(),
            TokenKind::Gt => "`>`".into(),
            TokenKind::Le => "`<=`".into(),
            TokenKind::Ge => "`>=`".into(),
            TokenKind::Plus => "`+`".into(),
            TokenKind::PlusPlus => "`++`".into(),
            TokenKind::StarStar => "`**`".into(),
            TokenKind::BangBang => "`!!`".into(),
            TokenKind::DotDot => "`..`".into(),
            TokenKind::Bar => "`|`".into(),
            TokenKind::Minus => "`-`".into(),
            TokenKind::Star => "`*`".into(),
            TokenKind::Slash => "`/`".into(),
            TokenKind::LParen => "`(`".into(),
            TokenKind::RParen => "`)`".into(),
            TokenKind::LBracket => "`[`".into(),
            TokenKind::RBracket => "`]`".into(),
            TokenKind::ColonColon => "`::`".into(),
            TokenKind::Colon => "`:`".into(),
            TokenKind::Arrow => "`->`".into(),
            TokenKind::Comma => "`,`".into(),
            TokenKind::Semi => "`;`".into(),
            TokenKind::At => "`@`".into(),
            TokenKind::Newline => "end of line".into(),
            TokenKind::Eof => "end of file".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// The token is the first one on its line and starts in column 1.
    /// Luna has no keyword like `def` or `fn`: a top-level definition is
    /// recognized by starting in column 1 (see the parser's layout rules).
    /// The lexer is the last stage that sees columns, so it records this
    /// fact for the parser.
    pub at_col0: bool,
    /// The token is the first one on its line (even inside parentheses,
    /// where line breaks produce no `Newline` token).
    pub line_first: bool,
    /// 0-based column where the token starts. Indentation matters for
    /// `loop`: its body is the following lines indented further than the
    /// `loop` keyword.
    pub col: usize,
}

/// Lex a file whose byte offsets start at `base` (see `diag::Sources`).
pub fn lex_at(src: &str, base: usize) -> Result<Vec<Token>, Diagnostic> {
    let shift = |s: Span| Span::new(s.start + base, s.end + base);
    match lex(src) {
        Ok(mut tokens) => {
            for t in &mut tokens {
                t.span = shift(t.span);
            }
            Ok(tokens)
        }
        Err(mut d) => {
            d.span = d.span.map(shift);
            Err(d)
        }
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    Lexer {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        paren_depth: 0,
        line_start: true,
        line_begin: 0,
        out: Vec::new(),
    }
    .run()
}

/// The lexer is a hand-written state machine: it looks at the current byte,
/// decides which kind of token starts here, and consumes it. Real compilers
/// almost always lex by hand like this rather than with generator tools,
/// because it is simple, fast, and gives full control over error messages.
struct Lexer<'a> {
    // Scanning bytes is fast and every Luna token is ASCII; `text` is the
    // same source as `&str`, used to slice out names and decode UTF-8
    // characters inside string literals.
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    /// How many `(` are open. Inside parentheses newlines are ignored, so a
    /// long expression can be split across lines: `foo (1 +\n 2)`.
    paren_depth: usize,
    /// No token has been produced on the current line yet.
    line_start: bool,
    /// Byte offset where the current line begins.
    line_begin: usize,
    out: Vec<Token>,
}

impl<'a> Lexer<'a> {
    /// The current byte, or 0 at the end of input. Returning a sentinel
    /// instead of an `Option` keeps every check a simple comparison: 0 never
    /// matches a digit, letter or operator, so loops stop by themselves.
    fn peek(&self) -> u8 {
        self.src.get(self.pos).copied().unwrap_or(0)
    }

    fn peek2(&self) -> u8 {
        self.src.get(self.pos + 1).copied().unwrap_or(0)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let line_first = self.line_start;
        let at_col0 = line_first && (start == 0 || self.src[start - 1] == b'\n');
        self.line_start = false;
        let col = self.text[self.line_begin..start].chars().count();
        self.out.push(Token {
            kind,
            span: Span::new(start, self.pos),
            at_col0,
            line_first,
            col,
        });
    }

    fn run(mut self) -> Result<Vec<Token>, Diagnostic> {
        while self.pos < self.src.len() {
            let start = self.pos;
            let c = self.peek();
            match c {
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'#' => {
                    while self.pos < self.src.len() && self.peek() != b'\n' {
                        self.pos += 1;
                    }
                }
                b'\n' => {
                    self.pos += 1;
                    // Blank lines and comment-only lines would otherwise
                    // produce several Newline tokens in a row. Collapsing
                    // them here means the parser can treat "one Newline" as
                    // "end of statement" without special cases.
                    let last_is_newline = matches!(
                        self.out.last(),
                        None | Some(Token {
                            kind: TokenKind::Newline,
                            ..
                        })
                    );
                    if self.paren_depth == 0 && !last_is_newline {
                        self.out.push(Token {
                            kind: TokenKind::Newline,
                            span: Span::new(start, start + 1),
                            at_col0: false,
                            line_first: false,
                            col: start - self.line_begin,
                        });
                    }
                    self.line_start = true;
                    self.line_begin = self.pos;
                }
                b'0'..=b'9' => self.number()?,
                b'"' => self.string()?,
                c if c == b'_' || c.is_ascii_alphabetic() => {
                    while self.peek() == b'_' || self.peek().is_ascii_alphanumeric() {
                        self.pos += 1;
                    }
                    let word = &self.text[start..self.pos];
                    // Keywords are lexed exactly like names and then looked
                    // up in this table. That is why a keyword cannot be used
                    // as a variable name: it never becomes an `Ident`.
                    let kind = match word {
                        "check" => TokenKind::Check,
                        "else" => TokenKind::Else,
                        "true" => TokenKind::True,
                        "false" => TokenKind::False,
                        "echo" => TokenKind::Echo,
                        "loop" => TokenKind::Loop,
                        "in" => TokenKind::In,
                        "ret" => TokenKind::Ret,
                        _ => TokenKind::Ident(word.to_string()),
                    };
                    self.push(kind, start);
                }
                _ => {
                    // "Maximal munch": always take the longest operator that
                    // matches, so `>=` is one token and not `>` then `=`.
                    // That is why the two-character patterns come first.
                    let (kind, len) = match (c, self.peek2()) {
                        (b'=', b'=') => (TokenKind::EqEq, 2),
                        (b'!', b'=') => (TokenKind::NotEq, 2),
                        (b'<', b'=') => (TokenKind::Le, 2),
                        (b'>', b'=') => (TokenKind::Ge, 2),
                        (b':', b':') => (TokenKind::ColonColon, 2),
                        (b'-', b'>') => (TokenKind::Arrow, 2),
                        (b'+', b'+') => (TokenKind::PlusPlus, 2),
                        (b'*', b'*') => (TokenKind::StarStar, 2),
                        (b'!', b'!') => (TokenKind::BangBang, 2),
                        (b'.', b'.') => (TokenKind::DotDot, 2),
                        (b'|', _) => (TokenKind::Bar, 1),
                        (b'=', _) => (TokenKind::Assign, 1),
                        (b'<', _) => (TokenKind::Lt, 1),
                        (b'>', _) => (TokenKind::Gt, 1),
                        (b'+', _) => (TokenKind::Plus, 1),
                        (b'-', _) => (TokenKind::Minus, 1),
                        (b'*', _) => (TokenKind::Star, 1),
                        (b'/', _) => (TokenKind::Slash, 1),
                        (b'(', _) => (TokenKind::LParen, 1),
                        (b')', _) => (TokenKind::RParen, 1),
                        (b'[', _) => (TokenKind::LBracket, 1),
                        (b']', _) => (TokenKind::RBracket, 1),
                        (b':', _) => (TokenKind::Colon, 1),
                        (b',', _) => (TokenKind::Comma, 1),
                        (b';', _) => (TokenKind::Semi, 1),
                        (b'@', _) => (TokenKind::At, 1),
                        _ => {
                            let ch = self.text[start..].chars().next().unwrap();
                            return Err(Diagnostic::error(
                                ErrorKind::SyntaxError,
                                Span::new(start, start + ch.len_utf8()),
                                format!("unexpected character `{ch}`"),
                            ));
                        }
                    };
                    // Inside brackets, as inside parentheses, a line break
                    // does not end the statement.
                    match kind {
                        TokenKind::LParen | TokenKind::LBracket => self.paren_depth += 1,
                        TokenKind::RParen | TokenKind::RBracket => {
                            self.paren_depth = self.paren_depth.saturating_sub(1)
                        }
                        _ => {}
                    }
                    self.pos += len;
                    self.push(kind, start);
                }
            }
        }
        // Guarantee that the token stream always ends with `Newline, Eof`,
        // even if the file has no final newline. The parser can then assume
        // every statement is terminated, and `Eof` lets it look ahead
        // without ever running off the end of the vector.
        if !matches!(
            self.out.last(),
            None | Some(Token {
                kind: TokenKind::Newline,
                ..
            })
        ) {
            self.out.push(Token {
                kind: TokenKind::Newline,
                span: Span::new(self.pos, self.pos),
                at_col0: false,
                line_first: false,
                col: 0,
            });
        }
        // Column 0: `Eof` ends every indented block.
        self.out.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.pos, self.pos),
            at_col0: false,
            line_first: true,
            col: 0,
        });
        Ok(self.out)
    }

    fn number(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let mut is_float = false;
        while self.peek().is_ascii_digit() {
            self.pos += 1;
        }
        // Require a digit after the dot, so `5.` is not a float. Checking two
        // bytes ahead ("lookahead") lets the lexer decide without backtracking.
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
        }
        // An exponent (`1e5`, `2.5E-3`) needs digits after the `e`. If there
        // are none, rewind to `save`: then `2e` lexes as the number `2`
        // followed by the name `e`, instead of an error.
        if matches!(self.peek(), b'e' | b'E') {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), b'+' | b'-') {
                self.pos += 1;
            }
            if self.peek().is_ascii_digit() {
                is_float = true;
                while self.peek().is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let text = &self.text[start..self.pos];
        let span = Span::new(start, self.pos);
        let kind =
            if is_float {
                TokenKind::Float(text.parse().map_err(|_| {
                    Diagnostic::error(ErrorKind::SyntaxError, span, "invalid number")
                })?)
            } else {
                TokenKind::Int(text.parse().map_err(|_| {
                    Diagnostic::error(
                        ErrorKind::SyntaxError,
                        span,
                        "integer literal is too large for a 64-bit int",
                    )
                })?)
            };
        self.push(kind, start);
        Ok(())
    }

    fn string(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        self.pos += 1;
        // The token stores the *decoded* string (escapes already replaced),
        // which is exactly the bytes the program will hold at run time.
        let mut value = String::new();
        loop {
            // Strings may not span lines. Otherwise a missing `"` would
            // swallow the rest of the file, and the error would show up far
            // from the real mistake.
            if self.pos >= self.src.len() || self.peek() == b'\n' {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    Span::new(start, self.pos),
                    "unterminated string literal",
                ));
            }
            let ch = self.text[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
            match ch {
                '"' => break,
                '\\' => {
                    let esc_start = self.pos - 1;
                    let e = self.text[self.pos..].chars().next().unwrap_or('\0');
                    self.pos += e.len_utf8();
                    value.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        '\\' => '\\',
                        '"' => '"',
                        _ => {
                            return Err(Diagnostic::error(
                                ErrorKind::SyntaxError,
                                Span::new(esc_start, self.pos),
                                format!("unknown escape sequence `\\{e}`"),
                            ));
                        }
                    });
                }
                c => value.push(c),
            }
        }
        self.push(TokenKind::Str(value), start);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn basic_tokens() {
        use TokenKind::*;
        assert_eq!(
            kinds("foo = 10 + 8 * 7 # comment\n"),
            vec![
                Ident("foo".into()),
                Assign,
                Int(10),
                Plus,
                Int(8),
                Star,
                Int(7),
                Newline,
                Eof
            ]
        );
        assert_eq!(
            kinds("sum a b :: numeric -> void = 1.5"),
            vec![
                Ident("sum".into()),
                Ident("a".into()),
                Ident("b".into()),
                ColonColon,
                Ident("numeric".into()),
                Arrow,
                Ident("void".into()),
                Assign,
                Float(1.5),
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn newlines_collapse_and_parens_skip() {
        use TokenKind::*;
        assert_eq!(
            kinds("a\n\n# c\n\nb"),
            vec![Ident("a".into()), Newline, Ident("b".into()), Newline, Eof]
        );
        assert_eq!(
            kinds("(a\nb)"),
            vec![
                LParen,
                Ident("a".into()),
                Ident("b".into()),
                RParen,
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn column_zero_flag() {
        let toks = lex("a = 1\n  b\nc").unwrap();
        let flags: Vec<bool> = toks
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Ident(_)))
            .map(|t| t.at_col0)
            .collect();
        assert_eq!(flags, vec![true, false, true]);
    }

    #[test]
    fn strings_and_comparisons() {
        use TokenKind::*;
        assert_eq!(
            kinds(r#"echo "a\"b\n" <= >= == != < >"#),
            vec![
                Echo,
                Str("a\"b\n".into()),
                Le,
                Ge,
                EqEq,
                NotEq,
                Lt,
                Gt,
                Newline,
                Eof
            ]
        );
    }
}
