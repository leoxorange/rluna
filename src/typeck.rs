//! Type checking and specialization: HIR -> TIR.
//!
//! Luna functions are specialized on the static types of their arguments,
//! starting from `main`. Each (function, argument types) pair becomes one
//! *instance* with a fully typed body. Return types of (mutually) recursive
//! instances are found by fixpoint iteration: every instance starts out as
//! `never` and its return type only grows until nothing changes.
//!
//! Type annotations act as constraints: an argument whose type is not
//! contained in the declared parameter type is either a compile-time error
//! (no overlap) or gets a runtime check (partial overlap, e.g. passing an
//! `any` to a `numeric` parameter).

use std::collections::HashMap;

use crate::ast::BinOp;
use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::hir::{FuncId, HExpr, HExprKind, HStmt, Hir};
use crate::types::{Kind, Ty};

pub type InstId = usize;

#[derive(Debug)]
pub struct Tir {
    pub funcs: Vec<TFunc>,
    pub entry: InstId,
}

#[derive(Debug)]
pub struct TFunc {
    /// Source-level name, e.g. `sum`.
    pub name: String,
    /// Unique symbol for the instance, e.g. `sum` or `sum.2`.
    pub symbol: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// The function is declared `-> void`: the body's value is discarded.
    pub discard: bool,
    pub body: Vec<TStmt>,
    pub local_tys: Vec<Ty>,
}

impl TFunc {
    pub fn signature(&self) -> String {
        let ps: Vec<String> = self.params.iter().map(|t| t.to_string()).collect();
        format!("{}({}) -> {}", self.name, ps.join(", "), self.ret)
    }
}

#[derive(Debug)]
pub enum TStmt {
    Let { local: usize, value: TExpr },
    Expr(TExpr),
}

#[derive(Debug)]
pub struct TExpr {
    pub kind: TExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug)]
pub enum TExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Param(usize),
    Local(usize),
    Call {
        inst: InstId,
        args: Vec<TExpr>,
    },
    /// Evaluate the expressions, at least one of which never returns.
    Diverge(Vec<TExpr>),
    /// Arithmetic or comparison. Operands may be unions; the result type is
    /// `ty`. Operand kind pairs that are invalid fail at runtime.
    Binary {
        op: BinOp,
        lhs: Box<TExpr>,
        rhs: Box<TExpr>,
    },
    Neg(Box<TExpr>),
    Echo(Box<TExpr>),
    /// Each arm compares the scrutinee to the pattern with `==`.
    /// Arm bodies are already converted to the result type.
    Check {
        scrutinee: Box<TExpr>,
        arms: Vec<(TExpr, TExpr)>,
        else_arm: Option<Box<TExpr>>,
    },
    /// Change representation from the inner expression's type to `ty`.
    /// If `ty` does not contain the inner type, this is a runtime type check.
    Convert(Box<TExpr>),
}

/// Result type of applying an operator to two concrete kinds, if valid.
pub fn binop_result(op: BinOp, a: Kind, b: Kind) -> Option<Kind> {
    use Kind::*;
    let numeric = |k: Kind| matches!(k, Int | Float);
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => match (a, b) {
            (Int, Int) if op != BinOp::Div => Some(Int),
            _ if numeric(a) && numeric(b) => Some(Float),
            (Str, Str) if op == BinOp::Add => Some(Str),
            _ => None,
        },
        BinOp::Eq | BinOp::Ne => Some(Bool),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => match (a, b) {
            _ if numeric(a) && numeric(b) => Some(Bool),
            (Str, Str) => Some(Bool),
            _ => None,
        },
    }
}

struct Instance {
    func: FuncId,
    params: Vec<Ty>,
    ret: Ty,
    body: Vec<TStmt>,
    local_tys: Vec<Ty>,
}

pub fn check(hir: &Hir) -> Result<Tir, Vec<Diagnostic>> {
    let mut tc = Checker {
        hir,
        instances: Vec::new(),
        map: HashMap::new(),
    };
    let entry = tc.instantiate(hir.main, Vec::new());

    loop {
        let mut changed = false;
        let mut errors = Vec::new();
        let mut i = 0;
        while i < tc.instances.len() {
            match tc.check_instance(i) {
                Ok((body, local_tys, ret)) => {
                    let inst = &mut tc.instances[i];
                    let new_ret = inst.ret.union(ret);
                    if new_ret != inst.ret {
                        inst.ret = new_ret;
                        changed = true;
                    }
                    inst.body = body;
                    inst.local_tys = local_tys;
                }
                Err(e) => errors.push(e),
            }
            i += 1;
        }
        if !changed {
            if !errors.is_empty() {
                // Report each error once (several instances can hit the same one).
                let mut seen = Vec::new();
                errors.retain(|e: &Diagnostic| {
                    let key = (e.span.map(|s| (s.start, s.end)), e.message.clone());
                    if seen.contains(&key) {
                        false
                    } else {
                        seen.push(key);
                        true
                    }
                });
                return Err(errors);
            }
            break;
        }
    }

    Ok(tc.finish(entry))
}

struct Checker<'a> {
    hir: &'a Hir,
    instances: Vec<Instance>,
    map: HashMap<(FuncId, Vec<Ty>), InstId>,
}

struct Ctx<'a> {
    params: &'a [Ty],
    local_tys: Vec<Ty>,
}

fn err(span: Span, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(ErrorKind::TypeError, span, msg)
}

fn convert(e: TExpr, to: Ty) -> TExpr {
    if e.ty == to {
        return e;
    }
    let span = e.span;
    TExpr {
        kind: TExprKind::Convert(Box::new(e)),
        ty: to,
        span,
    }
}

impl<'a> Checker<'a> {
    fn instantiate(&mut self, func: FuncId, params: Vec<Ty>) -> InstId {
        let key = (func, params);
        if let Some(&id) = self.map.get(&key) {
            return id;
        }
        let id = self.instances.len();
        self.instances.push(Instance {
            func,
            params: key.1.clone(),
            ret: Ty::NEVER,
            body: Vec::new(),
            local_tys: Vec::new(),
        });
        self.map.insert(key, id);
        id
    }

    fn check_instance(&mut self, id: InstId) -> Result<(Vec<TStmt>, Vec<Ty>, Ty), Diagnostic> {
        let hir = self.hir;
        let func = &hir.funcs[self.instances[id].func];
        let params = self.instances[id].params.clone();
        let mut cx = Ctx {
            params: &params,
            local_tys: vec![Ty::NEVER; func.local_names.len()],
        };
        let mut body = Vec::new();
        let mut result = Ty::VOID;
        for (i, stmt) in func.body.iter().enumerate() {
            let last = i + 1 == func.body.len();
            match stmt {
                HStmt::Let { local, value } => {
                    let v = self.expr(&mut cx, value)?;
                    cx.local_tys[*local] = v.ty;
                    body.push(TStmt::Let {
                        local: *local,
                        value: v,
                    });
                    if last {
                        result = Ty::VOID;
                    }
                }
                HStmt::Expr(e) => {
                    let mut v = self.expr(&mut cx, e)?;
                    if last {
                        result = v.ty;
                        if let Some(declared) = func.ret
                            && declared != Ty::VOID
                        {
                            v = self.narrow(v, declared, || {
                                format!("`{}` is declared to return {declared}", func.name)
                            })?;
                            result = v.ty;
                        }
                    }
                    body.push(TStmt::Expr(v));
                }
            }
        }
        if func.ret == Some(Ty::VOID) {
            result = Ty::VOID;
        }
        Ok((body, cx.local_tys, result))
    }

    /// Convert `e` to `e.ty ∩ target`, inserting a runtime check if needed.
    fn narrow(
        &self,
        e: TExpr,
        target: Ty,
        what: impl FnOnce() -> String,
    ) -> Result<TExpr, Diagnostic> {
        if e.ty.is_subset(target) {
            return Ok(e);
        }
        let narrowed = e.ty.intersect(target);
        if narrowed.is_never() {
            return Err(err(e.span, format!("{}, but this is {}", what(), e.ty)));
        }
        Ok(convert(e, narrowed))
    }

    fn expr(&mut self, cx: &mut Ctx, e: &HExpr) -> Result<TExpr, Diagnostic> {
        let span = e.span;
        let (kind, ty) = match &e.kind {
            HExprKind::Int(v) => (TExprKind::Int(*v), Ty::INT),
            HExprKind::Float(v) => (TExprKind::Float(*v), Ty::FLOAT),
            HExprKind::Str(s) => (TExprKind::Str(s.clone()), Ty::STR),
            HExprKind::Bool(b) => (TExprKind::Bool(*b), Ty::BOOL),
            HExprKind::Param(i) => (TExprKind::Param(*i), cx.params[*i]),
            HExprKind::Local(l) => (TExprKind::Local(*l), cx.local_tys[*l]),
            HExprKind::Call { name, args } => return self.call(cx, name, args, span),
            HExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.expr(cx, lhs)?;
                let rhs = self.expr(cx, rhs)?;
                let ty = if lhs.ty.is_never() || rhs.ty.is_never() {
                    Ty::NEVER
                } else {
                    let mut ty = Ty::NEVER;
                    for a in lhs.ty.kinds() {
                        for b in rhs.ty.kinds() {
                            if let Some(r) = binop_result(*op, a, b) {
                                ty = ty.union(Ty::of(r));
                            }
                        }
                    }
                    if ty.is_never() {
                        return Err(err(
                            span,
                            format!(
                                "operator `{}` cannot be applied to {} and {}",
                                op.as_str(),
                                lhs.ty,
                                rhs.ty
                            ),
                        ));
                    }
                    ty
                };
                (
                    TExprKind::Binary {
                        op: *op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    ty,
                )
            }
            HExprKind::Neg(inner) => {
                let inner = self.expr(cx, inner)?;
                let ty = if inner.ty.is_never() {
                    Ty::NEVER
                } else {
                    let ty = inner.ty.intersect(Ty::NUMERIC);
                    if ty.is_never() {
                        return Err(err(
                            span,
                            format!("cannot negate a value of type {}", inner.ty),
                        ));
                    }
                    ty
                };
                (TExprKind::Neg(Box::new(inner)), ty)
            }
            HExprKind::Echo(inner) => {
                let inner = self.expr(cx, inner)?;
                (TExprKind::Echo(Box::new(inner)), Ty::VOID)
            }
            HExprKind::Check {
                scrutinee,
                arms,
                else_arm,
            } => {
                let scrutinee = self.expr(cx, scrutinee)?;
                let mut tarms = Vec::new();
                let mut ty = Ty::NEVER;
                let mut seen_true = false;
                let mut seen_false = false;
                for (pat, body) in arms {
                    let pat = self.expr(cx, pat)?;
                    match pat.kind {
                        TExprKind::Bool(true) => seen_true = true,
                        TExprKind::Bool(false) => seen_false = true,
                        _ => {}
                    }
                    let body = self.expr(cx, body)?;
                    ty = ty.union(body.ty);
                    tarms.push((pat, body));
                }
                let else_arm = match else_arm {
                    Some(e) => {
                        let e = self.expr(cx, e)?;
                        ty = ty.union(e.ty);
                        Some(e)
                    }
                    None => {
                        let exhaustive =
                            scrutinee.ty.is_subset(Ty::BOOL) && seen_true && seen_false;
                        if !exhaustive {
                            return Err(err(
                                span,
                                format!(
                                    "`check` on a value of type {} needs an `else` branch to cover all values",
                                    scrutinee.ty
                                ),
                            ));
                        }
                        None
                    }
                };
                let tarms = tarms
                    .into_iter()
                    .map(|(p, b)| (p, convert(b, ty)))
                    .collect();
                let else_arm = else_arm.map(|e| Box::new(convert(e, ty)));
                (
                    TExprKind::Check {
                        scrutinee: Box::new(scrutinee),
                        arms: tarms,
                        else_arm,
                    },
                    ty,
                )
            }
        };
        Ok(TExpr { kind, ty, span })
    }

    fn call(
        &mut self,
        cx: &mut Ctx,
        name: &str,
        args: &[HExpr],
        span: Span,
    ) -> Result<TExpr, Diagnostic> {
        let args: Vec<TExpr> = args
            .iter()
            .map(|a| self.expr(cx, a))
            .collect::<Result<_, _>>()?;
        if args.iter().any(|a| a.ty.is_never()) {
            // The call is never reached.
            return Ok(TExpr {
                kind: TExprKind::Diverge(args),
                ty: Ty::NEVER,
                span,
            });
        }
        let hir = self.hir;
        let candidates: Vec<FuncId> = hir.groups[name]
            .iter()
            .copied()
            .filter(|&f| hir.funcs[f].params.len() == args.len())
            .collect();

        let full: Vec<FuncId> = candidates
            .iter()
            .copied()
            .filter(|&f| {
                args.iter()
                    .zip(&hir.funcs[f].param_tys)
                    .all(|(a, t)| a.ty.is_subset(*t))
            })
            .collect();
        let chosen = if !full.is_empty() {
            // Most specific: its parameter types are contained in every other match's.
            let more_specific = |f: FuncId, g: FuncId| {
                hir.funcs[f]
                    .param_tys
                    .iter()
                    .zip(&hir.funcs[g].param_tys)
                    .all(|(a, b)| a.is_subset(*b))
            };
            let best: Vec<FuncId> = full
                .iter()
                .copied()
                .filter(|&f| full.iter().all(|&g| more_specific(f, g)))
                .collect();
            if best.len() != 1 {
                return Err(err(
                    span,
                    format!("call to `{name}` is ambiguous between several definitions"),
                ));
            }
            best[0]
        } else {
            let partial: Vec<FuncId> = candidates
                .iter()
                .copied()
                .filter(|&f| {
                    args.iter()
                        .zip(&hir.funcs[f].param_tys)
                        .all(|(a, t)| !a.ty.intersect(*t).is_never())
                })
                .collect();
            match partial.len() {
                1 => partial[0],
                0 => {
                    let arg_tys: Vec<String> = args.iter().map(|a| a.ty.to_string()).collect();
                    if candidates.len() == 1 {
                        let f = &hir.funcs[candidates[0]];
                        let (i, (a, t)) = args
                            .iter()
                            .zip(&f.param_tys)
                            .enumerate()
                            .find(|(_, (a, t))| a.ty.intersect(**t).is_never())
                            .unwrap();
                        return Err(err(
                            a.span,
                            format!(
                                "argument {} of `{name}` must be {t}, but this is {}",
                                i + 1,
                                a.ty
                            ),
                        ));
                    }
                    return Err(err(
                        span,
                        format!(
                            "no definition of `{name}` accepts arguments of type ({})",
                            arg_tys.join(", ")
                        ),
                    ));
                }
                _ => {
                    return Err(err(
                        span,
                        format!(
                            "cannot choose a definition of `{name}` at compile time; runtime overload dispatch is not supported yet"
                        ),
                    ));
                }
            }
        };

        let param_tys = hir.funcs[chosen].param_tys.clone();
        let args: Vec<TExpr> = args
            .into_iter()
            .zip(&param_tys)
            .map(|(a, t)| {
                let target = a.ty.intersect(*t);
                convert(a, target)
            })
            .collect();
        let inst = self.instantiate(chosen, args.iter().map(|a| a.ty).collect());
        let ret = self.instances[inst].ret;
        Ok(TExpr {
            kind: TExprKind::Call { inst, args },
            ty: ret,
            span,
        })
    }

    /// Keep only instances reachable from the entry and assign symbols.
    fn finish(mut self, entry: InstId) -> Tir {
        let mut order = vec![entry];
        let mut index: HashMap<InstId, usize> = HashMap::from([(entry, 0)]);
        let mut i = 0;
        while i < order.len() {
            let mut callees = Vec::new();
            for s in &self.instances[order[i]].body {
                match s {
                    TStmt::Let { value, .. } | TStmt::Expr(value) => {
                        collect_calls(value, &mut callees)
                    }
                }
            }
            for c in callees {
                if let std::collections::hash_map::Entry::Vacant(e) = index.entry(c) {
                    e.insert(order.len());
                    order.push(c);
                }
            }
            i += 1;
        }

        let mut count: HashMap<&str, usize> = HashMap::new();
        for &id in &order {
            *count
                .entry(&self.hir.funcs[self.instances[id].func].name)
                .or_default() += 1;
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut funcs = Vec::new();
        for &id in &order {
            let inst = &mut self.instances[id];
            let func = &self.hir.funcs[inst.func];
            let symbol = if count[func.name.as_str()] == 1 {
                func.name.clone()
            } else {
                let n = seen.entry(func.name.clone()).or_default();
                *n += 1;
                format!("{}.{}", func.name, n)
            };
            let mut body = std::mem::take(&mut inst.body);
            for s in &mut body {
                match s {
                    TStmt::Let { value, .. } | TStmt::Expr(value) => remap_calls(value, &index),
                }
            }
            funcs.push(TFunc {
                name: func.name.clone(),
                symbol,
                params: inst.params.clone(),
                ret: inst.ret,
                discard: func.ret == Some(Ty::VOID),
                body,
                local_tys: std::mem::take(&mut inst.local_tys),
            });
        }
        Tir { funcs, entry: 0 }
    }
}

fn visit_children(e: &TExpr, f: &mut impl FnMut(&TExpr)) {
    match &e.kind {
        TExprKind::Int(_)
        | TExprKind::Float(_)
        | TExprKind::Str(_)
        | TExprKind::Bool(_)
        | TExprKind::Param(_)
        | TExprKind::Local(_) => {}
        TExprKind::Call { args, .. } | TExprKind::Diverge(args) => args.iter().for_each(f),
        TExprKind::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        TExprKind::Neg(x) | TExprKind::Echo(x) | TExprKind::Convert(x) => f(x),
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
        } => {
            f(scrutinee);
            for (p, b) in arms {
                f(p);
                f(b);
            }
            if let Some(e) = else_arm {
                f(e);
            }
        }
    }
}

fn collect_calls(e: &TExpr, out: &mut Vec<InstId>) {
    if let TExprKind::Call { inst, .. } = &e.kind {
        out.push(*inst);
    }
    visit_children(e, &mut |c| collect_calls(c, out));
}

fn remap_calls(e: &mut TExpr, index: &HashMap<InstId, usize>) {
    match &mut e.kind {
        TExprKind::Call { inst, args } => {
            *inst = index[inst];
            args.iter_mut().for_each(|a| remap_calls(a, index));
        }
        TExprKind::Diverge(args) => args.iter_mut().for_each(|a| remap_calls(a, index)),
        TExprKind::Binary { lhs, rhs, .. } => {
            remap_calls(lhs, index);
            remap_calls(rhs, index);
        }
        TExprKind::Neg(x) | TExprKind::Echo(x) | TExprKind::Convert(x) => remap_calls(x, index),
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
        } => {
            remap_calls(scrutinee, index);
            for (p, b) in arms {
                remap_calls(p, index);
                remap_calls(b, index);
            }
            if let Some(e) = else_arm {
                remap_calls(e, index);
            }
        }
        _ => {}
    }
}
