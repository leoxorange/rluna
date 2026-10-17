//! Diagnostics: source positions and error rendering.

use std::fmt;

/// A byte range in the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

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
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::SymbolNotFoundError => "SymbolNotFoundError",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::DefinitionError => "DefinitionError",
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
}

impl Diagnostic {
    pub fn error(kind: ErrorKind, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            kind: Some(kind),
            message: message.into(),
            span: Some(span),
        }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            kind: None,
            message: message.into(),
            span: Some(span),
        }
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
            "{head}: {msg}\n{pad} --> {file}:{line}:{col}\n{pad} |\n{line} | {text}\n{pad} | {sp}{carets}\n",
            msg = d.message,
            pad = " ".repeat(gutter),
            file = self.name,
            sp = " ".repeat(col - 1),
            carets = "^".repeat(width),
        )
    }
}
