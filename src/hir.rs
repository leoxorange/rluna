//! Name resolution: AST -> HIR.
//!
//! Every name is resolved here, so later passes never deal with scopes.
//! Lookup order is function body (locals, most recent first), then
//! arguments, then global definitions; failing all three is a
//! `SymbolNotFoundError`. Overloads are grouped by name; picking a specific
//! overload by argument types is left to type checking.

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
    pub local_names: Vec<String>,
}

#[derive(Debug)]
pub enum HStmt {
    Let { local: LocalId, value: HExpr },
    Expr(HExpr),
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
    Param(usize),
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

pub fn resolve(program: &ast::Program) -> Result<(Hir, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    // Collect signatures; a later definition with the same name, arity and
    // parameter types replaces the earlier one.
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

    // Renumber the surviving definitions densely.
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

    let mut funcs = Vec::new();
    for def in &defs {
        let mut r = Resolver {
            def,
            arities: &arities,
            locals: Vec::new(),
            local_names: Vec::new(),
        };
        let mut body = Vec::new();
        for stmt in &def.body.stmts {
            match r.stmt(stmt) {
                Ok(s) => body.push(s),
                Err(e) => errors.push(e),
            }
        }
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

struct Resolver<'a> {
    def: &'a ast::Def,
    arities: &'a HashMap<&'a str, Vec<usize>>,
    /// Visible locals, innermost (most recent) last.
    locals: Vec<(String, LocalId)>,
    local_names: Vec<String>,
}

impl<'a> Resolver<'a> {
    fn stmt(&mut self, stmt: &Stmt) -> Result<HStmt, Diagnostic> {
        match stmt {
            Stmt::Let { name, value, .. } => {
                // The value is resolved before the new binding is visible.
                let value = self.expr(value)?;
                let id = self.local_names.len();
                self.local_names.push(name.clone());
                self.locals.push((name.clone(), id));
                Ok(HStmt::Let { local: id, value })
            }
            Stmt::Expr(e) => Ok(HStmt::Expr(self.expr(e)?)),
        }
    }

    fn lookup_value(&self, name: &str) -> Option<HExprKind> {
        if let Some((_, id)) = self.locals.iter().rev().find(|(n, _)| n == name) {
            return Some(HExprKind::Local(*id));
        }
        self.def
            .params
            .iter()
            .position(|p| p.name == name)
            .map(HExprKind::Param)
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
