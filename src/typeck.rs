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
//!
//! Why specialize? Luna functions usually have no type annotations, so
//! `sum a b = a + b` could receive ints, floats or strings. One machine-code
//! version that works for everything would have to check types and box
//! values on every `+`. Instead, the compiler makes a separate copy per
//! combination of argument types actually used. `sum 1 2` gets an int-only
//! copy that compiles to a single add instruction; `sum 1.5 2` gets a float
//! copy. This is called *monomorphization*; C++ templates and Rust generics
//! do the same.
//!
//! Example of the fixpoint, for `count n = check n: 0 -> 0; else -> 1 + count (n - 1)`:
//!
//! - round 1: `count(int)` returns `never` so far, so `1 + count(...)` is
//!   `never` and the check's type is `int ∪ never = int`;
//! - round 2: now `count(int)` returns `int`, so `1 + count(...)` is `int`,
//!   and the result is still `int`. Nothing changed, so we stop.
//!
//! Types only ever grow (union) and there are finitely many, so the loop
//! always ends.
//!
//! The output, TIR ("typed IR"), is the HIR tree with a type on every node,
//! every call pointing at a concrete instance, and explicit `Convert` nodes
//! wherever a value must change representation. Lowering then never has to
//! make a type decision itself.

use std::collections::HashMap;

use crate::ast::BinOp;
use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::hir::{Builtin, FuncId, HClause, HExpr, HExprKind, HPattern, HStmt, Hir};
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
    /// Type of each variable after its last assignment in the body; the
    /// parameters come first. (Only the count and the parameters are
    /// meaningful: a variable's type can change from one assignment to the
    /// next, so each expression carries its own type.)
    pub local_tys: Vec<Ty>,
    /// Name of each variable, the parameters first, for error messages.
    pub local_names: Vec<String>,
    /// For each parameter, the pattern that takes its array apart, if any.
    pub patterns: Vec<Option<HPattern>>,
    /// For each parameter, whether it is borrowed (`@a`).
    pub borrowed: Vec<bool>,
}

impl TFunc {
    pub fn signature(&self) -> String {
        let ps: Vec<String> = self.params.iter().map(|t| t.to_string()).collect();
        format!("{}({}) -> {}", self.name, ps.join(", "), self.ret)
    }
}

#[derive(Debug)]
pub enum TStmt {
    /// Give a variable a value: its first, or a new one.
    Let {
        local: usize,
        value: TExpr,
    },
    Expr(TExpr),
    /// `loop var in source:` / `loop pos, var in source:`.
    Loop {
        pos: Option<usize>,
        var: usize,
        source: Box<LoopSource>,
        /// `in @source`: the source is borrowed and `var` is a borrowed
        /// element; otherwise the source moves into the loop.
        borrowed: bool,
        body: Vec<TStmt>,
        /// Outside variables the body updates, with their type at the top of
        /// every iteration (and after the loop): the union of their type
        /// before the loop and at the end of the body.
        carried: Vec<(usize, Ty)>,
        /// Variables that exist only inside the body.
        inner: Vec<usize>,
    },
}

/// What a loop or a list comprehension goes through.
#[derive(Debug)]
pub enum LoopSource {
    /// `[start..end]` (ints): counted directly, without building an array.
    Range(TExpr, TExpr),
    /// The elements of an array, or the characters of a string.
    Each(TExpr),
}

/// A clause of a list comprehension.
#[derive(Debug)]
pub enum TClause {
    /// `pos, var in source`: runs once for every element of the generators
    /// before it.
    Gen {
        pos: Option<usize>,
        var: usize,
        source: LoopSource,
        /// `in @source`, as for `TStmt::Loop`.
        borrowed: bool,
    },
    /// Keeps only the combinations for which it is true.
    Guard(TExpr),
}

impl TClause {
    pub fn exprs(&self) -> Vec<&TExpr> {
        match self {
            TClause::Gen { source, .. } => source.exprs(),
            TClause::Guard(g) => vec![g],
        }
    }

    pub fn exprs_mut(&mut self) -> Vec<&mut TExpr> {
        match self {
            TClause::Gen { source, .. } => source.exprs_mut(),
            TClause::Guard(g) => vec![g],
        }
    }
}

impl LoopSource {
    pub fn exprs(&self) -> Vec<&TExpr> {
        match self {
            LoopSource::Range(a, b) => vec![a, b],
            LoopSource::Each(e) => vec![e],
        }
    }

    pub fn exprs_mut(&mut self) -> Vec<&mut TExpr> {
        match self {
            LoopSource::Range(a, b) => vec![a, b],
            LoopSource::Each(e) => vec![e],
        }
    }
}

/// Call `f` on every expression of these statements that is not part of a
/// larger expression, including those inside loop bodies.
pub fn for_each_expr(stmts: &[TStmt], f: &mut impl FnMut(&TExpr)) {
    for s in stmts {
        match s {
            TStmt::Let { value, .. } | TStmt::Expr(value) => f(value),
            TStmt::Loop { source, body, .. } => {
                source.exprs().into_iter().for_each(&mut *f);
                for_each_expr(body, f);
            }
        }
    }
}

/// `for_each_expr` with mutable access.
pub fn for_each_expr_mut(stmts: &mut [TStmt], f: &mut impl FnMut(&mut TExpr)) {
    for s in stmts {
        match s {
            TStmt::Let { value, .. } | TStmt::Expr(value) => f(value),
            TStmt::Loop { source, body, .. } => {
                source.exprs_mut().into_iter().for_each(&mut *f);
                for_each_expr_mut(body, f);
            }
        }
    }
}

#[derive(Debug)]
pub struct TExpr {
    pub kind: TExprKind,
    pub ty: Ty,
    pub span: Span,
}

impl TExpr {
    /// The value is borrowed from something else (a variable, or an array
    /// that holds it), so whoever reads it must not free it.
    ///
    /// A variable's value belongs to the variable. `head a`, `last a` and
    /// `a !! i` give the element itself, still inside `a`, rather than a
    /// copy of it: Luna never copies a string or an array without being
    /// asked to (`copy`). Elements of `Copy` types are copied as usual, and
    /// so are the characters of a string, which are new strings anyway.
    pub fn is_borrowed(&self) -> bool {
        match &self.kind {
            TExprKind::Local(_) | TExprKind::Str(_) => true,
            TExprKind::Builtin(Builtin::Head | Builtin::Last, a) | TExprKind::Index(a, _) => {
                gives_element_itself(a.ty)
            }
            TExprKind::Convert(inner) => inner.is_borrowed(),
            _ => false,
        }
    }

    /// The variable whose value this borrowed value is part of: `a` for
    /// `a`, `head a` or `a !! 2`. Moving that variable, or giving it a new
    /// value, while the borrowed value is in use would free it.
    pub fn borrowed_from(&self) -> Option<usize> {
        match &self.kind {
            TExprKind::Local(l) => Some(*l),
            TExprKind::Builtin(Builtin::Head | Builtin::Last, a) | TExprKind::Index(a, _)
                if gives_element_itself(a.ty) =>
            {
                a.borrowed_from()
            }
            TExprKind::Convert(inner) => inner.borrowed_from(),
            _ => None,
        }
    }
}

/// Whether `head`, `last` and `!!` on a value of type `ty` give the element
/// itself (borrowed) rather than a copy: for arrays of strings or arrays.
/// A value that may be an array or a string (`[string]|string`) gets a
/// copy, because the string case makes a new character either way and both
/// cases must give the same kind of result.
pub fn gives_element_itself(ty: Ty) -> bool {
    ty.elem().is_some_and(|e| !e.is_copy()) && !ty.contains(Kind::Str)
}

#[derive(Debug)]
pub enum TExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    /// Read a variable (parameters are variables too).
    Local(usize),
    Call {
        inst: InstId,
        args: Vec<TExpr>,
    },
    /// A call whose definition is chosen at run time: the first case whose
    /// tests the arguments pass is called. If none does and the cases are
    /// not `exhaustive`, the program stops with a type error.
    Dispatch {
        name: String,
        args: Vec<TExpr>,
        cases: Vec<DispatchCase>,
        exhaustive: bool,
    },
    /// Evaluate the expressions, at least one of which never returns.
    /// For example, a call whose argument is an infinite recursion: the
    /// arguments still run (for their side effects), but the call itself
    /// is never reached, so no instance is created for it.
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
        /// Arms where the checked variable cannot be void.
        narrowing: Option<Narrowing>,
    },
    /// The void value.
    Void,
    /// `[a, b, c]`: the elements, converted to the element type.
    Array(Vec<TExpr>),
    /// `[start..end]`: an array of ints.
    Range(Box<TExpr>, Box<TExpr>),
    /// `[body | clauses]`: generators and (boolean) guards.
    Comprehension {
        body: Box<TExpr>,
        clauses: Vec<TClause>,
    },
    /// `ret value` / `ret`: leave the function.
    Ret(Option<Box<TExpr>>),
    /// A built-in function on an array or a string, which it only reads.
    Builtin(Builtin, Box<TExpr>),
    /// `a ++ b`: both operands are arrays (or void), which it only reads.
    Concat(Box<TExpr>, Box<TExpr>),
    /// `a !! i`: an element of an array or a character of a string, or void
    /// if `i` is out of range.
    Index(Box<TExpr>, Box<TExpr>),
    /// `a ** b`: element by element products of two arrays, which it only
    /// reads.
    ElemMul(Box<TExpr>, Box<TExpr>),
    /// Change representation from the inner expression's type to `ty`.
    /// If `ty` does not contain the inner type, this is a runtime type check.
    ///
    /// Types decide representation (see `types.rs`): an `int` is a raw
    /// number but an `int|string` is a pointer to a tagged box. Whenever a
    /// value flows from one type to another (an int arm of a `check` whose
    /// result is `int|string`, say), the machine code must box or unbox it.
    /// Making that an explicit node keeps lowering simple.
    Convert(Box<TExpr>),
}

/// In a `check` on a variable of a maybe type, the arms that only run when
/// the variable is not void see it with type `ty` (the maybe type without
/// void).
#[derive(Debug)]
pub struct Narrowing {
    pub local: usize,
    pub ty: Ty,
    /// For each arm, whether it sees the narrowed type.
    pub arms: Vec<bool>,
    pub else_arm: bool,
}

/// One definition a `Dispatch` may call.
#[derive(Debug)]
pub struct DispatchCase {
    /// For each argument, the type its value must have at run time, or
    /// `None` when every possible value fits.
    pub tests: Vec<Option<Ty>>,
    /// For each argument, the length its array must have (a pattern).
    pub lens: Vec<Option<LenTest>>,
    /// The instance's parameter types: the arguments are converted to them.
    pub params: Vec<Ty>,
    pub inst: InstId,
}

/// The length test of a pattern parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LenTest {
    /// `(x:xs)`: at least this many elements.
    AtLeast(usize),
    /// `[]`.
    Empty,
}

impl LenTest {
    pub fn accepts(self, len: usize) -> bool {
        match self {
            LenTest::AtLeast(n) => len >= n,
            LenTest::Empty => len == 0,
        }
    }
}

/// A definition a call may reach, while resolving the call.
struct Case {
    f: FuncId,
    tests: Vec<Option<Ty>>,
    lens: Vec<Option<LenTest>>,
    /// The argument types it receives.
    receives: Vec<Ty>,
}

/// Result type of applying an operator to two concrete kinds, if valid.
///
/// This table is the single source of truth for Luna's operator rules. The
/// type checker uses it to compute static types, and lowering uses it to
/// generate the runtime dispatch for union operands, so the two cannot
/// disagree.
pub fn binop_result(op: BinOp, a: Kind, b: Kind) -> Option<Kind> {
    use Kind::*;
    let numeric = |k: Kind| matches!(k, Int | Float);
    // Nothing flows through: an operation on void gives void, so
    // `head a + 1` is void when `a` is empty.
    if a == Void || b == Void {
        return Some(Void);
    }
    match op {
        // Typed with their element types, see `Checker::concat` etc.
        BinOp::Concat | BinOp::Index | BinOp::ElemMul => None,
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
    // Start from `main`: only code reachable from it is compiled. Calls found
    // while checking a body create new instances, which this same loop then
    // checks too (the `while` re-reads `instances.len()`).
    let entry = tc.instantiate(hir.main, Vec::new());

    // The fixpoint loop: re-check every instance until no return type grows.
    // Each round uses the return types found by the previous one. Errors are
    // only reported once types are stable, because an error seen in an early
    // round may disappear once a callee's return type is known.
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
    /// (function, argument types) -> instance. Memoizing here is what makes
    /// recursion work: `count` calling `count(int)` finds the instance that
    /// is already being checked, instead of creating copies forever.
    map: HashMap<(FuncId, Vec<Ty>), InstId>,
}

struct Ctx<'a> {
    /// The current type of each variable. It follows the program: after
    /// `x = 5` it is int, after `x = "a"` string.
    local_tys: Vec<Ty>,
    names: &'a [String],
    /// The variables of the loops being checked, innermost last, with where
    /// each is named.
    loop_vars: Vec<(usize, Span, Ty)>,
    /// The types `ret` gives in this function so far.
    ret_ty: Ty,
    /// The declared return type, if any.
    declared: Option<Ty>,
    /// The function is declared `-> void`: `ret` values are discarded.
    discard: bool,
    /// How many list comprehensions enclose the current expression.
    in_comprehension: usize,
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
            // Start at the bottom of the lattice: "returns nothing yet".
            // A recursive call seen before the body is understood
            // contributes nothing, and the fixpoint loop fills in the truth.
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
        let params = &self.instances[id].params;
        // The parameters are the first variables, typed by this instance.
        let mut local_tys = params.clone();
        local_tys.resize(func.local_names.len(), Ty::NEVER);
        // A pattern's parts: elements of the array, and the rest of it.
        for (p, pattern) in params.iter().zip(&func.patterns) {
            if let Some(HPattern::Cons { heads, rest }) = pattern {
                let elem = p.elem().unwrap_or(Ty::NEVER);
                for &h in heads {
                    local_tys[h] = elem;
                }
                local_tys[*rest] = Ty::array(elem);
            }
        }
        let mut cx = Ctx {
            local_tys,
            names: &func.local_names,
            loop_vars: Vec::new(),
            ret_ty: Ty::NEVER,
            declared: func.ret,
            discard: func.ret == Some(Ty::VOID),
            in_comprehension: 0,
        };
        let mut body = self.stmts(&mut cx, &func.body)?;
        // The value of the last statement is the result, if it is an
        // expression (a binding or a loop gives void).
        let mut result = Ty::VOID;
        if let Some(TStmt::Expr(_)) = body.last() {
            let Some(TStmt::Expr(mut v)) = body.pop() else {
                unreachable!()
            };
            result = v.ty;
            if let Some(declared) = func.ret
                && declared != Ty::VOID
            {
                v = self.narrow(v, declared, || {
                    format!("`{}` is declared to return {declared}", func.name)
                })?;
                result = v.ty;
            }
            body.push(TStmt::Expr(v));
        }
        // `ret` gives the function's result too.
        result = result.union(cx.ret_ty);
        if func.ret == Some(Ty::VOID) {
            result = Ty::VOID;
        }
        Ok((body, cx.local_tys, result))
    }

    fn stmts(&mut self, cx: &mut Ctx, stmts: &[HStmt]) -> Result<Vec<TStmt>, Diagnostic> {
        stmts.iter().map(|s| self.stmt(cx, s)).collect()
    }

    fn stmt(&mut self, cx: &mut Ctx, stmt: &HStmt) -> Result<TStmt, Diagnostic> {
        Ok(match stmt {
            HStmt::Let { local, value } => {
                let v = self.expr(cx, value)?;
                // A loop variable may get another value, never another type.
                if let Some(&(_, declared, var_ty)) =
                    cx.loop_vars.iter().find(|(l, _, _)| l == local)
                    && !v.ty.is_subset(var_ty)
                {
                    let name = &cx.names[*local];
                    let must = match var_ty {
                        Ty::INT => "an int".to_string(),
                        Ty::STR => "a string".to_string(),
                        t => t.to_string(),
                    };
                    return Err(err(
                        v.span,
                        format!(
                            "`{name}` is the variable of a loop and must stay {must}, but this is {}",
                            v.ty
                        ),
                    )
                    .with_note(declared, format!("`{name}` is the variable of this loop")));
                }
                cx.local_tys[*local] = v.ty;
                TStmt::Let {
                    local: *local,
                    value: v,
                }
            }
            HStmt::Expr(e) => TStmt::Expr(self.expr(cx, e)?),
            HStmt::Loop {
                pos,
                var,
                var_span,
                source,
                borrowed,
                body,
                carried,
                inner,
            } => {
                let (source, var_ty) = self.loop_source(cx, source)?;
                // Another fixpoint, like the one for return types: a
                // variable the body updates may come back from an iteration
                // with a type it did not have before (`x = 0` before the
                // loop, `x = "s"` inside), and the next iteration must
                // accept both. Check the body with the types at the top of
                // the loop, widen them by the types at the end, and repeat
                // until they no longer grow.
                let mut header: Vec<Ty> = carried.iter().map(|&l| cx.local_tys[l]).collect();
                loop {
                    for (&l, &t) in carried.iter().zip(&header) {
                        cx.local_tys[l] = t;
                    }
                    for &l in inner {
                        cx.local_tys[l] = Ty::NEVER;
                    }
                    cx.local_tys[*var] = var_ty;
                    cx.loop_vars.push((*var, *var_span, var_ty));
                    // The position is an int, and stays one.
                    if let Some((p, span)) = pos {
                        cx.local_tys[*p] = Ty::INT;
                        cx.loop_vars.push((*p, *span, Ty::INT));
                    }
                    let checked = self.stmts(cx, body);
                    if pos.is_some() {
                        cx.loop_vars.pop();
                    }
                    cx.loop_vars.pop();
                    let checked = checked?;
                    let widened: Vec<Ty> = carried
                        .iter()
                        .zip(&header)
                        .map(|(&l, &t)| t.union(cx.local_tys[l]))
                        .collect();
                    if widened != header {
                        header = widened;
                        continue;
                    }
                    // Done: after the loop, the variables have these types.
                    for (&l, &t) in carried.iter().zip(&header) {
                        cx.local_tys[l] = t;
                    }
                    break TStmt::Loop {
                        pos: pos.map(|(p, _)| p),
                        var: *var,
                        source: Box::new(source),
                        borrowed: *borrowed,
                        body: checked,
                        carried: carried.iter().copied().zip(header).collect(),
                        inner: inner.clone(),
                    };
                }
            }
        })
    }

    /// The clauses of a list comprehension, in order: each generator gives
    /// its variables their types for the clauses after it.
    fn clauses(&mut self, cx: &mut Ctx, clauses: &[HClause]) -> Result<Vec<TClause>, Diagnostic> {
        let mut out = Vec::new();
        for c in clauses {
            out.push(match c {
                HClause::Gen {
                    pos,
                    var,
                    source,
                    borrowed,
                } => {
                    let (source, var_ty) = self.loop_source(cx, source)?;
                    cx.local_tys[*var] = var_ty;
                    if let Some(p) = pos {
                        cx.local_tys[*p] = Ty::INT;
                    }
                    TClause::Gen {
                        pos: *pos,
                        var: *var,
                        source,
                        borrowed: *borrowed,
                    }
                }
                HClause::Guard(g) => {
                    let g = self.expr(cx, g)?;
                    TClause::Guard(self.narrow(g, Ty::BOOL, || {
                        "a condition in a list comprehension must be a boolean".into()
                    })?)
                }
            });
        }
        Ok(out)
    }

    /// What a loop or comprehension goes through, and the type of its
    /// variable: an int for a range, the element type for an array, a
    /// (one-character) string for a string.
    fn loop_source(
        &mut self,
        cx: &mut Ctx,
        source: &HExpr,
    ) -> Result<(LoopSource, Ty), Diagnostic> {
        if let HExprKind::Range(start, end) = &source.kind {
            let start = self.expr(cx, start)?;
            let start = self.narrow(start, Ty::INT, || {
                "the start of a range must be an int".into()
            })?;
            let end = self.expr(cx, end)?;
            let end = self.narrow(end, Ty::INT, || "the end of a range must be an int".into())?;
            return Ok((LoopSource::Range(start, end), Ty::INT));
        }
        let e = self.expr(cx, source)?;
        let e = self.narrow(e, Ty::ITERABLE, || {
            "a loop goes through a range, an array or a string".into()
        })?;
        let var_ty = match e.ty.as_kind() {
            Some(Kind::Array) => e.ty.elem().unwrap_or(Ty::NEVER),
            Some(Kind::Str) => Ty::STR,
            _ if e.ty.is_never() => Ty::NEVER,
            _ => {
                return Err(err(
                    e.span,
                    format!(
                        "a loop goes through an array or a string, but this may be either ({})",
                        e.ty
                    ),
                ));
            }
        };
        Ok((LoopSource::Each(e), var_ty))
    }

    /// The type of `[items...]`: all elements have the same type (a value
    /// that may be void counts as the same type: `[1, head a]` is `[int?]`).
    fn array_literal(&mut self, items: Vec<TExpr>) -> Result<(TExprKind, Ty), Diagnostic> {
        let mut elem = Ty::NEVER;
        let mut first: Option<&TExpr> = None;
        for item in &items {
            elem = elem.union(item.ty);
            // A void element fits any element type (`[void, 2]` is `[int?]`).
            if item.ty.without(Ty::VOID).is_never() {
                continue;
            }
            match first {
                None => first = Some(item),
                // Same type, ignoring void; an empty array `[]` fits any
                // array type (`[[1], []]` is `[[int]]`).
                Some(f)
                    if !item.ty.without(Ty::VOID).is_subset(f.ty.without(Ty::VOID))
                        && !f.ty.without(Ty::VOID).is_subset(item.ty.without(Ty::VOID)) =>
                {
                    return Err(err(
                        item.span,
                        format!(
                            "the elements of an array must all have the same type, but this is {} and the first one is {}",
                            item.ty, f.ty
                        ),
                    )
                    .with_note(f.span, "the first element"));
                }
                Some(_) => {}
            }
        }
        let items = items.into_iter().map(|i| convert(i, elem)).collect();
        Ok((TExprKind::Array(items), Ty::array(elem)))
    }

    /// `a ++ b`: two arrays with elements of the same type; void flows
    /// through.
    fn concat(&mut self, a: TExpr, b: TExpr, span: Span) -> Result<(TExprKind, Ty), Diagnostic> {
        for e in [&a, &b] {
            if !e.ty.is_subset(Ty::ARRAY.union(Ty::VOID)) {
                return Err(err(
                    e.span,
                    format!("`++` joins two arrays, but this is {}", e.ty),
                ));
            }
        }
        let mut ty = Ty::NEVER;
        if a.ty.contains(Kind::Void) || b.ty.contains(Kind::Void) {
            ty = Ty::VOID;
        }
        if let (Some(ea), Some(eb)) = (a.ty.elem(), b.ty.elem()) {
            let same =
                ea.is_never() || eb.is_never() || ea.without(Ty::VOID) == eb.without(Ty::VOID);
            if !same {
                return Err(err(
                    span,
                    format!(
                        "`++` joins arrays whose elements have the same type, but these are {} and {}",
                        a.ty, b.ty
                    ),
                ));
            }
            ty = ty.union(Ty::array(ea.union(eb)));
        }
        Ok((TExprKind::Concat(Box::new(a), Box::new(b)), ty))
    }

    /// `a ** b`: two arrays of numbers multiplied element by element; the
    /// longer array's extra elements are kept as they are. The elements of
    /// the result are what `*` gives for each pair, or what the longer array
    /// holds; void flows through.
    fn elem_mul(&mut self, a: TExpr, b: TExpr) -> Result<(TExprKind, Ty), Diagnostic> {
        let numbers = Ty::array(Ty::NUMERIC.union(Ty::VOID)).union(Ty::VOID);
        for e in [&a, &b] {
            if !e.ty.is_subset(numbers) {
                return Err(err(
                    e.span,
                    format!(
                        "`**` multiplies two arrays of numbers, but this is {}",
                        e.ty
                    ),
                ));
            }
        }
        let mut ty = Ty::NEVER;
        if a.ty.contains(Kind::Void) || b.ty.contains(Kind::Void) {
            ty = Ty::VOID;
        }
        if let (Some(ea), Some(eb)) = (a.ty.elem(), b.ty.elem()) {
            // pairs, then the leftovers of either array
            let mut elem = ea.union(eb);
            for ka in ea.kinds() {
                for kb in eb.kinds() {
                    if let Some(k) = binop_result(BinOp::Mul, ka, kb) {
                        elem = elem.union(Ty::of(k));
                    }
                }
            }
            ty = ty.union(Ty::array(elem));
        }
        Ok((TExprKind::ElemMul(Box::new(a), Box::new(b)), ty))
    }

    /// `a !! i`: an element (or a character), or void when `i` is out of
    /// range; void flows through.
    fn index(&mut self, a: TExpr, i: TExpr) -> Result<(TExprKind, Ty), Diagnostic> {
        if !a.ty.is_subset(Ty::ITERABLE.union(Ty::VOID)) {
            return Err(err(
                a.span,
                format!(
                    "`!!` takes an element of an array or a string, but this is {}",
                    a.ty
                ),
            ));
        }
        if !i.ty.is_subset(Ty::INT.union(Ty::VOID)) {
            return Err(err(
                i.span,
                format!("the position in `!!` must be an int, but this is {}", i.ty),
            ));
        }
        let mut ty = Ty::VOID;
        if let Some(elem) = a.ty.elem() {
            ty = ty.union(elem);
        }
        if a.ty.contains(Kind::Str) {
            ty = ty.union(Ty::STR);
        }
        Ok((TExprKind::Index(Box::new(a), Box::new(i)), ty))
    }

    /// Convert `e` to `e.ty ∩ target`, inserting a runtime check if needed.
    ///
    /// Three cases: the value certainly fits (no code needed), certainly
    /// cannot fit (compile-time error), or might fit (runtime check). This is
    /// *gradual typing*: annotations are checked statically where possible
    /// and dynamically where the compiler cannot know.
    fn narrow(
        &self,
        e: TExpr,
        target: Ty,
        what: impl FnOnce() -> String,
    ) -> Result<TExpr, Diagnostic> {
        if e.ty.is_subset(target) {
            return Ok(e);
        }
        // A value that may be void cannot go where void is not accepted:
        // the program must handle the void case first.
        if e.ty.is_maybe() && !target.contains(Kind::Void) {
            return Err(err(
                e.span,
                format!(
                    "{}, but this is {}, which may be void: handle the void case first, e.g. with a `check` that has a `void ->` arm",
                    what(),
                    e.ty
                ),
            ));
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
            HExprKind::Void => (TExprKind::Void, Ty::VOID),
            HExprKind::Local(l) => (TExprKind::Local(*l), cx.local_tys[*l]),
            HExprKind::Call { name, args } => return self.call(cx, name, args, span),
            HExprKind::Binary {
                op: op @ (BinOp::Concat | BinOp::Index | BinOp::ElemMul),
                lhs,
                rhs,
            } => {
                let lhs = self.expr(cx, lhs)?;
                let rhs = self.expr(cx, rhs)?;
                if lhs.ty.is_never() || rhs.ty.is_never() {
                    return Ok(TExpr {
                        kind: TExprKind::Diverge(vec![lhs, rhs]),
                        ty: Ty::NEVER,
                        span,
                    });
                }
                if *op == BinOp::Concat {
                    self.concat(lhs, rhs, span)?
                } else if *op == BinOp::ElemMul {
                    self.elem_mul(lhs, rhs)?
                } else {
                    self.index(lhs, rhs)?
                }
            }
            HExprKind::Ret(value) => {
                if cx.in_comprehension > 0 {
                    return Err(err(
                        span,
                        "`ret` cannot leave the function from inside a list comprehension",
                    ));
                }
                let value = match value {
                    Some(v) => {
                        let mut v = self.expr(cx, v)?;
                        if let Some(declared) = cx.declared
                            && declared != Ty::VOID
                        {
                            v = self.narrow(v, declared, || {
                                format!("the function is declared to return {declared}")
                            })?;
                        }
                        Some(v)
                    }
                    None => None,
                };
                if !cx.discard {
                    cx.ret_ty = cx.ret_ty.union(value.as_ref().map_or(Ty::VOID, |v| v.ty));
                }
                // The expression itself never produces a value: control
                // leaves the function.
                (TExprKind::Ret(value.map(Box::new)), Ty::NEVER)
            }
            HExprKind::Builtin(b, arg) => {
                let arg = self.expr(cx, arg)?;
                if arg.ty.is_never() {
                    return Ok(TExpr {
                        kind: TExprKind::Diverge(vec![arg]),
                        ty: Ty::NEVER,
                        span,
                    });
                }
                // `copy` takes a value of any type and gives one of the same.
                if *b == Builtin::Copy {
                    let ty = arg.ty;
                    return Ok(TExpr {
                        kind: TExprKind::Builtin(*b, Box::new(arg)),
                        ty,
                        span,
                    });
                }
                if !arg.ty.is_subset(Ty::ITERABLE.union(Ty::VOID)) {
                    return Err(err(
                        arg.span,
                        format!(
                            "`{}` takes an array or a string, but this is {}",
                            b.name(),
                            arg.ty
                        ),
                    ));
                }
                // Void flows through; an empty array or string has no
                // element, so `head`, `tail`, `init` and `last` may give void.
                let mut ty = if arg.ty.contains(Kind::Void) {
                    Ty::VOID
                } else {
                    Ty::NEVER
                };
                if let Some(elem) = arg.ty.elem() {
                    ty = ty.union(match b {
                        Builtin::Len => Ty::INT,
                        Builtin::Head | Builtin::Last => elem.union(Ty::VOID),
                        Builtin::Tail | Builtin::Init => Ty::array(elem).union(Ty::VOID),
                        Builtin::Reverse => Ty::array(elem),
                        Builtin::Copy => unreachable!("typed above"),
                    });
                }
                if arg.ty.contains(Kind::Str) {
                    ty = ty.union(match b {
                        Builtin::Len => Ty::INT,
                        Builtin::Reverse => Ty::STR,
                        _ => Ty::STR.union(Ty::VOID),
                    });
                }
                (TExprKind::Builtin(*b, Box::new(arg)), ty)
            }
            HExprKind::Array(items) => {
                let items = items
                    .iter()
                    .map(|i| self.expr(cx, i))
                    .collect::<Result<Vec<_>, _>>()?;
                self.array_literal(items)?
            }
            HExprKind::Range(start, end) => {
                let start = self.expr(cx, start)?;
                let start = self.narrow(start, Ty::INT, || {
                    "the start of a range must be an int".into()
                })?;
                let end = self.expr(cx, end)?;
                let end =
                    self.narrow(end, Ty::INT, || "the end of a range must be an int".into())?;
                (
                    TExprKind::Range(Box::new(start), Box::new(end)),
                    Ty::array(Ty::INT),
                )
            }
            HExprKind::Comprehension { body, clauses } => {
                cx.in_comprehension += 1;
                let checked = self
                    .clauses(cx, clauses)
                    .and_then(|clauses| Ok((clauses, self.expr(cx, body)?)));
                cx.in_comprehension -= 1;
                let (clauses, body) = checked?;
                let ty = Ty::array(body.ty);
                (
                    TExprKind::Comprehension {
                        body: Box::new(body),
                        clauses,
                    },
                    ty,
                )
            }
            HExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.expr(cx, lhs)?;
                let rhs = self.expr(cx, rhs)?;
                // If either side never produces a value, neither does the
                // operation. Without this rule, round 1 of the fixpoint would
                // report false "cannot add never and int" errors.
                let ty = if lhs.ty.is_never() || rhs.ty.is_never() {
                    Ty::NEVER
                } else {
                    // Try every pair of possible kinds and collect the
                    // results. `int|float + int` gives `int ∪ float`. It is
                    // an error only if *no* pair is valid; pairs that are
                    // invalid but possible become runtime errors instead.
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
                    // Nothing flows through: negating void gives void.
                    let ty = inner.ty.intersect(Ty::NUMERIC.union(Ty::VOID));
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
                // Handling the void case: when the checked value is a
                // variable of a maybe type (`int?`), an arm whose pattern is
                // not void only runs when the variable is not void, so in
                // that arm it has the plain type (`int`). After a `void ->`
                // arm, the same holds for `else`.
                let narrow_local = match scrutinee.kind {
                    TExprKind::Local(l) if scrutinee.ty.is_maybe() => Some(l),
                    _ => None,
                };
                let narrowed_ty = scrutinee.ty.without(Ty::VOID);
                let mut narrowed_arms = Vec::new();
                let mut tarms = Vec::new();
                let mut ty = Ty::NEVER;
                let mut seen_true = false;
                let mut seen_false = false;
                let mut seen_void = false;
                let arm_body = |this: &mut Self, cx: &mut Ctx, body: &HExpr, narrowed: bool| {
                    let saved = narrow_local.map(|l| cx.local_tys[l]);
                    if let (Some(l), true) = (narrow_local, narrowed) {
                        cx.local_tys[l] = narrowed_ty;
                    }
                    let body = this.expr(cx, body);
                    if let (Some(l), Some(t)) = (narrow_local, saved) {
                        cx.local_tys[l] = t;
                    }
                    body
                };
                for (pat, body) in arms {
                    let pat = self.expr(cx, pat)?;
                    match pat.kind {
                        TExprKind::Bool(true) => seen_true = true,
                        TExprKind::Bool(false) => seen_false = true,
                        TExprKind::Void => seen_void = true,
                        _ => {}
                    }
                    let narrowed = narrow_local.is_some() && !pat.ty.contains(Kind::Void);
                    let body = arm_body(self, cx, body, narrowed)?;
                    ty = ty.union(body.ty);
                    narrowed_arms.push(narrowed);
                    tarms.push((pat, body));
                }
                let else_narrowed = narrow_local.is_some() && seen_void;
                let else_arm = match else_arm {
                    Some(e) => {
                        let e = arm_body(self, cx, e, else_narrowed)?;
                        ty = ty.union(e.ty);
                        Some(e)
                    }
                    None => {
                        // Exhaustiveness check: without `else`, some value
                        // could match no arm and the `check` would have no
                        // result. Only booleans (`true`, `false`) and void
                        // can be covered completely, since there are
                        // infinitely many ints and strings.
                        let covered = |k: Kind| match k {
                            Kind::Bool => seen_true && seen_false,
                            Kind::Void => seen_void,
                            _ => false,
                        };
                        if !scrutinee.ty.kinds().all(covered) {
                            let hint = if scrutinee.ty.contains(Kind::Void) && !seen_void {
                                " (or a `void ->` arm for the void case)"
                            } else {
                                ""
                            };
                            return Err(err(
                                span,
                                format!(
                                    "`check` on a value of type {} needs an `else` branch to cover all values{hint}",
                                    scrutinee.ty
                                ),
                            ));
                        }
                        None
                    }
                };
                let narrowing = narrow_local.map(|local| Narrowing {
                    local,
                    ty: narrowed_ty,
                    arms: narrowed_arms,
                    else_arm: else_narrowed,
                });
                // All arms must produce the same representation, because
                // after the `check` the code continues from one place with
                // one result. Arms whose type is narrower than the union get
                // converted (boxed).
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
                        narrowing,
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
        // Overload resolution: definitions are tried from top to bottom, and
        // the first one whose parameter types accept the arguments is
        // called. Usually the argument types decide this at compile time.
        // But a mixed type such as `int|string` is only resolved at run
        // time: a definition taking `numeric` accepts some of its values and
        // not others. Then the program tests the value, trying the
        // definitions in the same order, until one accepts every possible
        // value (or none is left and the call fails at run time).
        let hir = self.hir;
        let candidates: Vec<FuncId> = hir.groups[name]
            .iter()
            .copied()
            .filter(|&f| hir.funcs[f].params.len() == args.len())
            .collect();
        // What each argument may still be when a definition is tried: a
        // definition that tests a single argument leaves the next ones only
        // the values it rejected.
        let mut remaining: Vec<Ty> = args.iter().map(|a| a.ty).collect();
        // Which array lengths earlier patterns accepted, per argument: an
        // empty array (`[]`), and arrays of at least n elements (`(x:xs)`).
        let mut empty_covered = vec![false; args.len()];
        let mut from_covered: Vec<Option<usize>> = vec![None; args.len()];
        let mut cases: Vec<Case> = Vec::new();
        let mut exhaustive = false;
        for &f in &candidates {
            let func = &hir.funcs[f];
            let params = &func.param_tys;
            if remaining
                .iter()
                .zip(params)
                .any(|(a, t)| a.intersect(*t).is_never())
            {
                continue; // cannot accept these arguments
            }
            let tests: Vec<Option<Ty>> = remaining
                .iter()
                .zip(params)
                .map(|(a, t)| (!a.is_subset(*t)).then_some(*t))
                .collect();
            // A pattern also tests the array's length, at run time.
            let lens: Vec<Option<LenTest>> = func
                .patterns
                .iter()
                .map(|p| match p {
                    Some(HPattern::Cons { heads, .. }) => Some(LenTest::AtLeast(heads.len())),
                    Some(HPattern::Empty) => Some(LenTest::Empty),
                    None => None,
                })
                .collect();
            let tested: Vec<usize> = (0..tests.len())
                .filter(|&j| tests[j].is_some() || lens[j].is_some())
                .collect();
            let receives = remaining
                .iter()
                .zip(params)
                .map(|(a, t)| a.intersect(*t))
                .collect();
            if let [j] = tested[..] {
                // Only argument `j` decides whether this definition is taken,
                // so the next definitions only see the values it rejects.
                match lens[j] {
                    None => remaining[j] = remaining[j].without(params[j]),
                    Some(test) => {
                        match test {
                            LenTest::Empty => empty_covered[j] = true,
                            LenTest::AtLeast(n) => {
                                from_covered[j] = Some(from_covered[j].map_or(n, |m| m.min(n)))
                            }
                        }
                        // `[]` and `(x:xs)` together accept every array.
                        let all_arrays = from_covered[j] == Some(0)
                            || (empty_covered[j] && from_covered[j] == Some(1));
                        if all_arrays {
                            remaining[j] = remaining[j].without(Ty::ARRAY);
                        }
                    }
                }
            }
            let done = tested.is_empty() || remaining.iter().any(|r| r.is_never());
            cases.push(Case {
                f,
                tests,
                lens,
                receives,
            });
            if done {
                exhaustive = true;
                break;
            }
        }
        // A value that may be void must reach a definition that accepts void
        // (one with an unannotated or `any` parameter); otherwise the
        // program has to handle the void case before the call.
        for (j, a) in args.iter().enumerate() {
            if a.ty.is_maybe()
                && !cases.is_empty()
                && cases.iter().all(|c| !c.receives[j].contains(Kind::Void))
            {
                return Err(err(
                    a.span,
                    format!(
                        "argument {} of `{name}` is {}, which may be void, but `{name}` does not accept void: handle the void case first, e.g. with a `check` that has a `void ->` arm",
                        j + 1,
                        a.ty
                    ),
                ));
            }
        }
        let chosen = match cases.len() {
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
            // One definition: call it directly. If it accepts only some of
            // the values, the argument conversion below checks the value at
            // run time and names the expected type if it does not fit.
            // A pattern must be tested at run time, even with one definition.
            1 if cases[0].lens.iter().all(Option::is_none) => cases[0].f,
            _ => return self.dispatch(name, args, cases, exhaustive, span),
        };

        // Narrow each argument to what the parameter accepts (a runtime
        // check where needed), then instantiate the callee for exactly
        // those types. Its current return type may still be `never` if it
        // is recursive; the fixpoint loop will come back to this call.
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

    /// A call decided at run time: test the arguments against the
    /// definitions in `cases`, in order, and call the first that accepts
    /// them.
    fn dispatch(
        &mut self,
        name: &str,
        args: Vec<TExpr>,
        cases: Vec<Case>,
        exhaustive: bool,
        span: Span,
    ) -> Result<TExpr, Diagnostic> {
        let mut ty = Ty::NEVER;
        let mut tcases = Vec::new();
        // Each instance receives the arguments narrowed to what reaches it.
        for c in cases {
            let inst = self.instantiate(c.f, c.receives.clone());
            ty = ty.union(self.instances[inst].ret);
            tcases.push(DispatchCase {
                tests: c.tests,
                lens: c.lens,
                params: c.receives,
                inst,
            });
        }
        Ok(TExpr {
            kind: TExprKind::Dispatch {
                name: name.to_string(),
                args,
                cases: tcases,
                exhaustive,
            },
            ty,
            span,
        })
    }

    /// Keep only instances reachable from the entry and assign symbols.
    ///
    /// Early fixpoint rounds can create instances that the final types no
    /// longer call. A breadth-first walk of the call graph from `main` finds
    /// the live ones; the rest are dropped (dead code elimination at the
    /// function level). Each instance gets a unique symbol, because
    /// `sum(int, int)` and `sum(float, int)` become two machine-code
    /// functions and the linker needs a distinct name for each.
    fn finish(mut self, entry: InstId) -> Tir {
        let mut order = vec![entry];
        let mut index: HashMap<InstId, usize> = HashMap::from([(entry, 0)]);
        let mut i = 0;
        while i < order.len() {
            let mut callees = Vec::new();
            for_each_expr(&self.instances[order[i]].body, &mut |e| {
                collect_calls(e, &mut callees)
            });
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
            // A linked file's definitions are named after the file
            // (`entity/try.pred`); symbols use `.` instead of `/`.
            let base = func.name.replace('/', ".");
            let symbol = if count[func.name.as_str()] == 1 {
                base
            } else {
                let n = seen.entry(func.name.clone()).or_default();
                *n += 1;
                format!("{base}.{n}")
            };
            let mut body = std::mem::take(&mut inst.body);
            for_each_expr_mut(&mut body, &mut |e| remap_calls(e, &index));
            funcs.push(TFunc {
                name: func.name.clone(),
                symbol,
                params: inst.params.clone(),
                ret: inst.ret,
                discard: func.ret == Some(Ty::VOID),
                body,
                local_tys: std::mem::take(&mut inst.local_tys),
                local_names: func.local_names.clone(),
                patterns: func.patterns.clone(),
                borrowed: func.borrowed.clone(),
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
        | TExprKind::Void
        | TExprKind::Local(_) => {}
        TExprKind::Call { args, .. }
        | TExprKind::Dispatch { args, .. }
        | TExprKind::Diverge(args) => args.iter().for_each(f),
        TExprKind::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        TExprKind::Neg(x)
        | TExprKind::Echo(x)
        | TExprKind::Convert(x)
        | TExprKind::Builtin(_, x) => f(x),
        TExprKind::Array(items) => items.iter().for_each(f),
        TExprKind::Ret(value) => value.iter().for_each(|v| f(v)),
        TExprKind::Range(a, b)
        | TExprKind::Concat(a, b)
        | TExprKind::Index(a, b)
        | TExprKind::ElemMul(a, b) => {
            f(a);
            f(b);
        }
        TExprKind::Comprehension { body, clauses } => {
            clauses.iter().flat_map(TClause::exprs).for_each(&mut *f);
            f(body);
        }
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
            ..
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
    match &e.kind {
        TExprKind::Call { inst, .. } => out.push(*inst),
        TExprKind::Dispatch { cases, .. } => out.extend(cases.iter().map(|c| c.inst)),
        _ => {}
    }
    visit_children(e, &mut |c| collect_calls(c, out));
}

fn remap_calls(e: &mut TExpr, index: &HashMap<InstId, usize>) {
    match &mut e.kind {
        TExprKind::Call { inst, args } => {
            *inst = index[inst];
            args.iter_mut().for_each(|a| remap_calls(a, index));
        }
        TExprKind::Dispatch { args, cases, .. } => {
            for c in cases {
                c.inst = index[&c.inst];
            }
            args.iter_mut().for_each(|a| remap_calls(a, index));
        }
        TExprKind::Diverge(args) => args.iter_mut().for_each(|a| remap_calls(a, index)),
        TExprKind::Binary { lhs, rhs, .. } => {
            remap_calls(lhs, index);
            remap_calls(rhs, index);
        }
        TExprKind::Neg(x)
        | TExprKind::Echo(x)
        | TExprKind::Convert(x)
        | TExprKind::Builtin(_, x) => remap_calls(x, index),
        TExprKind::Array(items) => items.iter_mut().for_each(|i| remap_calls(i, index)),
        TExprKind::Ret(value) => value.iter_mut().for_each(|v| remap_calls(v, index)),
        TExprKind::Range(a, b)
        | TExprKind::Concat(a, b)
        | TExprKind::Index(a, b)
        | TExprKind::ElemMul(a, b) => {
            remap_calls(a, index);
            remap_calls(b, index);
        }
        TExprKind::Comprehension { body, clauses } => {
            clauses
                .iter_mut()
                .flat_map(TClause::exprs_mut)
                .for_each(|e| remap_calls(e, index));
            remap_calls(body, index);
        }
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
            ..
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
