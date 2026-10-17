//! Abstract syntax tree produced by the parser.
//!
//! "Abstract" means the tree keeps the *structure* of the program and drops
//! the syntax that only served to show it: there are no parenthesis,
//! newline or `;` nodes, because the nesting of the tree already says what
//! they said. `(1 + 2) * 3` and `(1+2)*3` produce the same tree.
//!
//! The AST is still close to what the user wrote: names are plain strings,
//! nothing has a type yet, and a `Name` might be a local, a parameter or a
//! function. Later stages turn it into more precise trees (HIR, then TIR),
//! each adding what its stage worked out. Keeping one tree type per stage,
//! instead of filling in optional fields of a single tree, lets the Rust type
//! system guarantee that, say, the code generator never sees an unresolved name.

use std::fmt::Write;

use crate::diag::Span;

#[derive(Debug, Clone)]
pub struct Program {
    pub defs: Vec<Def>,
}

/// A top-level definition: `name params [:: signature] = body`.
#[derive(Debug, Clone)]
pub struct Def {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub sig: Option<Signature>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Signature {
    pub params: Vec<TypeAnn>,
    pub ret: Option<TypeAnn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    Numeric,
    Int,
    Float,
    String,
    Boolean,
    Void,
    Any,
}

impl TypeName {
    pub fn parse(s: &str) -> Option<TypeName> {
        Some(match s {
            "numeric" => TypeName::Numeric,
            "int" => TypeName::Int,
            "float" => TypeName::Float,
            "string" => TypeName::String,
            "boolean" => TypeName::Boolean,
            "void" => TypeName::Void,
            "any" => TypeName::Any,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TypeName::Numeric => "numeric",
            TypeName::Int => "int",
            TypeName::Float => "float",
            TypeName::String => "string",
            TypeName::Boolean => "boolean",
            TypeName::Void => "void",
            TypeName::Any => "any",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TypeAnn {
    pub name: TypeName,
    pub span: Span,
}

/// A function body: a sequence of statements; the last one is the result.
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// A local binding inside a function body: `name = value`.
    Let {
        name: String,
        span: Span,
        value: Expr,
    },
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }

    pub fn is_arith(self) -> bool {
        matches!(self, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
    }
}

/// Every expression carries its span so that any later error about it
/// ("cannot add string and int") can underline exactly this expression.
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    /// A bare name: a local, a parameter, or a zero-argument function call.
    Name(String),
    /// A function application `f a b c` with at least one argument.
    Call {
        callee: String,
        callee_span: Span,
        args: Vec<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Neg(Box<Expr>),
    /// `echo` is built into the language, not a library function, because
    /// it accepts a value of *any* type and prints each type differently.
    /// The compiler picks the right printing routine from the argument's type.
    Echo(Box<Expr>),
    Check {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
        else_arm: Option<Box<Expr>>,
    },
}

#[derive(Debug, Clone)]
pub struct Arm {
    pub pattern: Expr,
    pub body: Expr,
}

// ---------------------------------------------------------------------------
// Pretty printing (`--emit=ast`)
// ---------------------------------------------------------------------------
//
// The tree is printed as Lisp-style S-expressions, `(+ 1 (* 2 3))`, which
// show the nesting unambiguously. That makes it easy to check how
// precedence was resolved, and the parser tests use it for exactly that.

pub fn dump(program: &Program) -> String {
    let mut out = String::new();
    for def in &program.defs {
        let params: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
        write!(out, "(def {} ({})", def.name, params.join(" ")).unwrap();
        if let Some(sig) = &def.sig {
            let ps: Vec<&str> = sig.params.iter().map(|t| t.name.as_str()).collect();
            write!(out, " :: ({})", ps.join(", ")).unwrap();
            if let Some(r) = &sig.ret {
                write!(out, " -> {}", r.name.as_str()).unwrap();
            }
        }
        out.push('\n');
        for stmt in &def.body.stmts {
            out.push_str("  ");
            match stmt {
                Stmt::Let { name, value, .. } => {
                    write!(out, "(let {name} ").unwrap();
                    dump_expr(&mut out, value, 2);
                    out.push(')');
                }
                Stmt::Expr(e) => dump_expr(&mut out, e, 2),
            }
            out.push('\n');
        }
        out.push_str(")\n");
    }
    out
}

fn dump_expr(out: &mut String, e: &Expr, indent: usize) {
    match &e.kind {
        ExprKind::Int(v) => write!(out, "{v}").unwrap(),
        ExprKind::Float(v) => write!(out, "{v:?}").unwrap(),
        ExprKind::Str(s) => write!(out, "{s:?}").unwrap(),
        ExprKind::Bool(b) => write!(out, "{b}").unwrap(),
        ExprKind::Name(n) => out.push_str(n),
        ExprKind::Call { callee, args, .. } => {
            write!(out, "({callee}").unwrap();
            for a in args {
                out.push(' ');
                dump_expr(out, a, indent);
            }
            out.push(')');
        }
        ExprKind::Binary { op, lhs, rhs } => {
            write!(out, "({} ", op.as_str()).unwrap();
            dump_expr(out, lhs, indent);
            out.push(' ');
            dump_expr(out, rhs, indent);
            out.push(')');
        }
        ExprKind::Neg(inner) => {
            out.push_str("(neg ");
            dump_expr(out, inner, indent);
            out.push(')');
        }
        ExprKind::Echo(inner) => {
            out.push_str("(echo ");
            dump_expr(out, inner, indent);
            out.push(')');
        }
        ExprKind::Check {
            scrutinee,
            arms,
            else_arm,
        } => {
            out.push_str("(check ");
            dump_expr(out, scrutinee, indent);
            let pad = " ".repeat(indent + 2);
            for arm in arms {
                write!(out, "\n{pad}(").unwrap();
                dump_expr(out, &arm.pattern, indent + 2);
                out.push_str(" -> ");
                dump_expr(out, &arm.body, indent + 4);
                out.push(')');
            }
            if let Some(e) = else_arm {
                write!(out, "\n{pad}(else -> ").unwrap();
                dump_expr(out, e, indent + 4);
                out.push(')');
            }
            out.push(')');
        }
    }
}
