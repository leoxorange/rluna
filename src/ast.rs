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
    /// The files this one links, from the `| ...` lines at its top.
    pub links: Vec<Link>,
    pub trails: Vec<Trail>,
    pub defs: Vec<Def>,
}

/// `trail Fruit follow Copy:` and its indented fields.
#[derive(Debug, Clone)]
pub struct Trail {
    pub name: String,
    pub name_span: Span,
    /// The trails it follows.
    pub follows: Vec<(String, Span)>,
    pub fields: Vec<Field>,
    /// Its functions: `describe n = ...`, or `compare @other :: Self ->
    /// Int` without a body, for the trails that follow it to give.
    pub methods: Vec<Method>,
    pub span: Span,
    /// Which file it is in (see `Def::file`).
    pub file: usize,
    /// It comes from the standard library, which declares the embedded
    /// trails (`Show`, `Compare`, ...) and which trails the embedded values
    /// follow (`trail Int follow Numeric, Copy:`).
    pub library: bool,
}

/// A function of a trail. It is called on a value (`t.describe 1`), whose
/// fields and functions it sees by name.
#[derive(Debug, Clone)]
pub struct Method {
    pub name: String,
    pub span: Span,
    pub params: Vec<Param>,
    pub sig: Option<Signature>,
    /// `None`: the trails that follow this one give it.
    pub body: Option<Block>,
}

/// A field of a trail: `name :: String`, `peel :: Boolean = false`, or
/// `textualize = copy name`. A field without a default must be given when
/// a value is made.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub span: Span,
    pub ty: Option<TypeAnn>,
    pub default: Option<Expr>,
}

/// `| entity/try > try` links the file `entity/try.luna`: its definitions
/// are used as `try.name`. `| entity#pred, next > p, n` links only `pred`
/// and `next`, used without a prefix (as `p` and `n`).
#[derive(Debug, Clone)]
pub struct Link {
    /// The path below the entry file's folder, without `.luna`:
    /// `["entity", "try"]`.
    pub path: Vec<String>,
    pub path_span: Span,
    /// `#pred, next`: only these definitions.
    pub names: Option<Vec<(String, Span)>>,
    /// After `>`: the name of the file (for a whole file), or one new name
    /// per definition (for `#...`). Empty when there is none.
    pub aliases: Vec<(String, Span)>,
    pub span: Span,
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
    /// Which file it is in, when a program has several (see `driver`): an
    /// index into the per-file name tables given to `hir::resolve_files`.
    pub file: usize,
}

#[derive(Debug, Clone)]
pub struct Param {
    /// The parameter's name; for a pattern, its text (`(x:xs)`, `[]`).
    pub name: String,
    pub span: Span,
    pub pattern: Option<Pattern>,
    /// `@x`: the function only borrows the argument, so the caller keeps
    /// it. Without `@` the argument moves into the function.
    pub borrowed: bool,
}

/// A parameter that takes an array apart.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// `(x:xs)`, `(x:y:xs)`: an array with at least `heads.len()` elements;
    /// the first ones are named `heads`, the remaining array `rest`.
    Cons {
        heads: Vec<(String, Span)>,
        rest: (String, Span),
    },
    /// `[]`: an empty array.
    Empty,
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
    Array,
    Iterable,
    Any,
}

impl TypeName {
    /// An embedded trail, by its name: `Int`, or `int` as Luna wrote it
    /// before trails.
    pub fn parse(s: &str) -> Option<TypeName> {
        Some(match s {
            "Numeric" | "numeric" => TypeName::Numeric,
            "Int" | "int" => TypeName::Int,
            "Float" | "float" => TypeName::Float,
            "String" | "string" => TypeName::String,
            "Boolean" | "boolean" => TypeName::Boolean,
            "Void" | "void" => TypeName::Void,
            "Array" | "array" => TypeName::Array,
            "Iterable" | "iterable" => TypeName::Iterable,
            "Any" | "any" => TypeName::Any,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TypeName::Numeric => "Numeric",
            TypeName::Int => "Int",
            TypeName::Float => "Float",
            TypeName::String => "String",
            TypeName::Boolean => "Boolean",
            TypeName::Void => "Void",
            TypeName::Array => "Array",
            TypeName::Iterable => "Iterable",
            TypeName::Any => "Any",
        }
    }
}

/// A trail written in a signature or a field: a name (`Int`, `Fruit`,
/// `food.Fruit`), or an array of something (`[Fruit]`).
#[derive(Debug, Clone)]
pub struct TypeAnn {
    pub kind: TypeAnnKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeAnnKind {
    Name(String),
    Array(Box<TypeAnn>),
}

impl std::fmt::Display for TypeAnn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            TypeAnnKind::Name(n) => f.write_str(n),
            TypeAnnKind::Array(inner) => write!(f, "[{inner}]"),
        }
    }
}

/// A function body: a sequence of statements; the last one is the result.
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// A binding inside a function body: `name = value`. It gives a new
    /// variable its value, or a new value to a variable that exists already.
    Let {
        name: String,
        span: Span,
        value: Expr,
    },
    Expr(Expr),
    /// `loop var in source:` followed by the indented body. `source` is a
    /// range `[a..b]` (var = a, a + 1, ..., b - 1), an array or a string.
    Loop {
        /// `loop i, x in source`: the position of each element.
        pos: Option<(String, Span)>,
        var: String,
        var_span: Span,
        source: Expr,
        /// `in @source`: the loop only borrows the source, and `var` is a
        /// borrowed element. Without `@` the source moves into the loop.
        borrowed: bool,
        body: Vec<Stmt>,
    },
}

/// One clause of a list comprehension, after the `|`.
#[derive(Debug, Clone)]
pub enum Clause {
    /// `x in source`, or `i, x in source` with the position `i`. Each
    /// generator runs once for every element of the generators before it.
    Gen {
        pos: Option<(String, Span)>,
        var: String,
        var_span: Span,
        source: Expr,
        /// `in @source`, as for `loop`.
        borrowed: bool,
    },
    /// A condition: only elements for which it is true are kept.
    Guard(Expr),
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
    /// `a ++ b`: the elements of `a`, then those of `b`.
    Concat,
    /// `a !! i`: the element of `a` at position `i`.
    Index,
    /// `a ** b`: the products of the elements of `a` and `b`, pair by pair;
    /// the longer array's extra elements are kept.
    ElemMul,
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
            BinOp::Concat => "++",
            BinOp::Index => "!!",
            BinOp::ElemMul => "**",
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
    /// `void`: the one "nothing" value.
    Void,
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
    /// `ret value` / `ret`: leave the function now with `value` (or void).
    Ret(Option<Box<Expr>>),
    /// `[a, b, c]`.
    Array(Vec<Expr>),
    /// `[start..end]`: the ints start, start + 1, ..., end - 1.
    Range(Box<Expr>, Box<Expr>),
    /// `[body | x in a, y in b, guard, ...]`: `body` for each combination
    /// of elements that passes the guards. The first clause is a generator.
    Comprehension {
        body: Box<Expr>,
        clauses: Vec<Clause>,
    },
    /// `{name = "kiwi", peel = true}`: a value of a trail. Written after
    /// the trail's name (`Fruit {...}`, which is a call in the syntax), or
    /// alone where the trail is known.
    Record(Vec<(String, Span, Expr)>),
    /// `(buy 6).name`: a field of a value. (`t.name` is a `Name`, since a
    /// name with dots can also mean a linked file's definition.)
    Field(Box<Expr>, String, Span),
    /// `(buy 6).describe 1`: a function of a value's trail, called on it.
    /// (`t.describe 1` is a `Call` of `t.describe`.)
    Method {
        receiver: Box<Expr>,
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
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
    for link in &program.links {
        write!(out, "(link {}", link.path.join("/")).unwrap();
        if let Some(names) = &link.names {
            let names: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
            write!(out, " #{}", names.join(",")).unwrap();
        }
        if !link.aliases.is_empty() {
            let aliases: Vec<&str> = link.aliases.iter().map(|(n, _)| n.as_str()).collect();
            write!(out, " > {}", aliases.join(",")).unwrap();
        }
        out.push_str(")\n");
    }
    for trail in &program.trails {
        write!(out, "(trail {}", trail.name).unwrap();
        for (f, _) in &trail.follows {
            write!(out, " follow {f}").unwrap();
        }
        for m in &trail.methods {
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{}{}", if p.borrowed { "@" } else { "" }, p.name))
                .collect();
            write!(out, "\n  (fn {} ({})", m.name, params.join(" ")).unwrap();
            if let Some(body) = &m.body {
                out.push('\n');
                dump_stmts(&mut out, &body.stmts, 4);
            }
            out.push(')');
        }
        for field in &trail.fields {
            write!(out, "\n  ({}", field.name).unwrap();
            if let Some(t) = &field.ty {
                write!(out, " :: {t}").unwrap();
            }
            if let Some(d) = &field.default {
                out.push_str(" = ");
                dump_expr(&mut out, d, 4);
            }
            out.push(')');
        }
        out.push_str(")\n");
    }
    for def in &program.defs {
        let params: Vec<String> = def
            .params
            .iter()
            .map(|p| format!("{}{}", if p.borrowed { "@" } else { "" }, p.name))
            .collect();
        write!(out, "(def {} ({})", def.name, params.join(" ")).unwrap();
        if let Some(sig) = &def.sig {
            let ps: Vec<String> = sig.params.iter().map(|t| t.to_string()).collect();
            write!(out, " :: ({})", ps.join(", ")).unwrap();
            if let Some(r) = &sig.ret {
                write!(out, " -> {r}").unwrap();
            }
        }
        out.push('\n');
        dump_stmts(&mut out, &def.body.stmts, 2);
        out.push_str(")\n");
    }
    out
}

fn dump_stmts(out: &mut String, stmts: &[Stmt], indent: usize) {
    for stmt in stmts {
        out.push_str(&" ".repeat(indent));
        match stmt {
            Stmt::Let { name, value, .. } => {
                write!(out, "(let {name} ").unwrap();
                dump_expr(out, value, indent);
                out.push(')');
            }
            Stmt::Expr(e) => dump_expr(out, e, indent),
            Stmt::Loop {
                var,
                source,
                borrowed,
                body,
                ..
            } => {
                write!(out, "(loop {var} {}", if *borrowed { "@" } else { "" }).unwrap();
                dump_expr(out, source, indent);
                out.push('\n');
                dump_stmts(out, body, indent + 2);
                out.push_str(&" ".repeat(indent));
                out.push(')');
            }
        }
        out.push('\n');
    }
}

fn dump_expr(out: &mut String, e: &Expr, indent: usize) {
    match &e.kind {
        ExprKind::Int(v) => write!(out, "{v}").unwrap(),
        ExprKind::Float(v) => write!(out, "{v:?}").unwrap(),
        ExprKind::Str(s) => write!(out, "{s:?}").unwrap(),
        ExprKind::Bool(b) => write!(out, "{b}").unwrap(),
        ExprKind::Void => out.push_str("void"),
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
        ExprKind::Ret(value) => {
            out.push_str("(ret");
            if let Some(v) = value {
                out.push(' ');
                dump_expr(out, v, indent);
            }
            out.push(')');
        }
        ExprKind::Array(items) => {
            out.push_str("(array");
            for item in items {
                out.push(' ');
                dump_expr(out, item, indent);
            }
            out.push(')');
        }
        ExprKind::Range(start, end) => {
            out.push_str("(range ");
            dump_expr(out, start, indent);
            out.push(' ');
            dump_expr(out, end, indent);
            out.push(')');
        }
        ExprKind::Comprehension { body, clauses } => {
            out.push_str("(for ");
            dump_expr(out, body, indent);
            for c in clauses {
                match c {
                    Clause::Gen {
                        pos,
                        var,
                        source,
                        borrowed,
                        ..
                    } => {
                        out.push_str(" (in ");
                        if let Some((p, _)) = pos {
                            write!(out, "{p} ").unwrap();
                        }
                        write!(out, "{var} {}", if *borrowed { "@" } else { "" }).unwrap();
                        dump_expr(out, source, indent);
                    }
                    Clause::Guard(g) => {
                        out.push_str(" (if ");
                        dump_expr(out, g, indent);
                    }
                }
                out.push(')');
            }
            out.push(')');
        }
        ExprKind::Echo(inner) => {
            out.push_str("(echo ");
            dump_expr(out, inner, indent);
            out.push(')');
        }
        ExprKind::Record(fields) => {
            out.push('{');
            for (k, (name, _, value)) in fields.iter().enumerate() {
                if k > 0 {
                    out.push_str(", ");
                }
                write!(out, "{name} = ").unwrap();
                dump_expr(out, value, indent);
            }
            out.push('}');
        }
        ExprKind::Field(base, name, _) => {
            out.push_str("(. ");
            dump_expr(out, base, indent);
            write!(out, " {name})").unwrap();
        }
        ExprKind::Method {
            receiver,
            name,
            args,
            ..
        } => {
            out.push_str("(.call ");
            dump_expr(out, receiver, indent);
            write!(out, " {name}").unwrap();
            for a in args {
                out.push(' ');
                dump_expr(out, a, indent);
            }
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

// ---------------------------------------------------------------------------
// Renaming global references
// ---------------------------------------------------------------------------

/// Point the references in `def` to the global functions in `renames` at
/// their new names. Used to keep the standard library calling its own
/// functions when a program defines functions with the same names. A
/// variable of the same name in `def` keeps its name: only calls, and bare
/// names that are not variables there, are renamed.
pub fn rename_globals(def: &mut Def, renames: &std::collections::HashMap<String, String>) {
    let mut variables: Vec<String> = Vec::new();
    for p in &def.params {
        match &p.pattern {
            None => variables.push(p.name.clone()),
            Some(Pattern::Cons { heads, rest }) => {
                variables.extend(heads.iter().chain([rest]).map(|(n, _)| n.clone()))
            }
            Some(Pattern::Empty) => {}
        }
    }
    collect_variables(&def.body.stmts, &mut variables);
    for stmt in &mut def.body.stmts {
        rename_stmt(stmt, renames, &variables);
    }
}

fn collect_variables(stmts: &[Stmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, .. } => out.push(name.clone()),
            Stmt::Loop { pos, var, body, .. } => {
                out.extend(pos.iter().map(|(p, _)| p.clone()));
                out.push(var.clone());
                collect_variables(body, out);
            }
            Stmt::Expr(_) => {}
        }
    }
}

fn rename_stmt(
    stmt: &mut Stmt,
    renames: &std::collections::HashMap<String, String>,
    vars: &[String],
) {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Expr(value) => rename_expr(value, renames, vars),
        Stmt::Loop { source, body, .. } => {
            rename_expr(source, renames, vars);
            for s in body {
                rename_stmt(s, renames, vars);
            }
        }
    }
}

fn rename_expr(e: &mut Expr, renames: &std::collections::HashMap<String, String>, vars: &[String]) {
    let go = |e: &mut Expr| rename_expr(e, renames, vars);
    match &mut e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Void => {}
        ExprKind::Name(n) => {
            if !vars.contains(n)
                && let Some(new) = renames.get(n)
            {
                *n = new.clone();
            }
        }
        ExprKind::Call { callee, args, .. } => {
            if let Some(new) = renames.get(callee) {
                *callee = new.clone();
            }
            args.iter_mut().for_each(go);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            go(lhs);
            go(rhs);
        }
        ExprKind::Range(a, b) => {
            go(a);
            go(b);
        }
        ExprKind::Neg(x) | ExprKind::Echo(x) | ExprKind::Field(x, _, _) => go(x),
        ExprKind::Record(fields) => fields.iter_mut().for_each(|(_, _, v)| go(v)),
        ExprKind::Method { receiver, args, .. } => {
            go(receiver);
            args.iter_mut().for_each(go);
        }
        ExprKind::Ret(x) => x.iter_mut().for_each(|x| go(x)),
        ExprKind::Array(items) => items.iter_mut().for_each(go),
        ExprKind::Comprehension { body, clauses } => {
            go(body);
            for c in clauses {
                match c {
                    Clause::Gen { source, .. } => go(source),
                    Clause::Guard(g) => go(g),
                }
            }
        }
        ExprKind::Check {
            scrutinee,
            arms,
            else_arm,
        } => {
            go(scrutinee);
            for arm in arms {
                go(&mut arm.pattern);
                go(&mut arm.body);
            }
            if let Some(e) = else_arm {
                go(e);
            }
        }
    }
}
