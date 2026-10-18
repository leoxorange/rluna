//! Name resolution: AST -> HIR.
//!
//! Every name is resolved here, so later passes never deal with scopes.
//! Lookup order is variables (the function's parameters and locals, the
//! innermost scope first), then global definitions; failing both is a
//! `SymbolNotFoundError`. Overloads are grouped by name; picking a specific
//! overload by argument types is left to type checking.
//!
//! Variables: a function's parameters are its first variables (ids 0, 1,
//! ...), followed by its locals. `x = value` gives a value to the variable
//! `x` if one is visible, and otherwise creates it. Each `loop` body is a
//! scope of its own: its loop variable and the variables it creates exist
//! only inside it, while `total = total + i` in the body updates the
//! `total` from outside.
//!
//! What is HIR? A "high-level IR": the same tree shape as the AST, but every
//! name has been replaced by what it refers to. `x` becomes `Local(3)`, and a
//! call names a function group. Strings are replaced by numbers because
//! comparing numbers is cheaper and unambiguous: two different variables can
//! both be called `x` (one in a loop body, one outside it) yet get different
//! ids, so later stages cannot confuse them.
//!
//! Why resolve names before type checking? Scoping rules (shadowing, which
//! `b` is meant) are a separate concern from types. Handling them in one
//! place keeps the type checker simpler, and means "unknown name" errors
//! are reported before any confusing type errors they would cause.

use std::collections::HashMap;

use crate::ast::{self, BinOp, ExprKind, Stmt};
use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::types::Ty;

/// The functions built into the language (chapter 2 of the readme). A
/// program that defines a function with the same name uses its own instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// The number of elements (or characters).
    Len,
    /// The first element, or void if there is none.
    Head,
    /// Everything after the first element, or void if there is none.
    Tail,
    /// Everything before the last element, or void if there is none.
    Init,
    /// The last element, or void if there is none.
    Last,
    /// The elements (or characters) in the opposite order.
    Reverse,
    /// A copy of any value, owned by whoever receives it: the way to keep
    /// something borrowed (`copy a` inside `f @a = ...`, `copy (head a)`).
    Copy,
}

impl Builtin {
    pub fn from_name(name: &str) -> Option<Builtin> {
        Some(match name {
            "len" => Builtin::Len,
            "head" => Builtin::Head,
            "tail" => Builtin::Tail,
            "init" => Builtin::Init,
            "last" => Builtin::Last,
            "reverse" => Builtin::Reverse,
            "copy" => Builtin::Copy,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Len => "len",
            Builtin::Head => "head",
            Builtin::Tail => "tail",
            Builtin::Init => "init",
            Builtin::Last => "last",
            Builtin::Reverse => "reverse",
            Builtin::Copy => "copy",
        }
    }
}

/// The built-in function a global name stands for: its own name, or
/// `builtin.len` from a file where `len` means something else (see
/// `FileNames::hidden`).
fn builtin_of(global: &str) -> Option<Builtin> {
    Builtin::from_name(global.strip_prefix("builtin.").unwrap_or(global))
}

/// How the names written in one file reach the global definitions, in a
/// program made of several files (see `driver`, which builds one table per
/// file). Each file sees its own definitions, the ones it links, and the
/// standard library; the definitions of every file but the main one have
/// global names of their own (`entity/try.pred`), so files never clash.
#[derive(Debug, Default, Clone)]
pub struct FileNames {
    /// The file, for messages (`entity/try.luna`).
    pub file: String,
    /// Names written without a `.` that mean a global of another name: the
    /// file's own definitions, and the definitions it links by name.
    pub names: HashMap<String, String>,
    /// Names of the main file's definitions, which a linked file does not
    /// see: what they mean there instead (the standard library's or a
    /// built-in function of that name), if anything.
    pub hidden: HashMap<String, Option<String>>,
    /// The files linked whole, by the name they are linked as: the file
    /// (for messages) and its definitions (name -> global name).
    pub files: HashMap<String, (String, HashMap<String, String>)>,
}

impl FileNames {
    /// The global name `written` refers to in this file.
    fn global(&self, written: &str) -> Result<String, String> {
        if let Some((alias, member)) = written.split_once('.') {
            let Some((file, defs)) = self.files.get(alias) else {
                return Err(format!(
                    "cannot find symbol `{written}`: no file is linked as `{alias}` in `{}`",
                    self.file
                ));
            };
            return defs.get(member).cloned().ok_or_else(|| {
                format!("cannot find symbol `{written}`: `{file}` has no definition `{member}`")
            });
        }
        if let Some(global) = self.names.get(written) {
            return Ok(global.clone());
        }
        match self.hidden.get(written) {
            Some(Some(global)) => Ok(global.clone()),
            Some(None) => Err(format!(
                "cannot find symbol `{written}` (the main file defines it, but `{}` does not link it)",
                self.file
            )),
            None => Ok(written.to_string()),
        }
    }
}

/// Built-in constants: names with a fixed float value.
fn constant(name: &str) -> Option<f64> {
    match name {
        "PI" => Some(std::f64::consts::PI),
        "infinite" => Some(f64::INFINITY),
        _ => None,
    }
}

pub type FuncId = usize;
pub type LocalId = usize;

#[derive(Debug)]
pub struct Hir {
    pub funcs: Vec<Func>,
    /// Overloads by name, in definition order.
    pub groups: HashMap<String, Vec<FuncId>>,
    pub main: FuncId,
}

#[derive(Debug)]
pub struct Func {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<String>,
    /// Declared parameter types (`any` when unannotated).
    pub param_tys: Vec<Ty>,
    /// Declared return type, if annotated.
    pub ret: Option<Ty>,
    pub body: Vec<HStmt>,
    /// Name of every variable, by id: the parameters first, then the locals.
    pub local_names: Vec<String>,
    /// For each parameter, the pattern that takes its array apart, if any.
    pub patterns: Vec<Option<HPattern>>,
    /// For each parameter, whether it is borrowed (`@a`). All definitions
    /// with the same name and number of parameters agree on this.
    pub borrowed: Vec<bool>,
}

/// A clause of a list comprehension.
#[derive(Debug)]
pub enum HClause {
    Gen {
        pos: Option<LocalId>,
        var: LocalId,
        source: HExpr,
        /// `in @source`: the source is borrowed, not moved.
        borrowed: bool,
    },
    Guard(HExpr),
}

/// A pattern parameter, with the variables it binds.
#[derive(Debug, Clone)]
pub enum HPattern {
    /// `(x:xs)`: `heads` get the first elements, `rest` the remaining array.
    Cons { heads: Vec<LocalId>, rest: LocalId },
    /// `[]`.
    Empty,
}

#[derive(Debug)]
pub enum HStmt {
    /// Give the variable `local` a value (its first one, or a new one).
    Let {
        local: LocalId,
        value: HExpr,
    },
    Expr(HExpr),
    Loop {
        /// `loop i, x in ...`: the position variable, and where it is named.
        pos: Option<(LocalId, Span)>,
        var: LocalId,
        /// Where the loop variable is named, for error messages.
        var_span: Span,
        /// A range `[a..b]`, an array or a string.
        source: HExpr,
        /// `in @source`: the source is borrowed, not moved.
        borrowed: bool,
        body: Vec<HStmt>,
        /// Variables from outside the loop that the body gives new values:
        /// their values carry over from one iteration to the next.
        carried: Vec<LocalId>,
        /// Variables that exist only inside the body (including `var`).
        inner: Vec<LocalId>,
    },
}

#[derive(Debug)]
pub struct HExpr {
    pub kind: HExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum HExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Void,
    Local(LocalId),
    /// Call of a global function group; the overload is chosen by type checking.
    Call {
        name: String,
        args: Vec<HExpr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<HExpr>,
        rhs: Box<HExpr>,
    },
    Neg(Box<HExpr>),
    /// `ret value` / `ret`.
    Ret(Option<Box<HExpr>>),
    /// A call of a built-in function, which takes one argument.
    Builtin(Builtin, Box<HExpr>),
    Array(Vec<HExpr>),
    Range(Box<HExpr>, Box<HExpr>),
    /// `[body | clauses]`; the generators' variables exist only inside.
    Comprehension {
        body: Box<HExpr>,
        clauses: Vec<HClause>,
    },
    Echo(Box<HExpr>),
    Check {
        scrutinee: Box<HExpr>,
        arms: Vec<(HExpr, HExpr)>,
        else_arm: Option<Box<HExpr>>,
    },
}

/// Resolution runs in two passes. Pass 1 collects every definition, so that
/// pass 2 can resolve calls to functions defined *later* in the file (and
/// recursive calls). A single top-to-bottom pass could not resolve
/// `summon` calling `sum` if `sum` came after it.
///
/// Errors are collected rather than returned at the first one, so the user
/// sees every unknown name at once instead of fixing them one per compile.
pub fn resolve(program: &ast::Program) -> Result<(Hir, Vec<Diagnostic>), Vec<Diagnostic>> {
    resolve_files(program, &[])
}

/// `resolve` for a program of several files: `files[def.file]` says how
/// the names in each definition reach the globals.
pub fn resolve_files(
    program: &ast::Program,
    files: &[FileNames],
) -> Result<(Hir, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    // Pass 1. Collect signatures; a later definition with the same name,
    // arity and parameter types replaces the earlier one (this is how the
    // readme's `foo = ...` then `foo = ...` redefinition works).
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut live: Vec<Option<&ast::Def>> = Vec::new();
    for def in &program.defs {
        if constant(&def.name).is_some() {
            errors.push(Diagnostic::error(
                ErrorKind::DefinitionError,
                def.name_span,
                format!(
                    "`{}` is a built-in constant and cannot be redefined",
                    def.name
                ),
            ));
            continue;
        }
        let tys = param_types(def);
        let group = groups.entry(def.name.clone()).or_default();
        if let Some(pos) = group.iter().position(|&i| {
            let other = live[i].unwrap();
            // Definitions with patterns are alternatives, never replacements:
            // `head (x:xs)` and `head []` both take an array.
            other.params.len() == def.params.len()
                && param_types(other) == tys
                && !has_patterns(other)
                && !has_patterns(def)
        }) {
            let old = group[pos];
            warnings.push(Diagnostic::warning(
                def.name_span,
                format!(
                    "`{}` is redefined; this definition replaces the earlier one",
                    def.name
                ),
            ));
            live[old] = None;
            group.remove(pos);
        }
        group.push(live.len());
        live.push(Some(def));
    }

    // Renumber the surviving definitions densely, so a `FuncId` can index
    // straight into `funcs` with no gaps left by replaced definitions.
    let mut ids = vec![usize::MAX; live.len()];
    let mut defs = Vec::new();
    for (i, d) in live.iter().enumerate() {
        if let Some(d) = d {
            ids[i] = defs.len();
            defs.push(*d);
        }
    }
    let groups: HashMap<String, Vec<FuncId>> = groups
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().map(|i| ids[i]).collect()))
        .collect();

    // Calls try definitions from top to bottom (see `typeck`), so a
    // definition is never used when an earlier one with as many parameters
    // accepts everything it does, like `head _` before `head a :: numeric`.
    let mut names: Vec<&String> = groups.keys().collect();
    names.sort_by_key(|n| groups[*n].first().map(|&f| defs[f].name_span.start));
    for group in names.into_iter().map(|n| &groups[n]) {
        for (k, &later) in group.iter().enumerate() {
            let (later_def, later_tys) = (defs[later], param_types(defs[later]));
            // A definition with patterns matches only some arrays, so it
            // never hides a later one.
            let covering = group[..k].iter().find(|&&earlier| {
                !has_patterns(defs[earlier])
                    && defs[earlier].params.len() == later_def.params.len()
                    && later_tys
                        .iter()
                        .zip(param_types(defs[earlier]))
                        .all(|(l, e)| l.is_subset(e))
            });
            if let Some(&earlier) = covering {
                warnings.push(
                    Diagnostic::warning(
                        later_def.name_span,
                        format!(
                            "this definition of `{}` is never used: an earlier one accepts all of its arguments",
                            later_def.name
                        ),
                    )
                    .with_note(defs[earlier].name_span, "calls always match this one first"),
                );
            }
        }
    }

    // A call does not know in advance which definition it reaches (that
    // can depend on the values), yet it must know whether its arguments
    // move or stay with the caller. So every definition with the same name
    // and number of parameters must borrow the same ones.
    for group in groups.values() {
        for (k, &later) in group.iter().enumerate() {
            let flags = |d: &ast::Def| d.params.iter().map(|p| p.borrowed).collect::<Vec<_>>();
            let later_def = defs[later];
            if let Some(&earlier) = group[..k].iter().find(|&&e| {
                defs[e].params.len() == later_def.params.len() && flags(defs[e]) != flags(later_def)
            }) {
                errors.push(
                    Diagnostic::error(
                        ErrorKind::DefinitionError,
                        later_def.name_span,
                        format!(
                            "every definition of `{}` with {} parameter(s) must borrow (`@`) the same parameters, because a call cannot tell in advance which one it reaches",
                            later_def.name,
                            later_def.params.len()
                        ),
                    )
                    .with_note(defs[earlier].name_span, "this definition borrows differently"),
                );
            }
        }
    }

    // Which argument counts each name accepts. A call with a count no
    // overload takes is an error that can be reported now, before types
    // are known.
    let mut arities: HashMap<&str, Vec<usize>> = HashMap::new();
    for def in &defs {
        arities
            .entry(def.name.as_str())
            .or_default()
            .push(def.params.len());
    }
    for a in arities.values_mut() {
        a.sort();
    }

    // Pass 2: resolve every name inside every body.
    let mut funcs = Vec::new();
    for def in &defs {
        // The parameters are the first variables.
        let params: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
        let borrowed: Vec<bool> = def.params.iter().map(|p| p.borrowed).collect();
        let mut r = Resolver {
            arities: &arities,
            names: files.get(def.file),
            locals: params.iter().cloned().zip(0..).collect(),
            local_names: params,
            assigned: Vec::new(),
            fixed: (0..borrowed.len()).filter(|&i| borrowed[i]).collect(),
            errors: Vec::new(),
        };
        // The names a pattern gives the parts of its array are variables too.
        // (The pattern parameter's own name, such as `(x:xs)`, can never be
        // written in the body, so the whole array is only reachable through
        // its parts.)
        let patterns: Vec<Option<HPattern>> = def
            .params
            .iter()
            .map(|p| match &p.pattern {
                None => None,
                Some(ast::Pattern::Empty) => Some(HPattern::Empty),
                Some(ast::Pattern::Cons { heads, rest }) => Some(HPattern::Cons {
                    heads: heads.iter().map(|(n, _)| r.new_local(n)).collect(),
                    rest: r.new_local(&rest.0),
                }),
            })
            .collect();
        let body = r.stmts(&def.body.stmts);
        errors.append(&mut r.errors);
        funcs.push(Func {
            name: def.name.clone(),
            name_span: def.name_span,
            params: def.params.iter().map(|p| p.name.clone()).collect(),
            param_tys: param_types(def),
            ret: def
                .sig
                .as_ref()
                .and_then(|s| s.ret)
                .map(|t| Ty::from_name(t.name)),
            body,
            local_names: r.local_names,
            patterns,
            borrowed,
        });
    }

    // A function whose definitions take arrays apart with `(x:xs)` but have
    // nothing for an empty array would stop the program when called with
    // one: warn about it.
    for def in &defs {
        for (j, p) in def.params.iter().enumerate() {
            if !matches!(p.pattern, Some(ast::Pattern::Cons { .. })) {
                continue;
            }
            let empty_handled = defs.iter().any(|other| {
                other.name == def.name
                    && other.params.len() == def.params.len()
                    && match &other.params[j].pattern {
                        Some(ast::Pattern::Empty) => true,
                        Some(_) => false,
                        None => param_types(other)[j].contains(crate::types::Kind::Array),
                    }
            });
            if !empty_handled {
                warnings.push(Diagnostic::warning(
                    p.span,
                    format!(
                        "no definition of `{}` accepts an empty array here, so calling it with one stops the program; add one such as `{} [] = ...`",
                        def.name, def.name
                    ),
                ));
                break;
            }
        }
    }

    let main = groups
        .get("main")
        .and_then(|g| g.iter().copied().find(|&f| funcs[f].params.is_empty()));
    let main = match main {
        Some(m) => m,
        None => {
            let span = program
                .defs
                .first()
                .map(|d| d.name_span)
                .unwrap_or_default();
            errors.push(Diagnostic {
                severity: crate::diag::Severity::Error,
                kind: Some(ErrorKind::DefinitionError),
                message:
                    "the program has no `main` function (define `main = ...` with no arguments)"
                        .into(),
                span: if program.defs.is_empty() {
                    None
                } else {
                    Some(span)
                },
                notes: Vec::new(),
            });
            usize::MAX
        }
    };

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok((
        Hir {
            funcs,
            groups,
            main,
        },
        warnings,
    ))
}

/// The declared parameter types: from the signature, or `any`; a pattern
/// parameter (`(x:xs)`, `[]`) takes an array.
fn param_types(def: &ast::Def) -> Vec<Ty> {
    let declared = match &def.sig {
        Some(sig) if !sig.params.is_empty() => {
            sig.params.iter().map(|t| Ty::from_name(t.name)).collect()
        }
        _ => vec![Ty::ANY; def.params.len()],
    };
    declared
        .into_iter()
        .zip(&def.params)
        .map(|(t, p)| match p.pattern {
            Some(_) => t.intersect(Ty::ARRAY),
            None => t,
        })
        .collect()
}

fn has_patterns(def: &ast::Def) -> bool {
    def.params.iter().any(|p| p.pattern.is_some())
}

/// Resolves the names in one function body.
struct Resolver<'a> {
    arities: &'a HashMap<&'a str, Vec<usize>>,
    /// How this file's names reach the globals, in a program of several
    /// files.
    names: Option<&'a FileNames>,
    /// Visible variables, innermost last. This stack is the "symbol table"
    /// of the classic textbook: a loop body pushes its variables and pops
    /// them when it ends, so they are not visible after the loop.
    locals: Vec<(String, LocalId)>,
    /// Name of each variable by id.
    local_names: Vec<String>,
    /// Every variable given a value so far, in order; a loop looks at the
    /// part written while resolving its body.
    assigned: Vec<LocalId>,
    /// Variables that hold something borrowed: `@` parameters and the
    /// variables of loops over `@source`. They cannot get a new value.
    fixed: Vec<LocalId>,
    errors: Vec<Diagnostic>,
}

impl<'a> Resolver<'a> {
    fn stmts(&mut self, stmts: &[Stmt]) -> Vec<HStmt> {
        let mut out = Vec::new();
        for stmt in stmts {
            match self.stmt(stmt) {
                Ok(s) => out.push(s),
                Err(e) => self.errors.push(e),
            }
        }
        out
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<HStmt, Diagnostic> {
        match stmt {
            Stmt::Let { name, span, .. } if constant(name).is_some() => Err(Diagnostic::error(
                ErrorKind::DefinitionError,
                *span,
                format!("`{name}` is a built-in constant and cannot be given a new value"),
            )),
            Stmt::Let { name, span, value } => {
                // The value is resolved first, so in a first binding such as
                // `x = x * 2` the right-hand `x` cannot be the new variable.
                let value = self.expr(value)?;
                let id = match self.lookup_local(name) {
                    Some(id) => id,
                    None => self.new_local(name),
                };
                if self.fixed.contains(&id) {
                    return Err(Diagnostic::error(
                        ErrorKind::OwnershipError,
                        *span,
                        format!(
                            "`{name}` is borrowed, so it cannot get a new value: it still belongs to the caller (or to what the loop goes through)"
                        ),
                    ));
                }
                self.assigned.push(id);
                Ok(HStmt::Let { local: id, value })
            }
            Stmt::Expr(e) => Ok(HStmt::Expr(self.expr(e)?)),
            Stmt::Loop {
                pos,
                var,
                var_span,
                source,
                borrowed,
                body,
            } => {
                let source = self.expr(source)?;
                // Variables created from here on belong to the body.
                let first_inner = self.local_names.len();
                let visible = self.locals.len();
                let assigned = self.assigned.len();
                let pos = pos.as_ref().map(|(p, span)| (self.new_local(p), *span));
                let var = self.new_local(var);
                if *borrowed {
                    self.fixed.push(var);
                }
                let body = self.stmts(body);
                self.locals.truncate(visible);
                let mut carried: Vec<LocalId> = self.assigned[assigned..]
                    .iter()
                    .copied()
                    .filter(|&id| id < first_inner)
                    .collect();
                carried.sort();
                carried.dedup();
                Ok(HStmt::Loop {
                    pos,
                    var,
                    var_span: *var_span,
                    source,
                    borrowed: *borrowed,
                    body,
                    carried,
                    inner: (first_inner..self.local_names.len()).collect(),
                })
            }
        }
    }

    fn clauses(&mut self, clauses: &[ast::Clause]) -> Result<Vec<HClause>, Diagnostic> {
        let mut out = Vec::new();
        for c in clauses {
            out.push(match c {
                ast::Clause::Gen {
                    pos,
                    var,
                    source,
                    borrowed,
                    ..
                } => {
                    let source = self.expr(source)?;
                    HClause::Gen {
                        pos: pos.as_ref().map(|(p, _)| self.new_local(p)),
                        var: self.new_local(var),
                        source,
                        borrowed: *borrowed,
                    }
                }
                ast::Clause::Guard(g) => HClause::Guard(self.expr(g)?),
            });
        }
        Ok(out)
    }

    fn new_local(&mut self, name: &str) -> LocalId {
        let id = self.local_names.len();
        self.local_names.push(name.to_string());
        self.locals.push((name.to_string(), id));
        id
    }

    /// Look a variable up from the innermost scope outward. Globals are
    /// checked by the caller, last. That order is the "name shadow" rule
    /// from the readme.
    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    fn lookup_value(&self, name: &str) -> Option<HExprKind> {
        self.lookup_local(name).map(HExprKind::Local)
    }

    /// The global name `written` refers to: itself, except in a program of
    /// several files (see `FileNames`).
    fn global(&self, written: &str, span: Span) -> Result<String, Diagnostic> {
        match self.names {
            None => Ok(written.to_string()),
            Some(names) => names
                .global(written)
                .map_err(|m| Diagnostic::error(ErrorKind::SymbolNotFoundError, span, m)),
        }
    }

    /// How many arguments `name` takes, if it names a function: a
    /// variable is a value, and a constant or unknown name is no function.
    fn function_arities(&self, name: &str) -> Option<Vec<usize>> {
        if self.lookup_local(name).is_some() {
            return None;
        }
        let global = self.global(name, Span::default()).ok()?;
        if let Some(a) = self.arities.get(global.as_str()) {
            return Some(a.clone());
        }
        builtin_of(&global).map(|_| vec![1])
    }

    /// Arity-directed grouping of `callee arg1 arg2 ...` when the flat
    /// argument count fits no definition. Reading left to right, a
    /// function name among the arguments takes the arguments after it, as
    /// many as it accepts, so `echo sum xs` is `echo (sum xs)` and
    /// `f g x y` with one-argument `f` and two-argument `g` is
    /// `f (g x y)`. Returns the regrouped call, or `None` if no grouping
    /// works (the arity error is reported as before); several groupings
    /// are an error asking for parentheses.
    fn group_call(
        &self,
        callee: &str,
        callee_span: Span,
        args: &[ast::Expr],
        arities: &[usize],
    ) -> Result<Option<ast::Expr>, Diagnostic> {
        let mut found = Vec::new();
        for &n in arities {
            for (end, parts) in self.take_args(args, 0, n) {
                if end == args.len() {
                    found.push(parts);
                }
            }
        }
        match found.len() {
            0 => Ok(None),
            1 => {
                let parts = found.pop().unwrap();
                let span = callee_span.to(args.last().unwrap().span);
                Ok(Some(ast::Expr {
                    kind: ExprKind::Call {
                        callee: callee.to_string(),
                        callee_span,
                        args: parts,
                    },
                    span,
                }))
            }
            _ => Err(Diagnostic::error(
                ErrorKind::TypeError,
                callee_span.to(args.last().unwrap().span),
                format!(
                    "the arguments of `{callee}` can be grouped in more than one way; add parentheses to show which"
                ),
            )),
        }
    }

    /// The ways to read `n` arguments from `args[i..]`: where they end, and
    /// the argument expressions.
    fn take_args(&self, args: &[ast::Expr], i: usize, n: usize) -> Vec<(usize, Vec<ast::Expr>)> {
        if n == 0 {
            return vec![(i, Vec::new())];
        }
        let mut ways = Vec::new();
        for (next, first) in self.take_one(args, i) {
            for (end, mut rest) in self.take_args(args, next, n - 1) {
                rest.insert(0, first.clone());
                ways.push((end, rest));
            }
        }
        ways
    }

    /// The ways to read one argument starting at `args[i]`: the atom
    /// itself, or, for a function name, a call taking the following
    /// arguments.
    fn take_one(&self, args: &[ast::Expr], i: usize) -> Vec<(usize, ast::Expr)> {
        let Some(atom) = args.get(i) else {
            return Vec::new();
        };
        let ExprKind::Name(name) = &atom.kind else {
            return vec![(i + 1, atom.clone())];
        };
        let Some(arities) = self.function_arities(name) else {
            return vec![(i + 1, atom.clone())];
        };
        let mut ways = Vec::new();
        for k in arities {
            if k == 0 {
                // a function without arguments is used as a value
                ways.push((i + 1, atom.clone()));
                continue;
            }
            for (end, parts) in self.take_args(args, i + 1, k) {
                let span = atom.span.to(parts.last().unwrap().span);
                ways.push((
                    end,
                    ast::Expr {
                        kind: ExprKind::Call {
                            callee: name.clone(),
                            callee_span: atom.span,
                            args: parts,
                        },
                        span,
                    },
                ));
            }
        }
        ways
    }

    /// Check that `name` names a function taking `nargs` arguments, and
    /// give its global name.
    fn check_callable(&self, name: &str, nargs: usize, span: Span) -> Result<String, Diagnostic> {
        let global = self.global(name, span)?;
        if !self.arities.contains_key(global.as_str()) && builtin_of(&global).is_some() {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!("`{name}` is called with {nargs} argument(s), but it takes 1"),
            ));
        }
        let Some(arities) = self.arities.get(global.as_str()) else {
            return Err(Diagnostic::error(
                ErrorKind::SymbolNotFoundError,
                span,
                format!("cannot find symbol `{name}`"),
            ));
        };
        if !arities.contains(&nargs) {
            let mut list: Vec<String> = arities.iter().map(|a| a.to_string()).collect();
            list.dedup();
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!(
                    "`{name}` is called with {nargs} argument(s), but it takes {}",
                    list.join(" or ")
                ),
            ));
        }
        Ok(global)
    }

    fn expr(&mut self, e: &ast::Expr) -> Result<HExpr, Diagnostic> {
        let kind = match &e.kind {
            ExprKind::Int(v) => HExprKind::Int(*v),
            ExprKind::Float(v) => HExprKind::Float(*v),
            ExprKind::Str(s) => HExprKind::Str(s.clone()),
            ExprKind::Bool(b) => HExprKind::Bool(*b),
            ExprKind::Void => HExprKind::Void,
            // A bare name that is not a local or parameter must be a global.
            // Globals are functions, so `foo` is a call with no arguments.
            // This is how the readme's "sum = 1 is a function that always
            // returns 1" works.
            ExprKind::Name(name) => match self.lookup_value(name) {
                Some(k) => k,
                None if !self.arities.contains_key(name.as_str()) && constant(name).is_some() => {
                    HExprKind::Float(constant(name).unwrap())
                }
                None => HExprKind::Call {
                    name: self.check_callable(name, 0, e.span)?,
                    args: Vec::new(),
                },
            },
            ExprKind::Call {
                callee,
                callee_span,
                args,
            } => {
                if self.lookup_value(callee).is_some() {
                    return Err(Diagnostic::error(
                        ErrorKind::TypeError,
                        *callee_span,
                        format!("`{callee}` is a value, not a function, and cannot take arguments"),
                    ));
                }
                // More arguments than any definition takes: `echo sum xs`,
                // `head tail a`. Group them by how many arguments each
                // function takes.
                if let Some(arities) = self.function_arities(callee)
                    && !arities.contains(&args.len())
                    && let Some(grouped) = self.group_call(callee, *callee_span, args, &arities)?
                {
                    return self.expr(&grouped);
                }
                let global = self.global(callee, *callee_span)?;
                if !self.arities.contains_key(global.as_str())
                    && let Some(b) = builtin_of(&global)
                {
                    let [arg] = &args[..] else {
                        return Err(Diagnostic::error(
                            ErrorKind::TypeError,
                            *callee_span,
                            format!(
                                "`{callee}` is called with {} argument(s), but it takes 1",
                                args.len()
                            ),
                        ));
                    };
                    return Ok(HExpr {
                        kind: HExprKind::Builtin(b, Box::new(self.expr(arg)?)),
                        span: e.span,
                    });
                }
                let name = self.check_callable(callee, args.len(), *callee_span)?;
                let args = args
                    .iter()
                    .map(|a| self.expr(a))
                    .collect::<Result<_, _>>()?;
                HExprKind::Call { name, args }
            }
            ExprKind::Binary { op, lhs, rhs } => HExprKind::Binary {
                op: *op,
                lhs: Box::new(self.expr(lhs)?),
                rhs: Box::new(self.expr(rhs)?),
            },
            ExprKind::Neg(inner) => HExprKind::Neg(Box::new(self.expr(inner)?)),
            ExprKind::Ret(value) => HExprKind::Ret(match value {
                Some(v) => Some(Box::new(self.expr(v)?)),
                None => None,
            }),
            ExprKind::Array(items) => HExprKind::Array(
                items
                    .iter()
                    .map(|i| self.expr(i))
                    .collect::<Result<_, _>>()?,
            ),
            ExprKind::Range(start, end) => {
                HExprKind::Range(Box::new(self.expr(start)?), Box::new(self.expr(end)?))
            }
            ExprKind::Comprehension { body, clauses } => {
                // Each clause sees the variables of the generators before
                // it; the body sees them all. None exist after the brackets.
                let visible = self.locals.len();
                let resolved = self.clauses(clauses).and_then(|clauses| {
                    let body = self.expr(body)?;
                    Ok(HExprKind::Comprehension {
                        body: Box::new(body),
                        clauses,
                    })
                });
                self.locals.truncate(visible);
                resolved?
            }
            ExprKind::Echo(inner) => HExprKind::Echo(Box::new(self.expr(inner)?)),
            ExprKind::Check {
                scrutinee,
                arms,
                else_arm,
            } => HExprKind::Check {
                scrutinee: Box::new(self.expr(scrutinee)?),
                arms: arms
                    .iter()
                    .map(|a| Ok((self.expr(&a.pattern)?, self.expr(&a.body)?)))
                    .collect::<Result<_, Diagnostic>>()?,
                else_arm: match else_arm {
                    Some(e) => Some(Box::new(self.expr(e)?)),
                    None => None,
                },
            },
        };
        Ok(HExpr { kind, span: e.span })
    }
}
