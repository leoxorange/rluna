//! Diagnostics: source positions and error rendering.
//!
//! Every stage reports problems as `Diagnostic` values instead of printing
//! them. Stages stay free of I/O, tests can inspect errors directly, and
//! all messages are rendered the same way: with file, line, column and the
//! offending code underlined.

use std::fmt;

/// A byte range in the source file.
///
/// Positions are stored as byte offsets rather than (line, column) pairs:
/// offsets are small, cheap to combine (see `to`), and line and column
/// numbers are only needed when an error is actually shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    /// The smallest span covering both. Parsers use this to build the span
    /// of a node from its first and last parts: `a + b` spans `a` to `b`.
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    SyntaxError,
    SymbolNotFoundError,
    TypeError,
    DefinitionError,
    OwnershipError,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::SymbolNotFoundError => "SymbolNotFoundError",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::DefinitionError => "DefinitionError",
            ErrorKind::OwnershipError => "OwnershipError",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub kind: Option<ErrorKind>,
    pub message: String,
    pub span: Option<Span>,
    /// Secondary locations that explain the error, e.g. where a value was
    /// moved before the use being reported.
    pub notes: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn error(kind: ErrorKind, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            kind: Some(kind),
            message: message.into(),
            span: Some(span),
            notes: Vec::new(),
        }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            kind: None,
            message: message.into(),
            span: Some(span),
            notes: Vec::new(),
        }
    }

    pub fn with_note(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.notes.push((span, message.into()));
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// A source file with a precomputed line table, used to render diagnostics.
pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> SourceFile {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceFile {
            name: name.into(),
            text,
            line_starts,
        }
    }

    /// 1-based (line, column) of a byte offset.
    ///
    /// `line_starts` is sorted, so a binary search finds the line in
    /// O(log n). `Err(l)` means the offset falls between two line starts,
    /// so it belongs to the line before. Columns count characters, not
    /// bytes, so the caret lines up under non-ASCII text too.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let col = self.text[self.line_starts[line]..offset].chars().count();
        (line + 1, col + 1)
    }

    fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn render(&self, d: &Diagnostic) -> String {
        let head = match (d.severity, d.kind) {
            (Severity::Error, Some(k)) => format!("error[{k}]"),
            (Severity::Error, None) => "error".to_string(),
            (Severity::Warning, _) => "warning".to_string(),
        };
        let Some(span) = d.span else {
            return format!("{head}: {}\n", d.message);
        };
        let mut out = format!("{head}: {}\n{}", d.message, self.snippet(span));
        for (span, note) in &d.notes {
            out.push_str(&format!("note: {note}\n{}", self.snippet(*span)));
        }
        out
    }

    /// The location line and the source line with the span underlined:
    ///
    /// ```text
    ///  --> main.luna:3:9
    ///   |
    /// 3 |     y = x
    ///   |         ^
    /// ```
    fn snippet(&self, span: Span) -> String {
        let (line, col) = self.line_col(span.start.min(self.text.len()));
        let text = self.line_text(line);
        let width = if span.end > span.start {
            let (end_line, end_col) = self.line_col(span.end.min(self.text.len()));
            if end_line == line {
                (end_col - col).max(1)
            } else {
                text.chars().count().saturating_sub(col - 1).max(1)
            }
        } else {
            1
        };
        let gutter = line.to_string().len();
        format!(
            "{pad} --> {file}:{line}:{col}\n{pad} |\n{line} | {text}\n{pad} | {sp}{carets}\n",
            pad = " ".repeat(gutter),
            file = self.name,
            sp = " ".repeat(col - 1),
            carets = "^".repeat(width),
        )
    }
}
