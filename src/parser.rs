//! Parser: tokens -> AST.
//!
//! Layout rules:
//! - A top-level definition starts at column 1 with `name params... =` or
//!   `name params... ::`. Its body runs until the next definition starts
//!   (or end of file); the body's last statement is its result.
//! - Inside a body, an *indented* line of the form `name = expr` is a local
//!   binding. Every other line is an expression statement.
//! - `check e:` arms are separated by `;` and may span lines.
//!
//! Expression precedence, loosest to tightest:
//!   check  <  comparison  <  + -  <  * /  <  unary -  <  application  <  atoms
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
    }
    .program()
}

/// The parser walks the token vector left to right with a cursor (`pos`).
/// It never goes back: every decision is made by looking at the current
/// token, or a few tokens ahead with `peek_at`.
struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

type PResult<T> = Result<T, Diagnostic>;

impl Parser {
    fn peek(&self) -> &TokenKind {
        &self.toks[self.pos].kind
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
        let mut i = 1;
        while matches!(self.peek_at(i), TokenKind::Ident(_)) {
            i += 1;
        }
        matches!(self.peek_at(i), TokenKind::Assign | TokenKind::ColonColon)
    }

    fn program(&mut self) -> PResult<Program> {
        let mut defs = Vec::new();
        self.skip_newlines();
        while self.peek() != &TokenKind::Eof {
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
        Ok(Program { defs })
    }

    fn def(&mut self) -> PResult<Def> {
        let start = self.span();
        let TokenKind::Ident(name) = self.bump().kind else {
            unreachable!()
        };
        let mut params = Vec::new();
        while let TokenKind::Ident(p) = self.peek().clone() {
            let span = self.bump().span;
            if let Some(prev) = params.iter().find(|q: &&Param| q.name == p) {
                let _ = prev;
                return Err(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    span,
                    format!("parameter `{p}` is declared more than once"),
                ));
            }
            params.push(Param { name: p, span });
        }
        let sig = if self.eat(&TokenKind::ColonColon) {
            Some(self.signature(params.len())?)
        } else {
            None
        };
        self.expect(&TokenKind::Assign, "`=`")?;
        let body = self.body()?;
        Ok(Def {
            name,
            name_span: start,
            params,
            sig,
            body,
            span: start.to(self.prev_span()),
        })
    }

    fn type_ann(&mut self) -> PResult<TypeAnn> {
        let span = self.span();
        if let TokenKind::Ident(n) = self.peek()
            && let Some(name) = TypeName::parse(n)
        {
            self.bump();
            return Ok(TypeAnn { name, span });
        }
        Err(self.unexpected("a type (numeric, int, float, string, boolean, void, any)"))
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
        let mut stmts = Vec::new();
        self.skip_newlines();
        while self.peek() != &TokenKind::Eof && !self.at_def_start() {
            stmts.push(self.stmt()?);
            if self.peek() != &TokenKind::Eof {
                if self.peek() != &TokenKind::Newline {
                    return Err(self.unexpected("end of line"));
                }
                self.skip_newlines();
            }
        }
        if stmts.is_empty() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.prev_span(),
                "definition has an empty body",
            ));
        }
        Ok(Block { stmts })
    }

    /// A statement is either a local binding `name = expr` or an expression.
    /// Two tokens of lookahead decide it: `x = ...` is a binding, while
    /// `x + 1` or `f x` is an expression.
    fn stmt(&mut self) -> PResult<Stmt> {
        if let (TokenKind::Ident(name), TokenKind::Assign) = (self.peek().clone(), self.peek_at(1))
        {
            let span = self.bump().span;
            self.bump();
            let value = self.expr()?;
            return Ok(Stmt::Let { name, span, value });
        }
        Ok(Stmt::Expr(self.expr()?))
    }

    fn expr(&mut self) -> PResult<Expr> {
        match self.peek() {
            TokenKind::Check => self.check(),
            _ => self.comparison(),
        }
    }

    /// `check e: p1 -> a; p2 -> b; else -> c`. The scrutinee and patterns
    /// are parsed with `comparison`, not `expr`, so that a nested `check`
    /// cannot start in the middle of them. Arm bodies use the full `expr`,
    /// so an arm can itself contain a `check`.
    fn check(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let scrutinee = self.comparison()?;
        self.expect(&TokenKind::Colon, "`:` after the checked expression")?;
        let mut arms = Vec::new();
        let mut else_arm = None;
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Else {
                self.bump();
                self.expect(&TokenKind::Arrow, "`->`")?;
                else_arm = Some(Box::new(self.expr()?));
                if self.peek() == &TokenKind::Semi {
                    return Err(Diagnostic::error(
                        ErrorKind::SyntaxError,
                        self.span(),
                        "the `else` branch must be the last branch of a `check`",
                    ));
                }
                break;
            }
            let pattern = self.comparison()?;
            self.expect(&TokenKind::Arrow, "`->`")?;
            let body = self.expr()?;
            arms.push(Arm { pattern, body });
            if !self.eat(&TokenKind::Semi) {
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

    /// Comparisons are *non-associative*: at most one per level. `a < b < c`
    /// is rejected because it would mean `(a < b) < c`, comparing a boolean
    /// with a number, which is almost never what the programmer meant.
    fn comparison(&mut self) -> PResult<Expr> {
        let lhs = self.additive()?;
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
        let rhs = self.additive()?;
        if matches!(
            self.peek(),
            TokenKind::EqEq
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
    fn additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.multiplicative()?;
        loop {
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
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
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
        self.application()
    }

    /// Can the current token begin an argument? Application has no
    /// punctuation (`f a b`), so the parser keeps taking arguments while the
    /// next token could start one. It stops at an operator, `)`, `;`, a
    /// newline, and so on. `-` is deliberately not in the list: `f -1` would
    /// be ambiguous, so it means `f - 1`, and a negative argument is
    /// written `f (-1)`.
    fn starts_atom(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::Ident(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::LParen
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
        if let TokenKind::Ident(name) = self.peek().clone() {
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
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "only named functions can be applied to arguments",
            ));
        }
        Ok(atom)
    }

    /// `echo` takes exactly one atom; anything larger goes in parentheses.
    fn echo(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        if !self.starts_atom() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                self.span(),
                "`echo` needs an argument, e.g. `echo x` or `echo (a + b)`",
            ));
        }
        let inner = self.atom()?;
        if self.starts_atom() {
            return Err(Diagnostic::error(
                ErrorKind::SyntaxError,
                start.to(self.span()),
                "`echo` takes one argument; wrap the expression in parentheses, e.g. `echo (f x)`",
            ));
        }
        let span = start.to(inner.span);
        Ok(Expr {
            kind: ExprKind::Echo(Box::new(inner)),
            span,
        })
    }

    fn atom(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Int(v) => ExprKind::Int(v),
            TokenKind::Float(v) => ExprKind::Float(v),
            TokenKind::Str(s) => ExprKind::Str(s),
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::Ident(n) => ExprKind::Name(n),
            TokenKind::LParen => {
                // Parentheses restart at the loosest level (`expr`); this is
                // how they override precedence. They leave no node in the
                // tree: `(1 + 2)` is just the `+` node, with a span widened
                // to cover the parentheses for error messages.
                self.bump();
                let inner = self.expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                return Ok(Expr {
                    kind: inner.kind,
                    span: span.to(self.prev_span()),
                });
            }
            _ => return Err(self.unexpected("an expression")),
        };
        self.bump();
        Ok(Expr { kind, span })
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
            "b = 10\nsum a b = a + b\nsummon a = check a:\n1 -> summon (sum a 9);\n10 -> echo \"summon successfully\";\nelse -> echo \"summon failed\"\n\nmain =\n  summon 1\n  summon 2\n",
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
    fn echo_takes_one_argument() {
        let out = dump(&parse_src("main = echo (5 > 6)"));
        assert!(out.contains("(echo (> 5 6))"), "{out}");
        // Like any application, `echo` binds tighter than operators.
        let out = dump(&parse_src("main = echo 5 > 6"));
        assert!(out.contains("(> (echo 5) 6)"), "{out}");
        let err = parse(lex("main = echo f 1").unwrap()).unwrap_err();
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
    fn signatures() {
        let p = parse_src(
            "sum a b :: numeric, numeric -> numeric = a + b + 5\nfoo :: boolean = 5 > 6\n",
        );
        let sig = p.defs[0].sig.as_ref().unwrap();
        assert_eq!(sig.params.len(), 2);
        assert_eq!(sig.ret.unwrap().name, TypeName::Numeric);
        assert_eq!(
            p.defs[1].sig.as_ref().unwrap().ret.unwrap().name,
            TypeName::Boolean
        );
    }

    #[test]
    fn indented_locals() {
        let p = parse_src("main =\n  x = 5\n  echo x\n");
        assert!(matches!(p.defs[0].body.stmts[0], Stmt::Let { .. }));
        assert_eq!(p.defs.len(), 1);
    }
}
