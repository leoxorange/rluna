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
use crate::ast::IntrinsicRet;
use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::hir::{FieldDefault, FuncId, HClause, HExpr, HExprKind, HPattern, HStmt, Hir, Native};
use crate::types::{Kind, Ty};

pub type InstId = usize;

#[derive(Debug)]
pub struct Tir {
    pub funcs: Vec<TFunc>,
    pub entry: InstId,
    /// The fields of every trail, with their final types, by global name.
    pub trails: HashMap<String, TTrail>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct TTrail {
    /// The name to show for it (`Fruit`).
    pub name: String,
    pub fields: Vec<(String, Ty)>,
    /// It follows Show: it is printed as its `textualize` field.
    pub shows: bool,
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
    /// A `native` function: the runtime function that gives it. The body
    /// is empty.
    pub native: Option<Native>,
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
        source: Box<LoopSource>,
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
    /// A variable's value belongs to the variable. `a !! i` gives the
    /// element itself, still inside `a`, rather than a copy of it: Luna
    /// never copies a string or an array without being asked to (`copy`).
    /// Elements of `Copy` types are copied as usual, and so are the
    /// characters of a string, which are new strings anyway.
    pub fn is_borrowed(&self) -> bool {
        match &self.kind {
            TExprKind::Local(_) | TExprKind::Str(_) => true,
            TExprKind::Index(a, _) => gives_element_itself(a.ty),
            // A field is like an element: itself, unless it is a number.
            TExprKind::Field(..) => !self.ty.is_copy(),
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
            TExprKind::Index(a, _) if gives_element_itself(a.ty) => a.borrowed_from(),
            TExprKind::Field(base, _) if !self.ty.is_copy() => base.borrowed_from(),
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
    /// `value :? Trail`, which only reads the value. The type is the part
    /// of the value's type that follows the trail: the value passes when
    /// its kind is one of these.
    Is(Box<TExpr>, Ty),
    /// Each arm compares the scrutinee to the pattern with `==`.
    /// Arm bodies are already converted to the result type.
    Check {
        scrutinee: Box<TExpr>,
        arms: Vec<(TExpr, TExpr)>,
        else_arm: Option<Box<TExpr>>,
        /// Arms where the checked variable cannot be void.
        narrowing: Option<Narrowing>,
        /// For each arm whose pattern is a value of a trail, the instance of
        /// that trail's `compare`: the arm matches when it gives 0.
        compares: Vec<Option<InstId>>,
        /// Variables from outside that the arms give new values (in the
        /// blocks of `if`s), with their type after the `check`: the union
        /// of what the arms leave in them.
        carried: Vec<(usize, Ty)>,
    },
    /// The block of an `if` or an `else`: the value of its last statement
    /// if that is an expression, else void.
    Block {
        stmts: Vec<TStmt>,
        /// Variables that exist only inside the block.
        inner: Vec<usize>,
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
    /// `over`: leave the innermost loop.
    Over,
    /// `jump`: go on with the innermost loop's next round.
    Jump,
    /// `a ++ b`: both operands are arrays (or void), which it only reads.
    Concat(Box<TExpr>, Box<TExpr>),
    /// `a !! i`: an element of an array or a character of a string, or void
    /// if `i` is out of range.
    Index(Box<TExpr>, Box<TExpr>),
    /// `a ** b`: element by element products of two arrays, which it only
    /// reads.
    ElemMul(Box<TExpr>, Box<TExpr>),
    /// A value of a trail (see `hir::HExprKind::Construct`): each step gives
    /// a variable a field's value, already converted to the field's type;
    /// `slots` are the variables of the fields, in order.
    Construct {
        trail: String,
        steps: Vec<(usize, TExpr)>,
        slots: Vec<usize>,
    },
    /// Field number `.1` of a value of a trail (or void, if the value may
    /// be void and is).
    Field(Box<TExpr>, usize),
    /// `x.textualize` for an embedded value (`[1, 2].textualize` is
    /// `"[1, 2]"`): what `echo` would print, as a new string.
    Textualize(Box<TExpr>),
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
        // Typed with their element types, see `Checker::concat` etc., and
        // `&&` / `||` as `check`s (`Checker::logic`).
        BinOp::Concat | BinOp::Index | BinOp::ElemMul | BinOp::And | BinOp::Or => None,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => match (a, b) {
            (Int, Int) if op != BinOp::Div => Some(Int),
            _ if numeric(a) && numeric(b) => Some(Float),
            (Str, Str) if op == BinOp::Add => Some(Str),
            _ => None,
        },
        // Only whole numbers have a remainder.
        BinOp::Mod => match (a, b) {
            (Int, Int) => Some(Int),
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
        warnings: Vec::new(),
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
        // Only the last round's warnings count, as for errors.
        tc.warnings.clear();
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
    warnings: Vec<Diagnostic>,
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
    /// The declared result is a trail the result must follow (`-> Fruit`
    /// accepts a trail that follows Fruit), instead of `declared`.
    ret_bound: Option<String>,
    /// The function is declared `-> void`: `ret` values are discarded.
    discard: bool,
    /// How many list comprehensions enclose the current expression.
    in_comprehension: usize,
    /// The loops being checked, innermost last: the variables each carries,
    /// with the types they have had at an `over` or a `jump` so far (those
    /// reach the top of the loop or its end too).
    loop_exits: Vec<(Vec<usize>, Vec<Ty>)>,
}

fn err(span: Span, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(ErrorKind::TypeError, span, msg)
}

/// `a ∪ b`, unless that mixes values of two trails, which one value cannot
/// be (for now).
fn join(a: Ty, b: Ty, span: Span, what: &str) -> Result<Ty, Diagnostic> {
    if a.clashes(b) {
        return Err(err(
            span,
            format!(
                "{what} give {a} and {b}, but a value has one trail at a time (mixing trails is not supported yet)"
            ),
        ));
    }
    Ok(a.union(b))
}

/// `infinite` as the end of a range: `[0..infinite]`.
fn is_infinite(end: &HExpr) -> bool {
    matches!(end.kind, HExprKind::Float(v) if v == f64::INFINITY)
}

/// `[0..infinite]` anywhere but as what a loop goes through.
fn endless_range(span: Span) -> Diagnostic {
    err(
        span,
        "a range that never ends (`[0..infinite]`) can only be what a `loop` goes through: an array or a list comprehension would never be finished",
    )
}

/// A side of `&&` or `||` that is not a boolean.
fn logic_error(op: BinOp, ty: Ty) -> String {
    let hint = if ty.is_maybe() {
        ": it may be void, so handle the void case first, with a `check` that has a `void ->` arm"
    } else {
        ""
    };
    format!(
        "`{}` needs a Boolean on each side, but this is {ty}{hint}",
        op.as_str()
    )
}

/// The name of a trail to show: `food.Fruit`, without the folders.
pub fn shown_trail(global: &str) -> String {
    global.rsplit('/').next().unwrap_or(global).to_string()
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
        // A native function gives what it declares; the runtime or the
        // compiler does the rest. One the compiler gives may give a value
        // of the type it is called on (`slice`), or one of its elements
        // (`at`, a character of a string).
        if let Some(native) = &func.native {
            let ret = match native {
                Native::Intrinsic(i) if i.ret == IntrinsicRet::Receiver => params[0],
                Native::Intrinsic(i) if i.ret == IntrinsicRet::Element => {
                    params[0].elem().unwrap_or(Ty::STR)
                }
                _ => func.ret.unwrap_or(Ty::VOID),
            };
            return Ok((Vec::new(), local_tys, ret));
        }
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
            declared: if func.ret_bound.is_some() {
                None
            } else {
                func.ret
            },
            ret_bound: func.ret_bound.clone(),
            discard: func.ret == Some(Ty::VOID),
            in_comprehension: 0,
            loop_exits: Vec::new(),
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
            if let Some(bound) = &func.ret_bound {
                v = self.narrow(v, self.accepted(bound), || {
                    format!("`{}` is declared to return {bound}", func.name)
                })?;
                self.bound(&v, bound, || {
                    format!("`{}` is declared to return {bound}", func.name)
                })?;
                result = v.ty;
            } else if let Some(declared) = func.ret
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
        result = join(
            result,
            cx.ret_ty,
            func.name_span,
            "the results of this function",
        )?;
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
                    cx.loop_exits
                        .push((carried.clone(), vec![Ty::NEVER; carried.len()]));
                    let checked = self.stmts(cx, body);
                    let (_, exits) = cx.loop_exits.pop().unwrap();
                    if pos.is_some() {
                        cx.loop_vars.pop();
                    }
                    cx.loop_vars.pop();
                    let checked = checked?;
                    let widened: Vec<Ty> = carried
                        .iter()
                        .zip(&header)
                        .zip(&exits)
                        .map(|((&l, &t), &exit)| {
                            let what = "the values a variable gets in this loop";
                            join(
                                join(t, cx.local_tys[l], *var_span, what)?,
                                exit,
                                *var_span,
                                what,
                            )
                        })
                        .collect::<Result<_, _>>()?;
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
                        source: Box::new(source),
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
            // `[0..infinite]` never ends: a loop goes on until something
            // leaves it (`over`, `ret`). The count stops at the largest
            // int, 2^63 - 1 steps away: centuries, in practice never.
            if is_infinite(end) {
                if cx.in_comprehension > 0 {
                    return Err(endless_range(source.span));
                }
                let end = TExpr {
                    kind: TExprKind::Int(i64::MAX),
                    ty: Ty::INT,
                    span: end.span,
                };
                return Ok((LoopSource::Range(start, end), Ty::INT));
            }
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
            elem = join(elem, item.ty, item.span, "the elements of this array")?;
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

    /// The function group of the function `name` of the trail of `ty`,
    /// for a call with `nargs` arguments after the value.
    fn method_of(
        &self,
        ty: Ty,
        name: &str,
        span: Span,
        nargs: usize,
    ) -> Result<String, Diagnostic> {
        let Some(trail) = ty.trail().filter(|_| ty.as_kind() == Some(Kind::Record)) else {
            let hint = if ty.is_maybe() {
                ", which may be void: handle the void case first, e.g. with a `check` that has a `void ->` arm"
            } else {
                ""
            };
            return Err(err(
                span,
                format!("`.{name}` calls a function of a trail's value, but this is {ty}{hint}"),
            ));
        };
        let t = &self.hir.trails[trail];
        let shown = shown_trail(trail);
        let Some(m) = t.methods.iter().find(|m| m.name == name) else {
            let msg = if t.fields.iter().any(|f| f.name == name) {
                format!("`{name}` is a field of `{shown}`, not a function")
            } else {
                format!("`{shown}` has no function `{name}`")
            };
            return Err(err(span, msg));
        };
        if !m.shapes.iter().any(|s| s.len() == nargs) {
            let counts: Vec<String> = m.shapes.iter().map(|s| s.len().to_string()).collect();
            return Err(err(
                span,
                format!(
                    "`{name}` of `{shown}` takes {} argument(s), but it is given {nargs}",
                    counts.join(" or ")
                ),
            ));
        }
        m.func.clone().ok_or_else(|| {
            err(
                span,
                format!(
                    "`{shown}` does not give the function `{name}`, which `{}` asks for",
                    shown_trail(&m.from)
                ),
            )
        })
    }

    /// `a op b` on values of trails: the trail's function behind the
    /// operator, called on `a` with `b` (both only read). Both must be
    /// values of the same trail, for now.
    fn trail_operator(
        &mut self,
        op: BinOp,
        lhs: TExpr,
        rhs: TExpr,
        span: Span,
    ) -> Result<TExpr, Diagnostic> {
        if lhs.ty.is_never() || rhs.ty.is_never() {
            return Ok(TExpr {
                kind: TExprKind::Diverge(vec![lhs, rhs]),
                ty: Ty::NEVER,
                span,
            });
        }
        let (method, needs) = match op {
            BinOp::Add => ("add", "Numeric"),
            BinOp::Sub => ("sub", "Numeric"),
            BinOp::Mul => ("mul", "Numeric"),
            BinOp::Div => ("div", "Numeric"),
            BinOp::Mod => ("mod", "Numeric"),
            BinOp::Concat | BinOp::Index | BinOp::ElemMul | BinOp::And | BinOp::Or => {
                unreachable!("typed separately")
            }
            _ => ("compare", "Compare"),
        };
        let single = |t: Ty| t.as_kind() == Some(Kind::Record);
        if !single(lhs.ty) || !single(rhs.ty) {
            let msg = if lhs.ty.is_maybe() || rhs.ty.is_maybe() {
                format!(
                    "`{}` cannot be applied to {} and {}: a value that may be void must be handled first",
                    op.as_str(),
                    lhs.ty,
                    rhs.ty
                )
            } else {
                format!(
                    "`{}` cannot be applied to {} and {}: on values of a trail it needs two of the same trail",
                    op.as_str(),
                    lhs.ty,
                    rhs.ty
                )
            };
            return Err(err(span, msg));
        }
        let trail = lhs.ty.trail().unwrap();
        if !self.follows(rhs.ty, trail) {
            return Err(err(
                span,
                format!(
                    "`{}` cannot be applied to {} and {}: on values of a trail it needs two of the same trail",
                    op.as_str(),
                    lhs.ty,
                    rhs.ty
                ),
            ));
        }
        if !self.hir.trails[trail].follows.iter().any(|f| f == needs) {
            let what = if needs == "Compare" {
                "compared"
            } else {
                "used as numbers"
            };
            return Err(err(
                span,
                format!(
                    "`{}` values cannot be {what}: `{}` does not follow {needs}",
                    shown_trail(trail),
                    shown_trail(trail)
                ),
            ));
        }
        // Numeric does not ask for `mod`: a trail that has one can use `%`.
        if op == BinOp::Mod
            && !self.hir.trails[trail]
                .methods
                .iter()
                .any(|m| m.name == "mod")
        {
            return Err(err(
                span,
                format!(
                    "`%` on `{}` values calls its `mod @other :: Self -> Self`, which `{}` does not give",
                    shown_trail(trail),
                    shown_trail(trail)
                ),
            ));
        }
        let group = self.method_of(lhs.ty, method, span, 1)?;
        let call = self.call_typed(&group, vec![lhs, rhs], span)?;
        if needs == "Numeric" {
            return Ok(call);
        }
        // `a < b` is `a.compare b < 0`, and so on.
        let zero = TExpr {
            kind: TExprKind::Int(0),
            ty: Ty::INT,
            span,
        };
        Ok(TExpr {
            kind: TExprKind::Binary {
                op,
                lhs: Box::new(call),
                rhs: Box::new(zero),
            },
            ty: Ty::BOOL,
            span,
        })
    }

    /// The instance of `compare` that tests whether the value checked by a
    /// `check` matches a pattern that is a value of a trail.
    fn pattern_compare(&mut self, scrutinee: &TExpr, pat: &TExpr) -> Result<InstId, Diagnostic> {
        let single = |t: Ty| t.as_kind() == Some(Kind::Record);
        if !single(pat.ty) || !single(scrutinee.ty) || scrutinee.ty.trail() != pat.ty.trail() {
            return Err(err(
                pat.span,
                format!(
                    "a pattern that is a value of a trail needs the checked value to be one of the same trail, but they are {} and {}",
                    scrutinee.ty, pat.ty
                ),
            ));
        }
        let trail = pat.ty.trail().unwrap();
        if !self.hir.trails[trail]
            .follows
            .iter()
            .any(|f| f == "Compare")
        {
            return Err(err(
                pat.span,
                format!(
                    "a `check` compares its value with each pattern, and `{}` does not follow Compare",
                    shown_trail(trail)
                ),
            ));
        }
        let group = self.method_of(pat.ty, "compare", pat.span, 1)?;
        let f = self.hir.groups[&group][0];
        Ok(self.instantiate(f, vec![scrutinee.ty, pat.ty]))
    }

    /// The types of a trail's fields: declared, or, for a field declared
    /// only with a default, what its default gives (from the fields before
    /// it). The latter grows during the fixpoint, like a return type.
    fn field_tys(&mut self, trail: &str) -> Vec<Ty> {
        let hir = self.hir;
        let fields = &hir.trails[trail].fields;
        let mut tys: Vec<Option<Ty>> = vec![None; fields.len()];
        for k in 0..fields.len() {
            self.field_ty(trail, k, &mut tys, &mut Vec::new());
        }
        tys.into_iter().map(|t| t.unwrap_or(Ty::NEVER)).collect()
    }

    /// The type of field `k`, after those of the fields its default uses.
    fn field_ty(
        &mut self,
        trail: &str,
        k: usize,
        tys: &mut Vec<Option<Ty>>,
        visiting: &mut Vec<usize>,
    ) -> Ty {
        if let Some(t) = tys[k] {
            return t;
        }
        let hir = self.hir;
        let fields = &hir.trails[trail].fields;
        let ty = match (&fields[k].ty, &fields[k].default) {
            (Some(ty), _) => *ty,
            // defaults that use each other: reported when a value is made
            _ if visiting.contains(&k) => Ty::NEVER,
            (None, FieldDefault::Func { name, uses }) => {
                visiting.push(k);
                let args: Vec<Ty> = uses
                    .iter()
                    .map(|u| {
                        let j = fields.iter().position(|f| &f.name == u).unwrap();
                        self.field_ty(trail, j, tys, visiting)
                    })
                    .collect();
                visiting.pop();
                let inst = self.instantiate(hir.groups[name][0], args);
                self.instances[inst].ret
            }
            (None, _) => Ty::NEVER,
        };
        tys[k] = Some(ty);
        ty
    }

    /// Every value of `ty` follows the trail `bound`: a trail of the
    /// program follows the trails in its `follow` line (and itself); the
    /// embedded values follow Show and Copy, and an array does when its
    /// elements do.
    fn follows(&self, ty: Ty, bound: &str) -> bool {
        let embedded = |k: Kind| {
            self.hir
                .embedded
                .get(&k)
                .is_some_and(|f| f.iter().any(|f| f == bound))
        };
        ty.kinds().all(|k| match k {
            Kind::Record => ty
                .trail()
                .is_some_and(|t| self.hir.trails[t].follows.iter().any(|f| f == bound)),
            Kind::Array => embedded(k) && ty.elem().is_some_and(|e| self.follows(e, bound)),
            k => embedded(k),
        })
    }

    /// `follows`, for the trail values and arrays in `ty`. The other
    /// embedded values a bound takes are settled by its accepted type, with
    /// a run-time check where the type cannot tell (a `Numeric` parameter
    /// given an `Int|String` value, as before trails).
    fn follows_bound(&self, ty: Ty, bound: &str) -> bool {
        [Kind::Record, Kind::Array]
            .into_iter()
            .filter(|k| ty.contains(*k))
            .all(|k| self.follows(ty.only(k), bound))
    }

    /// What a parameter or result declared with `bound` takes (see
    /// `hir::accepts`).
    fn accepted(&self, bound: &str) -> Ty {
        crate::hir::accepts(Ty::ANY, &Some(bound.to_string()), &self.hir.embedded)
    }

    /// `v` follows `bound`, or an error saying what was expected.
    fn bound(
        &self,
        v: &TExpr,
        bound: &str,
        what: impl FnOnce() -> String,
    ) -> Result<(), Diagnostic> {
        if v.ty.is_never() || self.follows_bound(v.ty, bound) {
            return Ok(());
        }
        Err(err(
            v.span,
            format!("{}, but this is {}, which does not follow it", what(), v.ty),
        ))
    }

    /// `copy` takes values whose trail follows Copy, and so must every
    /// field of such a trail.
    fn copyable(
        &mut self,
        ty: Ty,
        span: Span,
        seen: &mut Vec<&'static str>,
    ) -> Result<(), Diagnostic> {
        let hir = self.hir;
        for t in ty.trails() {
            if seen.contains(&t) {
                continue;
            }
            seen.push(t);
            let trail = &hir.trails[t];
            if !trail.follows_copy {
                return Err(err(
                    span,
                    format!(
                        "`copy` needs a value whose trail follows Copy, but `{}` does not follow Copy",
                        shown_trail(t)
                    ),
                ));
            }
            let tys = self.field_tys(t);
            for (f, fty) in trail.fields.iter().zip(tys) {
                self.copyable(fty, span, seen).map_err(|_| {
                    err(
                        span,
                        format!(
                            "`{}` follows Copy, so its fields must too, but `{}` is {fty}, which does not",
                            shown_trail(t),
                            f.name
                        ),
                    )
                })?;
            }
        }
        Ok(())
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
            HExprKind::Over | HExprKind::Jump => {
                // What the carried variables hold here reaches the top of the
                // loop (`jump`) or its end (`over`), which have the same
                // types.
                let (carried, exits) = cx.loop_exits.last_mut().expect("resolved inside a loop");
                for (&l, exit) in carried.iter().zip(exits.iter_mut()) {
                    *exit = join(
                        *exit,
                        cx.local_tys[l],
                        span,
                        "the values a variable gets in this loop",
                    )?;
                }
                let kind = if matches!(e.kind, HExprKind::Over) {
                    TExprKind::Over
                } else {
                    TExprKind::Jump
                };
                (kind, Ty::NEVER)
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
                        if let Some(bound) = &cx.ret_bound {
                            v = self.narrow(v, self.accepted(bound), || {
                                format!("the function is declared to return {bound}")
                            })?;
                            self.bound(&v, bound, || {
                                format!("the function is declared to return {bound}")
                            })?;
                        } else if let Some(declared) = cx.declared
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
            HExprKind::Array(items) => {
                let items = items
                    .iter()
                    .map(|i| self.expr(cx, i))
                    .collect::<Result<Vec<_>, _>>()?;
                self.array_literal(items)?
            }
            HExprKind::Range(_, end) if is_infinite(end) => return Err(endless_range(span)),
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
            HExprKind::MethodCall {
                receiver,
                name,
                name_span,
                args,
            } => {
                let receiver = self.expr(cx, receiver)?;
                if receiver.ty.is_never() {
                    return Ok(TExpr {
                        kind: TExprKind::Diverge(vec![receiver]),
                        ty: Ty::NEVER,
                        span,
                    });
                }
                let group = self.method_of(receiver.ty, name, *name_span, args.len())?;
                let mut targs = vec![receiver];
                for a in args {
                    targs.push(self.expr(cx, a)?);
                }
                return self.call_typed(&group, targs, span);
            }
            HExprKind::Construct {
                trail,
                steps,
                slots,
            } => {
                let hir = self.hir;
                let tys = self.field_tys(trail);
                let fields = &hir.trails[trail].fields;
                let shown = shown_trail(trail);
                let mut checked = Vec::new();
                for (l, v) in steps {
                    let k = slots
                        .iter()
                        .position(|s| s == l)
                        .expect("every step fills a slot");
                    let v = self.expr(cx, v)?;
                    let target = tys[k];
                    // An undeclared field's type comes from its default,
                    // which may not be known yet in an early round.
                    let v = if v.ty.is_never() || target.is_never() {
                        v
                    } else {
                        let v = self.narrow(v, target, || {
                            format!("the field `{}` of `{shown}` is {target}", fields[k].name)
                        })?;
                        convert(v, target)
                    };
                    cx.local_tys[*l] = v.ty;
                    checked.push((*l, v));
                }
                (
                    TExprKind::Construct {
                        trail: trail.clone(),
                        steps: checked,
                        slots: slots.clone(),
                    },
                    Ty::record(trail),
                )
            }
            HExprKind::Field(base, name, field_span) => {
                let base = self.expr(cx, base)?;
                if base.ty.is_never() {
                    return Ok(TExpr {
                        kind: TExprKind::Diverge(vec![base]),
                        ty: Ty::NEVER,
                        span,
                    });
                }
                // The embedded values follow Show too: their `textualize` is
                // what `echo` prints. So is that of a value that may be of
                // several kinds (`Fruit?`, a Fruit or void), whose kind
                // decides while the program runs.
                if name == "textualize"
                    && base.ty.as_kind() != Some(Kind::Record)
                    && self.follows_bound(base.ty, "Show")
                {
                    return Ok(TExpr {
                        kind: TExprKind::Textualize(Box::new(base)),
                        ty: Ty::STR,
                        span,
                    });
                }
                // A value that may be void gives void (nothing flows through).
                let Some(trail) = base
                    .ty
                    .trail()
                    .filter(|_| base.ty.without(Ty::VOID).as_kind() == Some(Kind::Record))
                else {
                    return Err(err(
                        *field_span,
                        format!(
                            "`.{name}` reads a field of a trail's value, but this is {}",
                            base.ty
                        ),
                    ));
                };
                let hir = self.hir;
                let Some(k) = hir.trails[trail]
                    .fields
                    .iter()
                    .position(|f| &f.name == name)
                else {
                    let why = if name == "textualize" {
                        " (it does not follow Show)"
                    } else if hir.trails[trail].methods.iter().any(|m| &m.name == name) {
                        " (it is a function: give it its arguments)"
                    } else {
                        ""
                    };
                    return Err(err(
                        *field_span,
                        format!("`{}` has no field `{name}`{why}", shown_trail(trail)),
                    ));
                };
                let mut ty = self.field_tys(trail)[k];
                if base.ty.contains(Kind::Void) && !ty.is_never() {
                    ty = ty.union(Ty::VOID);
                }
                (TExprKind::Field(Box::new(base), k), ty)
            }
            HExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.expr(cx, lhs)?;
                let rhs = self.expr(cx, rhs)?;
                // On values of trails, the operators are the trails'
                // functions: `compare` (Compare), `add`, `sub`, `mul`,
                // `div` (Numeric), and `mod` when the trail gives it.
                if !lhs.ty.trails().is_empty() || !rhs.ty.trails().is_empty() {
                    return self.trail_operator(*op, lhs, rhs, span);
                }
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
            // `!b` is `b != true`, on a boolean.
            HExprKind::Not(inner) => {
                let inner = self.expr(cx, inner)?;
                if inner.ty.is_never() {
                    return Ok(TExpr {
                        kind: TExprKind::Diverge(vec![inner]),
                        ty: Ty::NEVER,
                        span,
                    });
                }
                if !inner.ty.is_subset(Ty::BOOL) {
                    let hint = if inner.ty.is_maybe() {
                        ": it may be void, so handle the void case first, with a `check` that has a `void ->` arm"
                    } else {
                        ""
                    };
                    return Err(err(
                        inner.span,
                        format!("`!` needs a Boolean, but this is {}{hint}", inner.ty),
                    ));
                }
                let yes = TExpr {
                    kind: TExprKind::Bool(true),
                    ty: Ty::BOOL,
                    span,
                };
                (
                    TExprKind::Binary {
                        op: BinOp::Ne,
                        lhs: Box::new(inner),
                        rhs: Box::new(yes),
                    },
                    Ty::BOOL,
                )
            }
            HExprKind::Is { value, ty, bound } => {
                let value = self.expr(cx, value)?;
                // Each kind of the value's type passes or not while
                // compiling: an array by its elements, a value of a trail
                // by its trail. Only which kind a value of a union type
                // holds waits for run time.
                let accepted = crate::hir::accepts(*ty, bound, &self.hir.embedded);
                let passes = value
                    .ty
                    .kinds()
                    .map(|k| value.ty.only(k))
                    .filter(|part| {
                        part.is_subset(accepted)
                            && bound.as_ref().is_none_or(|b| self.follows_bound(*part, b))
                    })
                    .fold(Ty::NEVER, Ty::union);
                let ty = if value.ty.is_never() {
                    Ty::NEVER
                } else {
                    Ty::BOOL
                };
                (TExprKind::Is(Box::new(value), passes), ty)
            }
            HExprKind::Check {
                scrutinee,
                arms,
                else_arm,
                carried,
                is_if,
                logic,
            } => {
                let scrutinee = self.expr(cx, scrutinee)?;
                // `a && b`, `a || b`: two booleans
                if let Some(op) = logic
                    && !scrutinee.ty.is_never()
                    && !scrutinee.ty.is_subset(Ty::BOOL)
                {
                    return Err(err(scrutinee.span, logic_error(*op, scrutinee.ty)));
                }
                if *is_if && !scrutinee.ty.is_never() && !scrutinee.ty.is_subset(Ty::BOOL) {
                    let hint = if scrutinee.ty.is_maybe() {
                        ": it may be void, so handle the void case first, with a `check` that has a `void ->` arm"
                    } else {
                        ""
                    };
                    return Err(err(
                        scrutinee.span,
                        format!(
                            "the condition of `if` must be a Boolean, but this is {}{hint}",
                            scrutinee.ty
                        ),
                    ));
                }
                // A comprehension makes each element on its own: nothing
                // carries over from one element to the next.
                if cx.in_comprehension > 0
                    && let Some(&l) = carried.first()
                {
                    return Err(err(
                        span,
                        format!(
                            "a list comprehension cannot give `{}` a new value: it makes each element on its own, and keeps nothing from one to the next",
                            cx.names[l]
                        ),
                    ));
                }
                let what = if *is_if {
                    "the blocks of this `if`"
                } else {
                    "the branches of this `check`"
                };
                // Handling the void case: when the checked value is a
                // variable of a maybe type (`int?`), an arm whose pattern is
                // not void only runs when the variable is not void, so in
                // that arm it has the plain type (`int`). After a `void ->`
                // arm, the same holds for `else`. (Not when an arm gives the
                // variable a new value: after the `check` it would be
                // unclear which value is meant.)
                let narrow_local = match scrutinee.kind {
                    // (A variable that can only be void never reaches the
                    // arms for other values: there it is `never`.)
                    TExprKind::Local(l)
                        if (scrutinee.ty.is_maybe() || scrutinee.ty == Ty::VOID)
                            && !carried.contains(&l) =>
                    {
                        Some(l)
                    }
                    _ => None,
                };
                // Every arm starts from the types before it, and the
                // variables an arm gives new values have, after the
                // `check`, any type an arm that gets there leaves in them.
                let start = cx.local_tys.clone();
                let mut ends: Vec<Vec<Ty>> = Vec::new();
                let mut end_of = |cx: &Ctx, body: &TExpr| {
                    if !body.ty.is_never() {
                        ends.push(carried.iter().map(|&l| cx.local_tys[l]).collect());
                    }
                };
                let narrowed_ty = scrutinee.ty.without(Ty::VOID);
                let mut narrowed_arms = Vec::new();
                let mut compares = Vec::new();
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
                    cx.local_tys.clone_from(&start);
                    let pat = self.expr(cx, pat)?;
                    // `void ->` on a value that may be void is fine; a
                    // pattern that is a trail's value would need comparing.
                    // A pattern that is a value of a trail is compared with
                    // the trail's `compare`.
                    compares.push(if pat.ty.trails().is_empty() {
                        None
                    } else {
                        Some(self.pattern_compare(&scrutinee, &pat)?)
                    });
                    match pat.kind {
                        TExprKind::Bool(true) => seen_true = true,
                        TExprKind::Bool(false) => seen_false = true,
                        TExprKind::Void => seen_void = true,
                        _ => {}
                    }
                    let narrowed = narrow_local.is_some() && !pat.ty.contains(Kind::Void);
                    let body = arm_body(self, cx, body, narrowed)?;
                    if let Some(op) = logic
                        && !body.ty.is_never()
                        && !body.ty.is_subset(Ty::BOOL)
                    {
                        return Err(err(body.span, logic_error(*op, body.ty)));
                    }
                    end_of(cx, &body);
                    ty = join(ty, body.ty, body.span, what)?;
                    narrowed_arms.push(narrowed);
                    tarms.push((pat, body));
                }
                let else_narrowed = narrow_local.is_some() && seen_void;
                let else_arm = match else_arm {
                    Some(e) => {
                        cx.local_tys.clone_from(&start);
                        let e = arm_body(self, cx, e, else_narrowed)?;
                        end_of(cx, &e);
                        ty = join(ty, e.ty, e.span, what)?;
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
                let mut after = start;
                let mut carried_tys = Vec::new();
                for (k, &l) in carried.iter().enumerate() {
                    let mut t = if ends.is_empty() { after[l] } else { Ty::NEVER };
                    for end in &ends {
                        t = join(
                            t,
                            end[k],
                            span,
                            &format!("the values `{}` gets in {what}", cx.names[l]),
                        )?;
                    }
                    after[l] = t;
                    carried_tys.push((l, t));
                }
                cx.local_tys = after;
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
                        compares,
                        carried: carried_tys,
                    },
                    ty,
                )
            }
            HExprKind::Block { stmts, inner } => {
                // The block's own variables are new every time it runs.
                for &l in inner {
                    cx.local_tys[l] = Ty::NEVER;
                }
                let stmts = self.stmts(cx, stmts)?;
                let ty = match stmts.last() {
                    Some(TStmt::Expr(v)) => v.ty,
                    _ => Ty::VOID,
                };
                (
                    TExprKind::Block {
                        stmts,
                        inner: inner.clone(),
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
        self.call_typed(name, args, span)
    }

    /// A call of the function group `group` with arguments already typed.
    /// (A function of a trail is the group `Trail$name`; messages show
    /// `name`.)
    fn call_typed(
        &mut self,
        group: &str,
        args: Vec<TExpr>,
        span: Span,
    ) -> Result<TExpr, Diagnostic> {
        let name = group.rsplit('$').next().unwrap_or(group);
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
        let candidates: Vec<FuncId> = hir.groups[group]
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
            // A parameter declared with a trail takes values of trails that
            // follow it, which the types tell while compiling.
            if args
                .iter()
                .zip(&func.bounds)
                .any(|(a, b)| b.as_ref().is_some_and(|b| !self.follows_bound(a.ty, b)))
            {
                continue;
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
                    if let Some((i, (a, b))) =
                        args.iter().zip(&f.bounds).enumerate().find(|(_, (a, b))| {
                            b.as_ref().is_some_and(|b| !self.follows_bound(a.ty, b))
                        })
                    {
                        return Err(err(
                            a.span,
                            format!(
                                "argument {} of `{name}` must follow {}, but this is {}, which does not",
                                i + 1,
                                shown_trail(b.as_ref().unwrap()),
                                a.ty
                            ),
                        ));
                    }
                    let (i, (a, t)) = args
                        .iter()
                        .zip(&f.param_tys)
                        .enumerate()
                        .find(|(_, (a, t))| a.ty.intersect(**t).is_never())
                        .unwrap();
                    if let Some(b) = &f.bounds[i] {
                        return Err(err(
                            a.span,
                            format!(
                                "argument {} of `{name}` must follow {}, but this is {}, which does not",
                                i + 1,
                                shown_trail(b),
                                a.ty
                            ),
                        ));
                    }
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
        // `copy` (of the trail Copy) copies the whole value: the fields of
        // a trail that follows Copy must follow it too.
        if let Some(Native::Intrinsic(i)) = &hir.funcs[chosen].native
            && i.trail == "Copy"
        {
            self.copyable(args[0].ty, args[0].span, &mut Vec::new())?;
        }
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
            ty = join(
                ty,
                self.instances[inst].ret,
                span,
                "the definitions this call may reach",
            )?;
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
                native: func.native.clone(),
            });
        }
        let mut trails = HashMap::new();
        let names: Vec<String> = self.hir.trails.keys().cloned().collect();
        for name in names {
            let tys = self.field_tys(&name);
            let fields = self.hir.trails[&name]
                .fields
                .iter()
                .zip(tys)
                .map(|(f, t)| (f.name.clone(), t))
                .collect();
            trails.insert(
                name.clone(),
                TTrail {
                    name: shown_trail(&name),
                    fields,
                    shows: self.hir.trails[&name].shows,
                },
            );
        }
        let mut warnings: Vec<Diagnostic> = Vec::new();
        for w in std::mem::take(&mut self.warnings) {
            if !warnings
                .iter()
                .any(|o| o.span == w.span && o.message == w.message)
            {
                warnings.push(w);
            }
        }
        Tir {
            funcs,
            entry: 0,
            trails,
            warnings,
        }
    }
}

pub fn visit_children(e: &TExpr, f: &mut impl FnMut(&TExpr)) {
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
        TExprKind::Neg(x) | TExprKind::Is(x, _) | TExprKind::Convert(x) => f(x),
        TExprKind::Array(items) => items.iter().for_each(f),
        TExprKind::Ret(value) => value.iter().for_each(|v| f(v)),
        TExprKind::Over | TExprKind::Jump => {}
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
        TExprKind::Construct { steps, .. } => steps.iter().for_each(|(_, v)| f(v)),
        TExprKind::Field(base, _) | TExprKind::Textualize(base) => f(base),
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
        TExprKind::Block { stmts, .. } => for_each_expr(stmts, f),
    }
}

fn collect_calls(e: &TExpr, out: &mut Vec<InstId>) {
    match &e.kind {
        TExprKind::Call { inst, .. } => out.push(*inst),
        TExprKind::Check { compares, .. } => out.extend(compares.iter().flatten()),
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
        TExprKind::Neg(x) | TExprKind::Is(x, _) | TExprKind::Convert(x) => remap_calls(x, index),
        TExprKind::Array(items) => items.iter_mut().for_each(|i| remap_calls(i, index)),
        TExprKind::Ret(value) => value.iter_mut().for_each(|v| remap_calls(v, index)),
        TExprKind::Over | TExprKind::Jump => {}
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
        TExprKind::Construct { steps, .. } => {
            steps.iter_mut().for_each(|(_, v)| remap_calls(v, index))
        }
        TExprKind::Field(base, _) | TExprKind::Textualize(base) => remap_calls(base, index),
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
            compares,
            ..
        } => {
            for c in compares.iter_mut().flatten() {
                *c = index[c];
            }
            remap_calls(scrutinee, index);
            for (p, b) in arms {
                remap_calls(p, index);
                remap_calls(b, index);
            }
            if let Some(e) = else_arm {
                remap_calls(e, index);
            }
        }
        TExprKind::Block { stmts, .. } => {
            for_each_expr_mut(stmts, &mut |e| remap_calls(e, index));
        }
        _ => {}
    }
}
