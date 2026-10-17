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
        var: LocalId,
        /// Where the loop variable is named, for error messages.
        var_span: Span,
        start: HExpr,
        end: HExpr,
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
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    // Pass 1. Collect signatures; a later definition with the same name,
    // arity and parameter types replaces the earlier one (this is how the
    // readme's `foo = ...` then `foo = ...` redefinition works).
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut live: Vec<Option<&ast::Def>> = Vec::new();
    for def in &program.defs {
        let tys = param_types(def);
        let group = groups.entry(def.name.clone()).or_default();
        if let Some(pos) = group.iter().position(|&i| {
            let other = live[i].unwrap();
            other.params.len() == def.params.len() && param_types(other) == tys
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
            let covering = group[..k].iter().find(|&&earlier| {
                defs[earlier].params.len() == later_def.params.len()
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
        let mut r = Resolver {
            arities: &arities,
            locals: params.iter().cloned().zip(0..).collect(),
            local_names: params,
            assigned: Vec::new(),
            errors: Vec::new(),
        };
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
        });
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

fn param_types(def: &ast::Def) -> Vec<Ty> {
    match &def.sig {
        Some(sig) if !sig.params.is_empty() => {
            sig.params.iter().map(|t| Ty::from_name(t.name)).collect()
        }
        _ => vec![Ty::ANY; def.params.len()],
    }
}

/// Resolves the names in one function body.
struct Resolver<'a> {
    arities: &'a HashMap<&'a str, Vec<usize>>,
    /// Visible variables, innermost last. This stack is the "symbol table"
    /// of the classic textbook: a loop body pushes its variables and pops
    /// them when it ends, so they are not visible after the loop.
    locals: Vec<(String, LocalId)>,
    /// Name of each variable by id.
    local_names: Vec<String>,
    /// Every variable given a value so far, in order; a loop looks at the
    /// part written while resolving its body.
    assigned: Vec<LocalId>,
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
            Stmt::Let { name, value, .. } => {
                // The value is resolved first, so in a first binding such as
                // `x = x * 2` the right-hand `x` cannot be the new variable.
                let value = self.expr(value)?;
                let id = match self.lookup_local(name) {
                    Some(id) => id,
                    None => self.new_local(name),
                };
                self.assigned.push(id);
                Ok(HStmt::Let { local: id, value })
            }
            Stmt::Expr(e) => Ok(HStmt::Expr(self.expr(e)?)),
            Stmt::Loop {
                var,
                var_span,
                start,
                end,
                body,
            } => {
                let start = self.expr(start)?;
                let end = self.expr(end)?;
                // Variables created from here on belong to the body.
                let first_inner = self.local_names.len();
                let visible = self.locals.len();
                let assigned = self.assigned.len();
                let var = self.new_local(var);
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
                    var,
                    var_span: *var_span,
                    start,
                    end,
                    body,
                    carried,
                    inner: (first_inner..self.local_names.len()).collect(),
                })
            }
        }
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

    fn check_callable(&self, name: &str, nargs: usize, span: Span) -> Result<(), Diagnostic> {
        let Some(arities) = self.arities.get(name) else {
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
        Ok(())
    }

    fn expr(&mut self, e: &ast::Expr) -> Result<HExpr, Diagnostic> {
        let kind = match &e.kind {
            ExprKind::Int(v) => HExprKind::Int(*v),
            ExprKind::Float(v) => HExprKind::Float(*v),
            ExprKind::Str(s) => HExprKind::Str(s.clone()),
            ExprKind::Bool(b) => HExprKind::Bool(*b),
            // A bare name that is not a local or parameter must be a global.
            // Globals are functions, so `foo` is a call with no arguments.
            // This is how the readme's "sum = 1 is a function that always
            // returns 1" works.
            ExprKind::Name(name) => match self.lookup_value(name) {
                Some(k) => k,
                None => {
                    self.check_callable(name, 0, e.span)?;
                    HExprKind::Call {
                        name: name.clone(),
                        args: Vec::new(),
                    }
                }
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
                self.check_callable(callee, args.len(), *callee_span)?;
                let args = args
                    .iter()
                    .map(|a| self.expr(a))
                    .collect::<Result<_, _>>()?;
                HExprKind::Call {
                    name: callee.clone(),
                    args,
                }
            }
            ExprKind::Binary { op, lhs, rhs } => HExprKind::Binary {
                op: *op,
                lhs: Box::new(self.expr(lhs)?),
                rhs: Box::new(self.expr(rhs)?),
            },
            ExprKind::Neg(inner) => HExprKind::Neg(Box::new(self.expr(inner)?)),
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
