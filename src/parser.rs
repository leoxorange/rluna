//! Parser: tokens -> AST.
//!
//! Layout rules:
//! - A top-level definition starts at column 1 with `name params... =` or
//!   `name params... ::`. Its body runs until the next definition starts
//!   (or end of file); the body's last statement is its result.
//! - Inside a body, an *indented* line of the form `name = expr` is a local
//!   binding. Every other line is an expression statement.
//! - `check e:` and `loop x in [a..b]:` are followed by their body on the
//!   next lines, indented further than the line they start on: the arms of
//!   a `check` (one per line, separated by `;`), the statements of a loop.
//!   These are the only places where the amount of indentation matters.
//!
//! Expression precedence, loosest to tightest:
//!   check  <  comparison, :?  <  ++  <  + -  <  * / **  <  unary -  <  !!  <  application  <  atoms
//!
//! `echo` is applied like a function of one argument: `echo x`, `echo (a + b)`.
//!
//! How it works: this is a *recursive descent* parser. Each grammar rule is
//! one function (`program`, `def`, `expr`, `comparison`, ...), and rules
//! call each other the same way the grammar nests. Operator precedence
//! comes from the order of those calls: `additive` calls `multiplicative`
//! for its operands, so `*` binds tighter than `+`. In `1 + 2 * 3`,
//! `multiplicative` has already consumed `2 * 3` before `additive` sees the
//! `+`. Each level is one line in the precedence table above.
//!
//! The parser only checks *shape*: whether the tokens form a valid program.
//! Whether names exist or types match is checked by later stages, each of
//! which can then assume the earlier checks already passed.

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::lexer::{Token, TokenKind};

pub fn parse(tokens: Vec<Token>) -> Result<Program, Diagnostic> {
    Parser {
        toks: tokens,
        pos: 0,
        lines: false,
    }
    .program()
}

/// The parser walks the token vector left to right with a cursor (`pos`).
/// It never goes back: every decision is made by looking at the current
/// token, or a few tokens ahead with `peek_at`.
struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// The innermost thing being parsed is a block (of a `loop` or an
    /// `if`), not brackets: each of its lines is a statement, so a token
    /// that starts a line ends the expression before it. Outside brackets
    /// the newline token does that already; inside them (`echo (if c:`
    /// ... `)`) there are no newline tokens, and only this says so.
    lines: bool,
}

type PResult<T> = Result<T, Diagnostic>;

impl Parser {
    fn peek(&self) -> &TokenKind {
        &self.toks[self.pos].kind
    }

    /// The expression being parsed ends here: the current token starts
    /// another line of a block (see `lines`).
    fn line_ends(&self) -> bool {
        self.lines && self.toks[self.pos].line_first
    }

    /// Parse what is inside brackets, where an expression may go on over
    /// several lines.
    fn bracketed<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let lines = std::mem::replace(&mut self.lines, false);
        let r = f(self);
        self.lines = lines;
        r
    }

    fn peek_at(&self, n: usize) -> &TokenKind {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].kind
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }

    /// Consume the current token. The cursor never moves past the final
    /// `Eof`, so `peek()` is always valid and the parser cannot index out
    /// of bounds, even on malformed input.
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> PResult<Token> {
        if self.peek() == kind {
            Ok(self.bump())
        } else {
            Err(self.unexpected(what))
        }
    }

    fn unexpected(&self, expected: &str) -> Diagnostic {
        Diagnostic::error(
            ErrorKind::SyntaxError,
            self.span(),
            format!("expected {expected}, found {}", self.peek().describe()),
        )
    }

    fn skip_newlines(&mut self) {
        while self.peek() == &TokenKind::Newline {
            self.bump();
        }
    }

    /// Does a top-level definition start at the current token?
    ///
    /// Luna has no `def` keyword and no braces, so this check is what ends a
    /// function body: a line in column 1 shaped like `name params... =` or
    /// `name params... ::`. The scan skips over the parameter names to find
    /// the `=`/`::`. Without that lookahead, a call such as `summon 1` in
    /// column 1 could not be told apart from a definition.
    fn at_def_start(&self) -> bool {
        let tok = &self.toks[self.pos];
        if !tok.at_col0 || !matches!(tok.kind, TokenKind::Ident(_)) {
            return false;
        }
        // Skip the parameters: names, `@` names, `(x:xs)` patterns and `[]`.
        let mut i = 1;
        loop {
            match self.peek_at(i) {
                TokenKind::Ident(_) => i += 1,
                // (`param` reports an `@` before anything but a name)
                TokenKind::At => i += 1,
                TokenKind::LBracket if self.peek_at(i + 1) == &TokenKind::RBracket => i += 2,
                TokenKind::LParen => {
                    let mut k = i + 1;
                    while matches!(self.peek_at(k), TokenKind::Ident(_) | TokenKind::Colon) {
                        k += 1;
                    }
                    if self.peek_at(k) != &TokenKind::RParen {
                        return false;
                    }
                    i = k + 1;
                }
                _ => break,
            }
        }
        matches!(self.peek_at(i), TokenKind::Assign | TokenKind::ColonColon)
    }

    fn program(&mut self) -> PResult<Program> {
        let mut defs = Vec::new();
        let mut links = Vec::new();
        let mut trails = Vec::new();
        self.skip_newlines();
        while self.at_link() {
            links.push(self.link()?);
            self.skip_newlines();
        }
        while self.peek() != &TokenKind::Eof {
            if self.at_link() {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    self.span(),
                    "links (`| file`) go at the top of the file, before the definitions",
                ));
            }
            if self.at_trail() {
                trails.push(self.trail()?);
                self.skip_newlines();
                continue;
            }
            if self.at_native() {
                defs.push(self.native()?);
                self.skip_newlines();
                continue;
            }
            if !self.at_def_start() {
                let msg = if self.toks[self.pos].at_col0 {
                    "expected a definition (`name args = body`); top-level code must live inside a function such as `main`"
                } else {
                    "expected a definition starting in column 1"
                };
                return Err(Diagnostic::error(ErrorKind::SyntaxError, self.span(), msg));
            }
            defs.push(self.def()?);
            self.skip_newlines();
        }
        Ok(Program {
            links,
            trails,
            defs,
        })
    }

    /// A line starting with `native` in column 1.
    fn at_native(&self) -> bool {
        let tok = &self.toks[self.pos];
        tok.at_col0 && tok.kind == TokenKind::Native
    }

    /// `native name params :: signature`: a function the runtime gives,
    /// so it has a signature and no body.
    fn native(&mut self) -> PResult<Def> {
        let start = self.bump().span;
        if !matches!(self.peek(), TokenKind::Ident(_)) {
            return Err(self.unexpected("the name of the native function"));
        }
        let mut def = self.def_after_name(true)?;
        def.span = start.to(def.span);
        Ok(def)
    }

    /// A line starting with `trail` in column 1.
    fn at_trail(&self) -> bool {
        let tok = &self.toks[self.pos];
        tok.at_col0 && tok.kind == TokenKind::Trail
    }

    /// `trail Name [follow A, B]:` and its fields, one per line, indented:
    /// `name :: T`, `name :: T = default` or `name = default`.
    fn trail(&mut self) -> PResult<Trail> {
        let start = self.bump().span;
        let (name, name_span) = match self.peek().clone() {
            TokenKind::Ident(n) => (n, self.bump().span),
            _ => return Err(self.unexpected("the name of the trail")),
        };
        plain(&name, name_span)?;
        let mut follows = Vec::new();
        if matches!(self.peek(), TokenKind::Ident(w) if w == "follow") {
            self.bump();
            loop {
                match self.peek().clone() {
                    TokenKind::Ident(n) => follows.push((n, self.bump().span)),
                    _ => return Err(self.unexpected("the name of a trail to follow")),
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::Colon, "`:` after the trail's name")?;
        if !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof) {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "the fields of a trail go on the following lines, indented",
            ));
        }
        let mut fields: Vec<Field> = Vec::new();
        let mut methods: Vec<Method> = Vec::new();
        loop {
            self.skip_newlines();
            let tok = &self.toks[self.pos];
            if tok.kind == TokenKind::Eof || tok.col == 0 {
                break;
            }
            let col = tok.col;
            let (fname, span) = match self.peek().clone() {
                TokenKind::Ident(n) => (n, self.bump().span),
                _ => return Err(self.unexpected("a field of the trail (`name :: String`)")),
            };
            plain(&fname, span)?;
            // With parameters, it is a function of the trail; a function
            // may have several definitions, tried top to bottom.
            let is_function = matches!(
                self.peek(),
                TokenKind::Ident(_) | TokenKind::At | TokenKind::LParen | TokenKind::LBracket
            );
            if fields.iter().any(|f| f.name == fname)
                || (!is_function && methods.iter().any(|m| m.name == fname))
            {
                return Err(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    span,
                    format!("`{fname}` is declared more than once in this trail"),
                ));
            }
            if is_function {
                methods.push(self.method(fname, span, col)?);
                continue;
            }
            let ty = if self.eat(&TokenKind::ColonColon) {
                Some(self.type_ann()?)
            } else {
                None
            };
            let default = if self.eat(&TokenKind::Assign) {
                Some(self.expr()?)
            } else {
                None
            };
            if ty.is_none() && default.is_none() {
                return Err(self.unexpected("`::` and the field's trail, or `=` and its value"));
            }
            // (A default with a block, an `if`, ends where the block ends.)
            if !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof)
                && !self.toks[self.pos].line_first
            {
                return Err(self.unexpected("end of line after the field"));
            }
            fields.push(Field {
                name: fname,
                span,
                ty,
                default,
            });
        }
        Ok(Trail {
            name,
            name_span,
            follows,
            fields,
            methods,
            span: start.to(self.prev_span()),
            file: 0,
            library: false,
        })
    }

    /// A function of a trail, after its name: `p1 p2 [:: sig] [= body]`.
    /// The body is the rest of the line, or the lines below indented
    /// further than the name; without one, the trails that follow give it.
    fn method(&mut self, name: String, span: Span, col: usize) -> PResult<Method> {
        let mut params = Vec::new();
        while let Some(p) = self.param()? {
            if params.iter().any(|q: &Param| q.name == p.name) {
                return Err(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    p.span,
                    format!("parameter `{}` is declared more than once", p.name),
                ));
            }
            params.push(p);
        }
        let sig = if self.eat(&TokenKind::ColonColon) {
            Some(self.signature(params.len())?)
        } else {
            None
        };
        let body = if self.eat(&TokenKind::Assign) {
            let stmts = if matches!(self.peek(), TokenKind::Newline) {
                self.stmts(Some(col))?
            } else {
                let e = self.expr()?;
                // (A body with a block, an `if`, ends where the block ends.)
                if !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof)
                    && !self.toks[self.pos].line_first
                {
                    return Err(self.unexpected("end of line after the function"));
                }
                vec![Stmt::Expr(e)]
            };
            if stmts.is_empty() {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    span,
                    "this function has an empty body",
                ));
            }
            Some(Block { stmts })
        } else {
            if sig.is_none() {
                return Err(
                    self.unexpected("`::` and the function's signature, or `=` and its body")
                );
            }
            if !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof) {
                return Err(self.unexpected("end of line after the function"));
            }
            None
        };
        Ok(Method {
            name,
            span,
            params,
            sig,
            body,
        })
    }

    /// A line starting with `|` in column 1.
    fn at_link(&self) -> bool {
        let tok = &self.toks[self.pos];
        tok.at_col0 && tok.kind == TokenKind::Bar
    }

    /// `| path [#name, ...] [> alias, ...]`, alone on its line. The path is
    /// names separated by `/`.
    fn link(&mut self) -> PResult<Link> {
        let start = self.bump().span;
        let name = |this: &mut Self, what: &str| -> PResult<(String, Span)> {
            match this.peek().clone() {
                TokenKind::Ident(n) if !n.contains('.') => Ok((n, this.bump().span)),
                _ => Err(this.unexpected(what)),
            }
        };
        let list = |this: &mut Self, what: &str| -> PResult<Vec<(String, Span)>> {
            let mut items = vec![name(this, what)?];
            while this.eat(&TokenKind::Comma) {
                items.push(name(this, what)?);
            }
            Ok(items)
        };
        let first = name(
            self,
            "the path of a file to link (`| entity` or `| entity/try`)",
        )?;
        let mut path_span = first.1;
        let mut path = vec![first.0];
        while self.eat(&TokenKind::Slash) {
            let (part, span) = name(self, "a folder or file name after `/`")?;
            path.push(part);
            path_span = path_span.to(span);
        }
        let names = if self.eat(&TokenKind::Hash) {
            Some(list(self, "the name of a definition to link")?)
        } else {
            None
        };
        let aliases = if self.eat(&TokenKind::Gt) && matches!(self.peek(), TokenKind::Ident(_)) {
            list(self, "a name")?
        } else {
            Vec::new()
        };
        let span = start.to(self.prev_span());
        if !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof) {
            return Err(self.unexpected("end of line after the link"));
        }
        match &names {
            None if aliases.len() > 1 => {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    aliases[1].1,
                    "a whole file is linked under one name, like `| entity > et`",
                ));
            }
            Some(names) if !aliases.is_empty() && aliases.len() != names.len() => {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    span,
                    format!(
                        "{} definition(s) are linked but {} new name(s) are given; the names after `>` go with the definitions pair by pair",
                        names.len(),
                        aliases.len()
                    ),
                ));
            }
            _ => {}
        }
        Ok(Link {
            path,
            path_span,
            names,
            aliases,
            span,
        })
    }

    fn def(&mut self) -> PResult<Def> {
        self.def_after_name(false)
    }

    /// A definition from its name on; a `native` one ends after its
    /// signature.
    fn def_after_name(&mut self, native: bool) -> PResult<Def> {
        let start = self.span();
        let TokenKind::Ident(name) = self.bump().kind else {
            unreachable!()
        };
        plain(&name, start)?;
        let mut params: Vec<Param> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        while let Some(param) = self.param()? {
            let declared: Vec<(&String, Span)> = match &param.pattern {
                None => vec![(&param.name, param.span)],
                Some(Pattern::Cons { heads, rest }) => {
                    heads.iter().chain([rest]).map(|(n, s)| (n, *s)).collect()
                }
                Some(Pattern::Empty) => vec![],
            };
            for (name, span) in declared {
                if names.contains(name) {
                    return Err(Diagnostic::error(
                        ErrorKind::DefinitionError,
                        span,
                        format!("parameter `{name}` is declared more than once"),
                    ));
                }
                names.push(name.clone());
            }
            params.push(param);
        }
        let sig = if self.eat(&TokenKind::ColonColon) {
            Some(self.signature(params.len())?)
        } else {
            None
        };
        let body = if native {
            if sig.is_none() {
                return Err(self.unexpected("`::` and the native function's signature"));
            }
            if !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof) {
                return Err(self.unexpected(
                    "end of line: a native function has only a signature, the runtime gives its body",
                ));
            }
            Block { stmts: Vec::new() }
        } else {
            self.expect(&TokenKind::Assign, "`=`")?;
            self.body()?
        };
        Ok(Def {
            name,
            name_span: start,
            params,
            sig,
            body,
            span: start.to(self.prev_span()),
            file: 0,
            native,
        })
    }

    /// A parameter: a name, `@name` (borrowed), `(x:xs)` / `(x:y:xs)`, or
    /// `[]`.
    fn param(&mut self) -> PResult<Option<Param>> {
        let start = self.span();
        match self.peek().clone() {
            TokenKind::At => {
                self.bump();
                let TokenKind::Ident(name) = self.peek().clone() else {
                    return Err(Diagnostic::error(
                        ErrorKind::SyntaxError,
                        start.to(self.span()),
                        "`@` borrows a parameter given by name, like `@a`; a pattern such as `(x:xs)` takes its array apart, so it must own it",
                    ));
                };
                self.bump();
                plain(&name, self.prev_span())?;
                Ok(Some(Param {
                    name,
                    span: start.to(self.prev_span()),
                    pattern: None,
                    borrowed: true,
                }))
            }
            TokenKind::Ident(name) => {
                self.bump();
                plain(&name, start)?;
                Ok(Some(Param {
                    name,
                    span: start,
                    pattern: None,
                    borrowed: false,
                }))
            }
            TokenKind::LBracket => {
                self.bump();
                self.expect(&TokenKind::RBracket, "`]` (the pattern `[]`)")?;
                Ok(Some(Param {
                    name: "[]".into(),
                    span: start.to(self.prev_span()),
                    pattern: Some(Pattern::Empty),
                    borrowed: false,
                }))
            }
            TokenKind::LParen => {
                self.bump();
                let mut names = Vec::new();
                loop {
                    let TokenKind::Ident(n) = self.peek().clone() else {
                        return Err(self.unexpected("a name in the pattern (`(x:xs)`)"));
                    };
                    names.push((n, self.bump().span));
                    if !self.eat(&TokenKind::Colon) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`:` or `)` in the pattern")?;
                let span = start.to(self.prev_span());
                if names.len() < 2 {
                    return Err(Diagnostic::error(
                        ErrorKind::SyntaxError,
                        span,
                        "a pattern names the first element and the rest, like `(x:xs)`",
                    ));
                }
                let rest = names.pop().unwrap();
                let text: Vec<&str> = names
                    .iter()
                    .chain([&rest])
                    .map(|(n, _)| n.as_str())
                    .collect();
                Ok(Some(Param {
                    name: format!("({})", text.join(":")),
                    span,
                    pattern: Some(Pattern::Cons { heads: names, rest }),
                    borrowed: false,
                }))
            }
            _ => Ok(None),
        }
    }

    /// A trail in a signature or a field: `Int`, `Fruit`, `food.Fruit`,
    /// `[T]` for an array of `T`, and `T?` for `T` or void.
    fn type_ann(&mut self) -> PResult<TypeAnn> {
        let ann = self.plain_type_ann()?;
        if !self.eat(&TokenKind::Question) {
            return Ok(ann);
        }
        let span = ann.span.to(self.prev_span());
        Ok(TypeAnn {
            kind: TypeAnnKind::Maybe(Box::new(ann)),
            span,
        })
    }

    fn plain_type_ann(&mut self) -> PResult<TypeAnn> {
        let span = self.span();
        match self.peek().clone() {
            TokenKind::Ident(n) => {
                self.bump();
                Ok(TypeAnn {
                    kind: TypeAnnKind::Name(n),
                    span,
                })
            }
            TokenKind::LBracket => {
                self.bump();
                let inner = self.type_ann()?;
                self.expect(&TokenKind::RBracket, "`]` after the trail of the elements")?;
                Ok(TypeAnn {
                    kind: TypeAnnKind::Array(Box::new(inner)),
                    span: span.to(self.prev_span()),
                })
            }
            _ => Err(self.unexpected("a trail (Int, String, Numeric, Any, Fruit, [Int], ...)")),
        }
    }

    /// `:: T1, T2 -> R`. Knowing the number of parameters up front lets the
    /// parser report a mismatched signature here, pointing at the
    /// signature, rather than later as a confusing type error.
    fn signature(&mut self, nparams: usize) -> PResult<Signature> {
        let sig_start = self.span();
        if nparams == 0 {
            // `name :: R = ...` or `name :: -> R = ...`
            self.eat(&TokenKind::Arrow);
            let ret = self.type_ann()?;
            return Ok(Signature {
                params: Vec::new(),
                ret: Some(ret),
            });
        }
        let mut types = vec![self.type_ann()?];
        while self.eat(&TokenKind::Comma) {
            types.push(self.type_ann()?);
        }
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.type_ann()?)
        } else {
            None
        };
        if types.len() != nparams {
            return Err(Diagnostic::error(
                ErrorKind::DefinitionError,
                sig_start.to(self.prev_span()),
                format!(
                    "signature lists {} parameter type(s) but the function has {nparams} parameter(s)",
                    types.len()
                ),
            ));
        }
        Ok(Signature { params: types, ret })
    }

    fn body(&mut self) -> PResult<Block> {
        let stmts = self.stmts(None)?;
        if stmts.is_empty() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.prev_span(),
                "definition has an empty body",
            ));
        }
        Ok(Block { stmts })
    }

    /// Statements, one per line, up to the next definition or the end of the
    /// file. With `deeper_than`, only lines whose first token is indented
    /// further than that column: the body of a `loop`, or a block of an
    /// `if`. Such a block also ends at a `)`, which closes the brackets
    /// an `if` is in (`echo (if c:` ... `)`).
    fn stmts(&mut self, deeper_than: Option<usize>) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        let closes = |kind: &TokenKind| deeper_than.is_some() && *kind == TokenKind::RParen;
        let nested = self.lines || deeper_than.is_some();
        let lines = std::mem::replace(&mut self.lines, nested);
        let r = self.stmts_loop(deeper_than, &mut stmts, closes);
        self.lines = lines;
        r.map(|()| stmts)
    }

    fn stmts_loop(
        &mut self,
        deeper_than: Option<usize>,
        stmts: &mut Vec<Stmt>,
        closes: impl Fn(&TokenKind) -> bool,
    ) -> PResult<()> {
        loop {
            self.skip_newlines();
            let tok = &self.toks[self.pos];
            if tok.kind == TokenKind::Eof
                || self.at_def_start()
                || self.at_link()
                || self.at_trail()
                || self.at_native()
                || deeper_than.is_some_and(|col| tok.col <= col)
                || closes(&tok.kind)
            {
                return Ok(());
            }
            let stmt = self.stmt()?;
            // Every statement ends with its line. One with a block (a
            // `loop`, an `if`) ends where its block ends, at the start of
            // a line.
            if !self.toks[self.pos].line_first
                && !matches!(self.peek(), TokenKind::Newline | TokenKind::Eof)
                && !closes(self.peek())
            {
                return Err(self.unexpected("end of line"));
            }
            stmts.push(stmt);
        }
    }

    /// A statement is a binding `name = expr`, a `loop`, or an expression.
    /// Two tokens of lookahead decide between the first and the last: `x = ...`
    /// is a binding, while `x + 1` or `f x` is an expression.
    fn stmt(&mut self) -> PResult<Stmt> {
        if self.peek() == &TokenKind::Loop {
            return self.loop_stmt();
        }
        if self.peek() == &TokenKind::Else
            && matches!(self.peek_at(1), TokenKind::Colon | TokenKind::If)
        {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "this `else` belongs to no `if`: it goes on its own line, lined up with the line with its `if`",
            ));
        }
        if let (TokenKind::Ident(name), TokenKind::Assign) = (self.peek().clone(), self.peek_at(1))
        {
            let span = self.bump().span;
            plain(&name, span)?;
            self.bump();
            let value = self.expr()?;
            return Ok(Stmt::Let { name, span, value });
        }
        Ok(Stmt::Expr(self.expr()?))
    }

    /// `x` or `i, x` before `in`: the element's name, and the position's.
    #[allow(clippy::type_complexity)]
    fn loop_names(&mut self) -> PResult<(Option<(String, Span)>, String, Span)> {
        let TokenKind::Ident(first) = self.peek().clone() else {
            return Err(self.unexpected("a name for the elements"));
        };
        let first_span = self.bump().span;
        plain(&first, first_span)?;
        if !self.eat(&TokenKind::Comma) {
            return Ok((None, first, first_span));
        }
        let TokenKind::Ident(var) = self.peek().clone() else {
            return Err(self.unexpected("a name for the elements after the position (`i, x`)"));
        };
        let var_span = self.bump().span;
        plain(&var, var_span)?;
        Ok((Some((first, first_span)), var, var_span))
    }

    /// Does a comprehension generator (`x in` or `i, x in`) start here?
    fn at_generator(&self) -> bool {
        matches!(
            (
                self.peek(),
                self.peek_at(1),
                self.peek_at(2),
                self.peek_at(3)
            ),
            (TokenKind::Ident(_), TokenKind::In, _, _)
                | (
                    TokenKind::Ident(_),
                    TokenKind::Comma,
                    TokenKind::Ident(_),
                    TokenKind::In
                )
        )
    }

    /// `loop x in [start..end]:` and the indented lines below it.
    fn loop_stmt(&mut self) -> PResult<Stmt> {
        let loop_tok = self.bump();
        let (pos, var, var_span) = self.loop_names()?;
        self.expect(&TokenKind::In, "`in`")?;
        let borrowed = self.eat(&TokenKind::At);
        let source = self.comparison()?;
        self.expect(&TokenKind::Colon, "`:` after what the loop goes through")?;
        let header = loop_tok.span.to(self.prev_span());
        if self.peek() != &TokenKind::Newline {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "the loop body goes on the following lines, indented further than `loop`",
            ));
        }
        let body = self.stmts(Some(loop_tok.col))?;
        if body.is_empty() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                header,
                "this loop has no body: indent the lines below it further than `loop`",
            ));
        }
        Ok(Stmt::Loop {
            pos,
            var,
            var_span,
            source,
            borrowed,
            body,
        })
    }

    fn expr(&mut self) -> PResult<Expr> {
        match self.peek() {
            TokenKind::Check => self.check(),
            TokenKind::If => self.if_expr(),
            TokenKind::Ret => {
                let start = self.bump().span;
                // `ret` alone (end of line, `;`, `)`...) returns void.
                let value = if matches!(
                    self.peek(),
                    TokenKind::Newline
                        | TokenKind::Eof
                        | TokenKind::Semi
                        | TokenKind::RParen
                        | TokenKind::RBracket
                        | TokenKind::Comma
                ) {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                Ok(Expr {
                    kind: ExprKind::Ret(value),
                    span: start.to(self.prev_span()),
                })
            }
            _ => self.comparison(),
        }
    }

    /// `check e: p1 -> a; p2 -> b; else -> c`. The scrutinee and patterns
    /// are parsed with `comparison`, not `expr`, so that a nested `check`
    /// cannot start in the middle of them. Arm bodies use the full `expr`,
    /// so an arm can itself contain a `check`.
    /// The indentation of the line that token `idx` is on: the column of
    /// the line's first token.
    fn line_indent(&self, idx: usize) -> usize {
        let mut k = idx;
        while k > 0 && !self.toks[k].line_first {
            k -= 1;
        }
        self.toks[k].col
    }

    fn check(&mut self) -> PResult<Expr> {
        let indent = self.line_indent(self.pos);
        let start = self.bump().span;
        let scrutinee = self.comparison()?;
        self.expect(&TokenKind::Colon, "`:` after the checked expression")?;
        let header = start.to(self.prev_span());
        let mut arms = Vec::new();
        let mut else_arm = None;
        // The column of the first arm: every arm is lined up with it.
        let mut arm_col = None;
        loop {
            self.skip_newlines();
            // Every arm starts its own line, indented further than the line
            // with `check`, like the body of a loop.
            let tok = &self.toks[self.pos];
            if !tok.line_first || tok.col <= indent || tok.kind == TokenKind::Eof {
                let (span, msg) = if arm_col.is_none() {
                    (
                        header,
                        "the arms of a `check` go on the following lines, each on its own line, indented further than the line with `check`",
                    )
                } else if tok.line_first && tok.kind != TokenKind::Eof {
                    // The line after the `;` is outside this check: the
                    // arm before it was the last one.
                    let mut k = self.pos - 1;
                    while self.toks[k].kind == TokenKind::Newline {
                        k -= 1;
                    }
                    (
                        self.toks[k].span,
                        "the last arm of a `check` has no `;` after it (a nested `check` ends without one too)",
                    )
                } else {
                    (
                        tok.span,
                        "expected another arm of the `check` after `;`, on its own line, indented further than the line with `check`",
                    )
                };
                return Err(Diagnostic::error(ErrorKind::SyntaxError, span, msg));
            }
            let col = *arm_col.get_or_insert(tok.col);
            if self.peek() == &TokenKind::Else {
                self.bump();
                self.expect(&TokenKind::Arrow, "`->`")?;
                else_arm = Some(Box::new(self.expr()?));
                // A `;` after `else` may separate the arms of an enclosing
                // `check`; it is an error only if another arm of this one
                // follows.
                if self.peek() == &TokenKind::Semi {
                    let mut k = self.pos + 1;
                    while self.toks[k].kind == TokenKind::Newline {
                        k += 1;
                    }
                    if self.toks[k].line_first && self.toks[k].col == col {
                        return Err(Diagnostic::error(
                            ErrorKind::SyntaxError,
                            self.span(),
                            "the `else` branch must be the last branch of a `check`",
                        ));
                    }
                }
                break;
            }
            let pattern = self.comparison()?;
            self.expect(&TokenKind::Arrow, "`->`")?;
            let body = self.expr()?;
            // An arm that ends with a nested `check` needs no `;`: that
            // check's last arm has none, and the next line lined up with
            // this check's arms is another arm of this check.
            let nested = ends_in_check(&body);
            arms.push(Arm { pattern, body });
            if !self.eat(&TokenKind::Semi) {
                let mut k = self.pos;
                while self.toks[k].kind == TokenKind::Newline {
                    k += 1;
                }
                let next = &self.toks[k];
                if next.line_first && next.col == col && next.kind != TokenKind::Eof {
                    if nested {
                        continue;
                    }
                    return Err(Diagnostic::error(
                        ErrorKind::SyntaxError,
                        self.prev_span(),
                        "expected `;` after this arm: the next line is another arm of the `check`",
                    ));
                }
                break;
            }
        }
        Ok(Expr {
            kind: ExprKind::Check {
                scrutinee: Box::new(scrutinee),
                arms,
                else_arm,
            },
            span: start.to(self.prev_span()),
        })
    }

    /// `if cond:` and its block, the lines below indented further than the
    /// line with `if`; then maybe `else:` and its block, with `else` lined
    /// up with that line. `else if cond:` is an `else` whose block is
    /// another `if`.
    fn if_expr(&mut self) -> PResult<Expr> {
        let indent = self.line_indent(self.pos);
        let start = self.bump().span;
        let cond = self.comparison()?;
        self.expect(&TokenKind::Colon, "`:` after the condition of `if`")?;
        let header = start.to(self.prev_span());
        let then = self.if_block(indent, header, "`if`")?;
        let tok = &self.toks[self.pos];
        let has_else = tok.line_first
            && tok.kind == TokenKind::Else
            && tok.col == indent
            && matches!(self.peek_at(1), TokenKind::Colon | TokenKind::If);
        let else_block = if has_else {
            let else_span = self.bump().span;
            if self.peek() == &TokenKind::If {
                Some(vec![Stmt::Expr(self.if_expr()?)])
            } else {
                self.bump(); // `:`
                let header = else_span.to(self.prev_span());
                Some(self.if_block(indent, header, "`else`")?)
            }
        } else {
            None
        };
        Ok(Expr {
            kind: ExprKind::If {
                cond: Box::new(cond),
                then,
                else_block,
            },
            span: start.to(self.prev_span()),
        })
    }

    /// The block after `if cond:` or `else:`: its lines are indented
    /// further than `indent`, the line with `if`.
    fn if_block(&mut self, indent: usize, header: Span, what: &str) -> PResult<Vec<Stmt>> {
        // (In brackets there are no newline tokens: the next token starts
        // a line.)
        if self.peek() != &TokenKind::Newline && !self.toks[self.pos].line_first {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                format!(
                    "the block of {what} goes on the following lines, indented further than the line with `if`"
                ),
            ));
        }
        let block = self.stmts(Some(indent))?;
        if block.is_empty() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                header,
                format!(
                    "{what} has no block: indent the lines below it further than the line with `if`"
                ),
            ));
        }
        Ok(block)
    }

    /// Comparisons are *non-associative*: at most one per level. `a < b < c`
    /// is rejected because it would mean `(a < b) < c`, comparing a boolean
    /// with a number, which is almost never what the programmer meant.
    ///
    /// `value :? Trail` sits on the same level, and takes a trail on its
    /// right, written as in a signature: `x + 1 :? Int` tests `x + 1`.
    fn comparison(&mut self) -> PResult<Expr> {
        let lhs = self.concat()?;
        if self.line_ends() {
            return Ok(lhs);
        }
        if self.eat(&TokenKind::ColonQuestion) {
            let trail = self.type_ann()?;
            if matches!(
                self.peek(),
                TokenKind::ColonQuestion
                    | TokenKind::EqEq
                    | TokenKind::NotEq
                    | TokenKind::Lt
                    | TokenKind::Le
                    | TokenKind::Gt
                    | TokenKind::Ge
            ) {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    self.span(),
                    "`:?` cannot be chained with another comparison; use parentheses",
                ));
            }
            let span = lhs.span.to(trail.span);
            return Ok(Expr {
                kind: ExprKind::Is(Box::new(lhs), trail),
                span,
            });
        }
        let op = match self.peek() {
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::NotEq => BinOp::Ne,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::Le => BinOp::Le,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::Ge => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.bump();
        let rhs = self.concat()?;
        if matches!(
            self.peek(),
            TokenKind::ColonQuestion
                | TokenKind::EqEq
                | TokenKind::NotEq
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge
        ) {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "comparison operators cannot be chained; use parentheses",
            ));
        }
        let span = lhs.span.to(rhs.span);
        Ok(Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span,
        })
    }

    /// `+` and `-` are *left-associative*: `a - b - c` is `(a - b) - c`.
    /// A loop that keeps wrapping `lhs` builds the tree leaning left. A
    /// recursive call for the right-hand side would build `a - (b - c)`,
    /// which gives a different answer.
    /// `a ++ b`: below `+` and `-`, so `a ++ b + c` joins `a` with `b + c`.
    fn concat(&mut self) -> PResult<Expr> {
        let mut lhs = self.additive()?;
        while !self.line_ends() && self.eat(&TokenKind::PlusPlus) {
            let rhs = self.additive()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary {
                    op: BinOp::Concat,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        Ok(lhs)
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.multiplicative()?;
        loop {
            if self.line_ends() {
                return Ok(lhs);
            }
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.multiplicative()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            if self.line_ends() {
                return Ok(lhs);
            }
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::StarStar => BinOp::ElemMul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.unary()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.peek() == &TokenKind::Minus {
            let start = self.bump().span;
            let inner = self.unary()?;
            let span = start.to(inner.span);
            // Fold negative literals directly: `-5` becomes the constant -5
            // rather than "negate 5". This is constant folding, the
            // simplest optimization: work done once at compile time instead
            // of every time the program runs.
            return Ok(match inner.kind {
                ExprKind::Int(v) => Expr {
                    kind: ExprKind::Int(v.wrapping_neg()),
                    span,
                },
                ExprKind::Float(v) => Expr {
                    kind: ExprKind::Float(-v),
                    span,
                },
                _ => Expr {
                    kind: ExprKind::Neg(Box::new(inner)),
                    span,
                },
            });
        }
        self.index()
    }

    /// Can the current token begin an argument? Application has no
    /// punctuation (`f a b`), so the parser keeps taking arguments while the
    /// next token could start one. It stops at an operator, `)`, `;`, a
    /// newline, and so on. `-` is deliberately not in the list: `f -1` would
    /// be ambiguous, so it means `f - 1`, and a negative argument is
    /// written `f (-1)`.
    /// `a !! i`: tighter than every operator but looser than application,
    /// so `f a !! 2` indexes the result of `f a`, and `-a !! 2` negates the
    /// element.
    fn index(&mut self) -> PResult<Expr> {
        let mut lhs = self.application()?;
        while !self.line_ends() && self.eat(&TokenKind::BangBang) {
            let rhs = self.application()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary {
                    op: BinOp::Index,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        Ok(lhs)
    }

    fn starts_atom(&self) -> bool {
        !self.line_ends()
            && matches!(
                self.peek(),
                TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::Ident(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                // not an atom, but `f @x` gets `atom`'s explanation
                | TokenKind::At
            )
    }

    /// `f a b c`: a name followed by atoms. Arguments are parsed with
    /// `atom`, not `expr`, which is what makes application bind tighter
    /// than every operator: in `f a + 1` the `+` is not an atom, so the
    /// arguments end at `a`.
    fn application(&mut self) -> PResult<Expr> {
        if self.peek() == &TokenKind::Echo {
            return self.echo();
        }
        if let TokenKind::Ident(name) = self.peek().clone()
            && name != "void"
        {
            let callee_span = self.bump().span;
            let mut args = Vec::new();
            while self.starts_atom() {
                args.push(self.atom()?);
            }
            if args.is_empty() {
                return Ok(Expr {
                    kind: ExprKind::Name(name),
                    span: callee_span,
                });
            }
            let span = callee_span.to(args.last().unwrap().span);
            return Ok(Expr {
                kind: ExprKind::Call {
                    callee: name,
                    callee_span,
                    args,
                },
                span,
            });
        }
        let atom = self.atom()?;
        if self.starts_atom() {
            // `(buy 6).describe 1`: a function of the value's trail
            if let ExprKind::Field(receiver, name, name_span) = atom.kind {
                let mut args = Vec::new();
                while self.starts_atom() {
                    args.push(self.atom()?);
                }
                let span = atom.span.to(args.last().unwrap().span);
                return Ok(Expr {
                    kind: ExprKind::Method {
                        receiver,
                        name,
                        name_span,
                        args,
                    },
                    span,
                });
            }
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "only named functions can be applied to arguments",
            ));
        }
        Ok(atom)
    }

    /// `echo` takes one argument: an atom, or a function call written
    /// without parentheses (`echo sum xs` is `echo (sum xs)`). An operator
    /// expression goes in parentheses: `echo 5 > 6` is `(echo 5) > 6`.
    fn echo(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        if !self.starts_atom() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "`echo` needs an argument, e.g. `echo x` or `echo (a + b)`",
            ));
        }
        let inner = if matches!(self.peek(), TokenKind::Ident(n) if n != "void") {
            // a name, or a call: `echo sum xs`
            self.application()?
        } else {
            let atom = self.atom()?;
            if self.starts_atom() {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    start.to(self.span()),
                    "`echo` takes one argument; wrap the expression in parentheses, e.g. `echo (f x)`",
                ));
            }
            atom
        };
        let span = start.to(inner.span);
        Ok(Expr {
            kind: ExprKind::Echo(Box::new(inner)),
            span,
        })
    }

    /// `[]`, `[a, b, c]`, `[start..end]` or `[body | var in source, guard...]`.
    fn bracket(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let done = |this: &mut Self, kind| {
            Ok(Expr {
                kind,
                span: start.to(this.prev_span()),
            })
        };
        if self.eat(&TokenKind::RBracket) {
            return done(self, ExprKind::Array(Vec::new()));
        }
        let first = self.expr()?;
        if self.eat(&TokenKind::DotDot) {
            let end = self.expr()?;
            self.expect(&TokenKind::RBracket, "`]` after the range")?;
            return done(self, ExprKind::Range(Box::new(first), Box::new(end)));
        }
        if self.eat(&TokenKind::Bar) {
            // Clauses: generators (`x in a`, `i, x in a`) and conditions,
            // starting with a generator.
            let mut clauses = Vec::new();
            loop {
                if clauses.is_empty() || self.at_generator() {
                    let (pos, var, var_span) = self.loop_names()?;
                    self.expect(&TokenKind::In, "`in`")?;
                    let borrowed = self.eat(&TokenKind::At);
                    clauses.push(Clause::Gen {
                        pos,
                        var,
                        var_span,
                        source: self.expr()?,
                        borrowed,
                    });
                } else {
                    clauses.push(Clause::Guard(self.expr()?));
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket, "`,` or `]` in the list comprehension")?;
            return done(
                self,
                ExprKind::Comprehension {
                    body: Box::new(first),
                    clauses,
                },
            );
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            items.push(self.expr()?);
        }
        self.expect(&TokenKind::RBracket, "`,` or `]` in the array")?;
        done(self, ExprKind::Array(items))
    }

    /// An atom and the fields read from it: `(buy 6).name`.
    fn atom(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        while let TokenKind::FieldName(name) = self.peek().clone() {
            let span = self.bump().span;
            e = Expr {
                span: e.span.to(span),
                kind: ExprKind::Field(Box::new(e), name, span),
            };
        }
        Ok(e)
    }

    /// `{name = "kiwi", peel = true}`: the fields of a trail's value.
    fn record(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let mut fields: Vec<(String, Span, Expr)> = Vec::new();
        if !self.eat(&TokenKind::RBrace) {
            loop {
                let (name, span) = match self.peek().clone() {
                    TokenKind::Ident(n) => (n, self.bump().span),
                    _ => return Err(self.unexpected("a field name (`{name = value}`)")),
                };
                plain(&name, span)?;
                if fields.iter().any(|(n, _, _)| *n == name) {
                    return Err(Diagnostic::error(
                        ErrorKind::DefinitionError,
                        span,
                        format!("the field `{name}` is given more than once"),
                    ));
                }
                self.expect(&TokenKind::Assign, "`=` after the field name")?;
                fields.push((name, span, self.expr()?));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                // a comma before `}` is fine
                if self.peek() == &TokenKind::RBrace {
                    break;
                }
            }
            self.expect(&TokenKind::RBrace, "`,` or `}` after a field")?;
        }
        Ok(Expr {
            kind: ExprKind::Record(fields),
            span: start.to(self.prev_span()),
        })
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            TokenKind::LBrace => return self.bracketed(Self::record),
            TokenKind::Int(v) => ExprKind::Int(v),
            TokenKind::Float(v) => ExprKind::Float(v),
            TokenKind::Str(s) => ExprKind::Str(s),
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::LBracket => return self.bracketed(Self::bracket),
            // `void` is a type name in signatures and the void value here.
            TokenKind::Ident(n) if n == "void" => ExprKind::Void,
            TokenKind::Ident(n) => ExprKind::Name(n),
            TokenKind::LParen => {
                // Parentheses restart at the loosest level (`expr`); this is
                // how they override precedence. They leave no node in the
                // tree: `(1 + 2)` is just the `+` node, with a span widened
                // to cover the parentheses for error messages.
                self.bump();
                let inner = self.bracketed(Self::expr)?;
                self.expect(&TokenKind::RParen, "`)`")?;
                return Ok(Expr {
                    kind: inner.kind,
                    span: span.to(self.prev_span()),
                });
            }
            TokenKind::At => {
                return Err(Diagnostic::error(
                    ErrorKind::SyntaxError,
                    span,
                    "`@` only marks a borrowed parameter (`f @a = ...`) or what a loop or list comprehension borrows (`x in @a`); a call borrows an argument when the function's parameter has `@`",
                ));
            }
            _ => return Err(self.unexpected("an expression")),
        };
        self.bump();
        Ok(Expr { kind, span })
    }
}

/// A name being defined has no `.`: `et.pred` names a definition of
/// another file (see `Link`).
fn plain(name: &str, span: Span) -> PResult<()> {
    if name.contains('.') {
        return Err(Diagnostic::error(
            ErrorKind::SyntaxError,
            span,
            format!(
                "`{name}` names a definition of a linked file; a name defined here cannot contain `.`"
            ),
        ));
    }
    Ok(())
}

/// Whether an arm body ends with a `check` or an `if` of its own (`ret
/// check ...` too), whose arms or blocks are the last lines of the arm.
fn ends_in_check(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Check { .. } | ExprKind::If { .. } => true,
        ExprKind::Ret(Some(value)) => ends_in_check(value),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_src(src: &str) -> Program {
        parse(lex(src).unwrap()).unwrap()
    }

    #[test]
    fn readme_examples_parse() {
        let p = parse_src(
            "b = 10\nsum a b = a + b\nsummon a = check a:\n    1 -> summon (sum a 9);\n    10 -> echo \"summon successfully\";\n    else -> echo \"summon failed\"\n\nmain =\n  summon 1\n  summon 2\n",
        );
        let names: Vec<&str> = p.defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["b", "sum", "summon", "main"]);
        assert_eq!(p.defs[3].body.stmts.len(), 2);
        let ExprKind::Check { arms, else_arm, .. } = &match &p.defs[2].body.stmts[0] {
            Stmt::Expr(e) => e,
            _ => panic!(),
        }
        .kind
        else {
            panic!()
        };
        assert_eq!(arms.len(), 2);
        assert!(else_arm.is_some());
    }

    #[test]
    fn if_else_blocks() {
        // `else if` is an `else` whose block is another `if`; each `else`
        // belongs to the `if` on the line it is lined up with.
        let out = dump(&parse_src(
            "f n = if n < 0:\n    1\nelse if n == 0:\n    if true:\n        2\n    else:\n        3\nelse:\n    x = 4\n    x\n",
        ));
        let want = "  (if (< n 0)\n    1\n  else\n    (if (== n 0)\n      (if true\n        2\n      else\n        3\n      )\n    else\n      (let x 4)\n      x\n    )\n  )";
        assert!(out.contains(want), "{out}");
        // without `else`, the statement after the block is the function's
        let p = parse_src("main =\n    if true:\n        echo 1\n    echo 2\n");
        assert_eq!(p.defs[0].body.stmts.len(), 2);
    }

    #[test]
    fn echo_takes_one_argument() {
        let out = dump(&parse_src("main = echo (5 > 6)"));
        assert!(out.contains("(echo (> 5 6))"), "{out}");
        // Like any application, `echo` binds tighter than operators.
        let out = dump(&parse_src("main = echo 5 > 6"));
        assert!(out.contains("(> (echo 5) 6)"), "{out}");
        // A call needs no parentheses.
        let out = dump(&parse_src("main = echo f 1"));
        assert!(out.contains("(echo (f 1))"), "{out}");
        let err = parse(lex("main = echo 1 2").unwrap()).unwrap_err();
        assert!(
            err.message.contains("wrap the expression"),
            "{}",
            err.message
        );
    }

    #[test]
    fn application_binds_tighter_than_operators() {
        let p = parse_src("main = sum a 9 + 1 * 2");
        let out = dump(&p);
        assert!(out.contains("(+ (sum a 9) (* 1 2))"), "{out}");
    }

    #[test]
    fn is_trail() {
        let p = parse_src("main = x + 1 :? [Int]?");
        let out = dump(&p);
        assert!(out.contains("(:? (+ x 1) [Int]?)"), "{out}");
    }

    #[test]
    fn signatures() {
        let p = parse_src(
            "sum a b :: numeric, numeric -> numeric = a + b + 5\nfoo :: boolean = 5 > 6\n",
        );
        let sig = p.defs[0].sig.as_ref().unwrap();
        assert_eq!(sig.params.len(), 2);
        assert_eq!(sig.ret.as_ref().unwrap().to_string(), "numeric");
        assert_eq!(
            p.defs[1]
                .sig
                .as_ref()
                .unwrap()
                .ret
                .as_ref()
                .unwrap()
                .to_string(),
            "boolean"
        );
    }

    #[test]
    fn native_definitions() {
        let p = parse_src(
            "f = 1\nnative read_line :: String?\nnative print @s :: String -> Void\ng = 2\n",
        );
        let names: Vec<(&str, bool)> = p.defs.iter().map(|d| (d.name.as_str(), d.native)).collect();
        assert_eq!(
            names,
            [
                ("f", false),
                ("read_line", true),
                ("print", true),
                ("g", false)
            ]
        );
        let ret = |i: usize| {
            p.defs[i]
                .sig
                .as_ref()
                .unwrap()
                .ret
                .as_ref()
                .unwrap()
                .to_string()
        };
        assert_eq!(ret(1), "String?");
        assert_eq!(ret(2), "Void");
        assert!(p.defs[1].body.stmts.is_empty());
        assert!(p.defs[2].params[0].borrowed);
        // only a signature: no body, and no definition without one
        for src in ["native f :: Int = 1\n", "native f x\n"] {
            let tokens = crate::lexer::lex(src).unwrap();
            assert!(parse(tokens).is_err(), "{src}");
        }
    }

    #[test]
    fn trails() {
        let p = parse_src(
            "trail Fruit follow Copy:\n    name :: String\n    peel :: Boolean = false\n    label = name + \"!\"\n    box :: [food.Fruit]\n\nbuy n :: Int -> Fruit = {name = \"kiwi\", peel = true}\nmain = echo (Fruit {name = \"a\"}).name\n",
        );
        assert_eq!(p.trails.len(), 1);
        assert_eq!(p.defs.len(), 2);
        let dumped = crate::ast::dump(&p);
        assert!(
            dumped.contains(
                "(trail Fruit follow Copy\n  (name :: String)\n  (peel :: Boolean = false)\n  (label = (+ name \"!\"))\n  (box :: [food.Fruit]))"
            ),
            "{dumped}"
        );
        assert!(
            dumped.contains("{name = \"kiwi\", peel = true}"),
            "{dumped}"
        );
        assert!(
            dumped.contains("(echo (. (Fruit {name = \"a\"}) name))"),
            "{dumped}"
        );
    }

    #[test]
    fn indented_locals() {
        let p = parse_src("main =\n  x = 5\n  echo x\n");
        assert!(matches!(p.defs[0].body.stmts[0], Stmt::Let { .. }));
        assert_eq!(p.defs.len(), 1);
    }

    #[test]
    fn links() {
        let p = parse_src(
            "| entity > et # a comment\n| entity/try\n| entity#pred, next > p, n\n| x#y >\nmain = echo et.pred 5\n",
        );
        let dumped = crate::ast::dump(&p);
        assert!(
            dumped.starts_with(
                "(link entity > et)\n(link entity/try)\n(link entity #pred,next > p,n)\n(link x #y)\n"
            ),
            "{dumped}"
        );
        assert!(dumped.contains("(echo (et.pred 5))"), "{dumped}");
        // `#` starts a comment everywhere else, even right after a name
        let p = parse_src("main = echo a#b\na = 1\n");
        assert_eq!(p.defs.len(), 2);
    }

    #[test]
    fn borrowed_parameters_and_sources() {
        let p = parse_src(
            "total @a n = n\nmain =\n  loop x in @a:\n    echo x\n  echo [y | y in @b, z in c]\n",
        );
        assert_eq!(p.defs.len(), 2);
        let params: Vec<bool> = p.defs[0].params.iter().map(|p| p.borrowed).collect();
        assert_eq!(params, [true, false]);
        let dumped = crate::ast::dump(&p);
        assert!(dumped.contains("(def total (@a n)"), "{dumped}");
        assert!(dumped.contains("(loop x @a"), "{dumped}");
        assert!(dumped.contains("(in y @b) (in z c)"), "{dumped}");
    }
}
