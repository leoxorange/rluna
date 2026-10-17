//! Lexer: turns Luna source text into tokens.
//!
//! Newlines are significant (they end statements) and are emitted as
//! `Newline` tokens, except inside parentheses where they are skipped.
//! Consecutive newlines and blank/comment-only lines collapse into one token.

use crate::diag::{Diagnostic, ErrorKind, Span};

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
    // punctuation
    Assign,     // =
    EqEq,       // ==
    NotEq,      // !=
    Lt,         // <
    Gt,         // >
    Le,         // <=
    Ge,         // >=
    Plus,       // +
    Minus,      // -
    Star,       // *
    Slash,      // /
    LParen,     // (
    RParen,     // )
    ColonColon, // ::
    Colon,      // :
    Arrow,      // ->
    Comma,      // ,
    Semi,       // ;
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
            TokenKind::Assign => "`=`".into(),
            TokenKind::EqEq => "`==`".into(),
            TokenKind::NotEq => "`!=`".into(),
            TokenKind::Lt => "`<`".into(),
            TokenKind::Gt => "`>`".into(),
            TokenKind::Le => "`<=`".into(),
            TokenKind::Ge => "`>=`".into(),
            TokenKind::Plus => "`+`".into(),
            TokenKind::Minus => "`-`".into(),
            TokenKind::Star => "`*`".into(),
            TokenKind::Slash => "`/`".into(),
            TokenKind::LParen => "`(`".into(),
            TokenKind::RParen => "`)`".into(),
            TokenKind::ColonColon => "`::`".into(),
            TokenKind::Colon => "`:`".into(),
            TokenKind::Arrow => "`->`".into(),
            TokenKind::Comma => "`,`".into(),
            TokenKind::Semi => "`;`".into(),
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
    pub at_col0: bool,
}

pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    Lexer {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        paren_depth: 0,
        line_start: true,
        out: Vec::new(),
    }
    .run()
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    paren_depth: usize,
    /// No token has been produced on the current line yet.
    line_start: bool,
    out: Vec<Token>,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> u8 {
        self.src.get(self.pos).copied().unwrap_or(0)
    }

    fn peek2(&self) -> u8 {
        self.src.get(self.pos + 1).copied().unwrap_or(0)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let at_col0 = self.line_start && (start == 0 || self.src[start - 1] == b'\n');
        self.line_start = false;
        self.out.push(Token {
            kind,
            span: Span::new(start, self.pos),
            at_col0,
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
                        });
                    }
                    self.line_start = true;
                }
                b'0'..=b'9' => self.number()?,
                b'"' => self.string()?,
                c if c == b'_' || c.is_ascii_alphabetic() => {
                    while self.peek() == b'_' || self.peek().is_ascii_alphanumeric() {
                        self.pos += 1;
                    }
                    let word = &self.text[start..self.pos];
                    let kind = match word {
                        "check" => TokenKind::Check,
                        "else" => TokenKind::Else,
                        "true" => TokenKind::True,
                        "false" => TokenKind::False,
                        "echo" => TokenKind::Echo,
                        _ => TokenKind::Ident(word.to_string()),
                    };
                    self.push(kind, start);
                }
                _ => {
                    let (kind, len) = match (c, self.peek2()) {
                        (b'=', b'=') => (TokenKind::EqEq, 2),
                        (b'!', b'=') => (TokenKind::NotEq, 2),
                        (b'<', b'=') => (TokenKind::Le, 2),
                        (b'>', b'=') => (TokenKind::Ge, 2),
                        (b':', b':') => (TokenKind::ColonColon, 2),
                        (b'-', b'>') => (TokenKind::Arrow, 2),
                        (b'=', _) => (TokenKind::Assign, 1),
                        (b'<', _) => (TokenKind::Lt, 1),
                        (b'>', _) => (TokenKind::Gt, 1),
                        (b'+', _) => (TokenKind::Plus, 1),
                        (b'-', _) => (TokenKind::Minus, 1),
                        (b'*', _) => (TokenKind::Star, 1),
                        (b'/', _) => (TokenKind::Slash, 1),
                        (b'(', _) => (TokenKind::LParen, 1),
                        (b')', _) => (TokenKind::RParen, 1),
                        (b':', _) => (TokenKind::Colon, 1),
                        (b',', _) => (TokenKind::Comma, 1),
                        (b';', _) => (TokenKind::Semi, 1),
                        _ => {
                            let ch = self.text[start..].chars().next().unwrap();
                            return Err(Diagnostic::error(
                                ErrorKind::SyntaxError,
                                Span::new(start, start + ch.len_utf8()),
                                format!("unexpected character `{ch}`"),
                            ));
                        }
                    };
                    match kind {
                        TokenKind::LParen => self.paren_depth += 1,
                        TokenKind::RParen => self.paren_depth = self.paren_depth.saturating_sub(1),
                        _ => {}
                    }
                    self.pos += len;
                    self.push(kind, start);
                }
            }
        }
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
            });
        }
        self.out.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.pos, self.pos),
            at_col0: false,
        });
        Ok(self.out)
    }

    fn number(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let mut is_float = false;
        while self.peek().is_ascii_digit() {
            self.pos += 1;
        }
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            while self.peek().is_ascii_digit() {
                self.pos += 1;
            }
        }
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
        let mut value = String::new();
        loop {
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
