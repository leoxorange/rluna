//! Compile-time evaluation of constant globals.
//!
//! A global definition such as `sum = 5` or `foo = "he" + "llo"` is a
//! function with no arguments, so every use of `foo` would run the
//! concatenation again at run time. But such a global has no inputs, so if
//! evaluating it has no side effects, its value is the same every time and
//! can be computed once, here, by the compiler. Every call is then replaced
//! by the constant: `foo` becomes the string literal `"hello"`.
//!
//! Evaluation is done by a small interpreter over the typed tree (TIR). It
//! gives up, leaving the global as an ordinary function, when the global is
//! not a constant:
//!
//! - it has a side effect (`echo`), like `banner = echo "welcome"`, which
//!   must print every time it is used, or calls a native function (input,
//!   output, files), which only the running program can;
//! - it hits a runtime error (a failed type check, an invalid operator),
//!   which must happen when the program runs, not when it is compiled;
//! - it runs too long or recurses too deeply, which could also mean it
//!   never finishes; the compiler must not hang.
//!
//! The interpreter must compute exactly what the compiled code would, down
//! to the bit: integer arithmetic wraps around, `/` always divides as floats,
//! mixed int/float operations convert the int first, and so on. Each rule
//! below mirrors `lower::concrete_binary`.
//!
//! This is *constant folding* applied to whole functions. C++ `constexpr`
//! and Rust `const fn` are the same idea, made explicit by the programmer;
//! here the compiler finds the constants itself.

use std::collections::HashMap;

use crate::ast::BinOp;
use crate::hir::{Builtin, HPattern};
use crate::typeck::{
    DispatchCase, InstId, LoopSource, TClause, TExpr, TExprKind, TFunc, TStmt, Tir, binop_result,
    for_each_expr_mut,
};
use crate::types::{Kind, Ty};

/// How much work evaluating one global may take, in evaluated expressions.
const FUEL: u64 = 1_000_000;
/// How deep calls may nest. The interpreter recurses on its stack, which
/// must not overflow.
const MAX_DEPTH: usize = 1000;
/// The interpreter runs on its own thread with this much stack, so the
/// depth limit is safe whatever thread the compiler itself runs on (test
/// threads, for instance, get only 2 MiB). The memory is only reserved; it
/// is used only as deep as evaluation actually goes.
const STACK_SIZE: usize = 512 << 20;

/// A value computed at compile time.
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Void,
    Array(Vec<Const>),
}

impl Const {
    fn kind(&self) -> Kind {
        match self {
            Const::Int(_) => Kind::Int,
            Const::Float(_) => Kind::Float,
            Const::Str(_) => Kind::Str,
            Const::Bool(_) => Kind::Bool,
            Const::Void => Kind::Void,
            Const::Array(_) => Kind::Array,
        }
    }

    fn to_float(&self) -> f64 {
        match self {
            Const::Int(v) => *v as f64,
            Const::Float(v) => *v,
            _ => unreachable!("not a number"),
        }
    }
}

/// Why evaluation of an expression stopped early.
enum Stop {
    /// The global is not a constant (a side effect, a runtime error, too
    /// much work).
    NotConstant,
    /// `ret`: the current call returns this value.
    Return(Const),
    /// `over`: the innermost loop ends.
    Over,
    /// `jump`: the innermost loop goes on with its next round.
    Jump,
}

use Stop::NotConstant;

type Eval<T> = Result<T, Stop>;

/// Evaluate every constant global and replace its calls with the value.
pub fn fold_globals(tir: &mut Tir) {
    let shared: &Tir = tir;
    let consts = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("consteval".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, || evaluate_globals(shared))
            .ok()
            .and_then(|thread| thread.join().ok())
    })
    // Without a thread nothing is folded; the program is still correct.
    .unwrap_or_default();
    for f in &mut tir.funcs {
        for_each_expr_mut(&mut f.body, &mut |e| substitute(e, &consts));
    }
}

/// The value of every global that is a constant.
fn evaluate_globals(tir: &Tir) -> HashMap<InstId, Const> {
    let mut consts: HashMap<InstId, Const> = HashMap::new();
    for (id, f) in tir.funcs.iter().enumerate() {
        // `main` is the program itself, not a value.
        if !f.params.is_empty() || id == tir.entry {
            continue;
        }
        let mut interp = Interp {
            tir,
            fuel: FUEL,
            depth: 0,
        };
        // Void globals have no value to substitute.
        if let Ok(v) = interp.call(id, Vec::new())
            && v != Const::Void
        {
            consts.insert(id, v);
        }
    }
    consts
}

/// An expression of type `ty` with the value `c`. The type can be wider
/// than the value's kind (a global whose `check` gives `int|string`): then
/// the literal is converted (boxed) to it, as the call's result would be.
/// An array becomes an array literal; each use builds a new array.
fn literal(c: &Const, ty: Ty, span: crate::diag::Span) -> TExpr {
    let (kind, own) = match c {
        Const::Int(v) => (TExprKind::Int(*v), Ty::INT),
        Const::Float(v) => (TExprKind::Float(*v), Ty::FLOAT),
        Const::Str(s) => (TExprKind::Str(s.clone()), Ty::STR),
        Const::Bool(b) => (TExprKind::Bool(*b), Ty::BOOL),
        Const::Void => (TExprKind::Void, Ty::VOID),
        Const::Array(items) => {
            let elem = ty.elem().unwrap_or(Ty::NEVER);
            let items = items.iter().map(|i| literal(i, elem, span)).collect();
            (TExprKind::Array(items), Ty::array(elem))
        }
    };
    let e = TExpr {
        kind,
        ty: own,
        span,
    };
    if own == ty {
        e
    } else {
        TExpr {
            kind: TExprKind::Convert(Box::new(e)),
            ty,
            span,
        }
    }
}

/// Replace calls of constant globals in `e` with their values.
fn substitute(e: &mut TExpr, consts: &HashMap<InstId, Const>) {
    if let TExprKind::Call { inst, .. } = &e.kind
        && let Some(c) = consts.get(inst)
    {
        *e = literal(c, e.ty, e.span);
        return;
    }
    match &mut e.kind {
        TExprKind::Int(_)
        | TExprKind::Float(_)
        | TExprKind::Str(_)
        | TExprKind::Bool(_)
        | TExprKind::Void
        | TExprKind::Local(_) => {}
        TExprKind::Call { args, .. }
        | TExprKind::Dispatch { args, .. }
        | TExprKind::Diverge(args) => args.iter_mut().for_each(|a| substitute(a, consts)),
        TExprKind::Binary { lhs, rhs, .. } => {
            substitute(lhs, consts);
            substitute(rhs, consts);
        }
        TExprKind::Neg(x)
        | TExprKind::Is(x, _)
        | TExprKind::Echo(x)
        | TExprKind::Convert(x)
        | TExprKind::Builtin(_, x) => substitute(x, consts),
        TExprKind::Array(items) => items.iter_mut().for_each(|i| substitute(i, consts)),
        TExprKind::Construct { steps, .. } => {
            steps.iter_mut().for_each(|(_, v)| substitute(v, consts))
        }
        TExprKind::Field(base, _) | TExprKind::Textualize(base) => substitute(base, consts),
        TExprKind::Ret(value) => value.iter_mut().for_each(|v| substitute(v, consts)),
        TExprKind::Over | TExprKind::Jump => {}
        TExprKind::Range(a, b)
        | TExprKind::Concat(a, b)
        | TExprKind::Index(a, b)
        | TExprKind::ElemMul(a, b) => {
            substitute(a, consts);
            substitute(b, consts);
        }
        TExprKind::Comprehension { body, clauses } => {
            clauses
                .iter_mut()
                .flat_map(TClause::exprs_mut)
                .for_each(|e| substitute(e, consts));
            substitute(body, consts);
        }
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
            ..
        } => {
            substitute(scrutinee, consts);
            for (p, b) in arms {
                substitute(p, consts);
                substitute(b, consts);
            }
            if let Some(e) = else_arm {
                substitute(e, consts);
            }
        }
        TExprKind::Block { stmts, .. } => for_each_expr_mut(stmts, &mut |e| substitute(e, consts)),
    }
}

struct Interp<'a> {
    tir: &'a Tir,
    fuel: u64,
    depth: usize,
}

/// The variables (parameters first) of one call being evaluated.
struct Frame {
    locals: Vec<Option<Const>>,
}

impl Interp<'_> {
    fn call(&mut self, inst: InstId, args: Vec<Const>) -> Eval<Const> {
        // A native function reaches the world outside (input, files): what
        // it gives is only known when the program runs.
        if self.depth == MAX_DEPTH || self.tir.funcs[inst].native.is_some() {
            return Err(NotConstant);
        }
        self.depth += 1;
        let f: &TFunc = &self.tir.funcs[inst];
        let mut locals: Vec<Option<Const>> = args.into_iter().map(Some).collect();
        locals.resize(f.local_tys.len(), None);
        for (p, pattern) in f.patterns.iter().enumerate() {
            if let (Some(HPattern::Cons { heads, rest }), Some(Const::Array(items))) =
                (pattern, &locals[p])
            {
                let items = items.clone();
                for (k, &h) in heads.iter().enumerate() {
                    locals[h] = Some(items[k].clone());
                }
                locals[*rest] = Some(Const::Array(items[heads.len()..].to_vec()));
            }
        }
        let mut frame = Frame { locals };
        let result = match self.stmts(&f.body, &mut frame) {
            Err(Stop::Return(v)) => v,
            other => other?,
        };
        self.depth -= 1;
        Ok(if f.discard { Const::Void } else { result })
    }

    /// Run statements; the value of the last one if it is an expression.
    fn stmts(&mut self, stmts: &[TStmt], frame: &mut Frame) -> Eval<Const> {
        let mut result = Const::Void;
        for stmt in stmts {
            result = Const::Void;
            match stmt {
                TStmt::Let { local, value } => {
                    let v = self.expr(value, frame)?;
                    frame.locals[*local] = Some(v);
                }
                TStmt::Expr(e) => result = self.expr(e, frame)?,
                TStmt::Loop {
                    pos,
                    var,
                    source,
                    body,
                    ..
                } => {
                    // A range is counted as it goes: `[0..infinite]` with an
                    // `over` ends, and the fuel stops one without.
                    let elements = match &**source {
                        LoopSource::Range(start, end) => {
                            let (Const::Int(from), Const::Int(to)) =
                                (self.expr(start, frame)?, self.expr(end, frame)?)
                            else {
                                unreachable!("range bounds are ints")
                            };
                            Box::new((from..to).map(Const::Int)) as Box<dyn Iterator<Item = Const>>
                        }
                        LoopSource::Each(_) => Box::new(self.elements(source, frame)?.into_iter()),
                    };
                    for (i, element) in elements.enumerate() {
                        // Every iteration costs fuel, even an empty body.
                        self.fuel = self.fuel.checked_sub(1).ok_or(NotConstant)?;
                        frame.locals[*var] = Some(element);
                        if let Some(p) = pos {
                            frame.locals[*p] = Some(Const::Int(i as i64));
                        }
                        match self.stmts(body, frame) {
                            Ok(_) | Err(Stop::Jump) => {}
                            Err(Stop::Over) => break,
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    /// Run the clauses of a comprehension from the first: a generator binds
    /// each element in turn and runs the rest, a guard runs the rest only if
    /// it is true; at the end, the body's value is added.
    fn comprehension(
        &mut self,
        clauses: &[TClause],
        body: &TExpr,
        frame: &mut Frame,
        out: &mut Vec<Const>,
    ) -> Eval<()> {
        match clauses.split_first() {
            None => out.push(self.expr(body, frame)?),
            Some((
                TClause::Gen {
                    pos, var, source, ..
                },
                rest,
            )) => {
                for (i, element) in self.elements(source, frame)?.into_iter().enumerate() {
                    self.fuel = self.fuel.checked_sub(1).ok_or(NotConstant)?;
                    frame.locals[*var] = Some(element);
                    if let Some(p) = pos {
                        frame.locals[*p] = Some(Const::Int(i as i64));
                    }
                    self.comprehension(rest, body, frame, out)?;
                }
            }
            Some((TClause::Guard(g), rest)) => {
                if self.expr(g, frame)? == Const::Bool(true) {
                    self.comprehension(rest, body, frame, out)?;
                }
            }
        }
        Ok(())
    }

    /// The elements a loop or comprehension goes through.
    fn elements(&mut self, source: &LoopSource, frame: &mut Frame) -> Eval<Vec<Const>> {
        Ok(match source {
            LoopSource::Range(start, end) => {
                let from = self.expr(start, frame)?;
                let to = self.expr(end, frame)?;
                range(from, to)?
            }
            LoopSource::Each(e) => match self.expr(e, frame)? {
                Const::Array(items) => items,
                Const::Str(s) => s.chars().map(|c| Const::Str(c.to_string())).collect(),
                _ => unreachable!("loops go through arrays and strings"),
            },
        })
    }

    fn expr(&mut self, e: &TExpr, frame: &mut Frame) -> Eval<Const> {
        self.fuel = self.fuel.checked_sub(1).ok_or(NotConstant)?;
        Ok(match &e.kind {
            // Values of trails are made while the program runs.
            TExprKind::Construct { .. } | TExprKind::Field(..) | TExprKind::Textualize(_) => {
                return Err(NotConstant);
            }
            TExprKind::Int(v) => Const::Int(*v),
            TExprKind::Float(v) => Const::Float(*v),
            TExprKind::Str(s) => Const::Str(s.clone()),
            TExprKind::Bool(b) => Const::Bool(*b),
            TExprKind::Void => Const::Void,
            TExprKind::Array(items) => Const::Array(
                items
                    .iter()
                    .map(|i| self.expr(i, frame))
                    .collect::<Eval<_>>()?,
            ),
            TExprKind::Range(start, end) => {
                let from = self.expr(start, frame)?;
                let to = self.expr(end, frame)?;
                Const::Array(range(from, to)?)
            }
            TExprKind::Comprehension { body, clauses } => {
                let mut out = Vec::new();
                self.comprehension(clauses, body, frame, &mut out)?;
                Const::Array(out)
            }
            TExprKind::Builtin(b, arg) => builtin(*b, self.expr(arg, frame)?),
            TExprKind::Concat(a, b) => match (self.expr(a, frame)?, self.expr(b, frame)?) {
                (Const::Array(mut x), Const::Array(y)) => {
                    x.extend(y);
                    Const::Array(x)
                }
                _ => Const::Void, // void flows through
            },
            TExprKind::ElemMul(a, b) => match (self.expr(a, frame)?, self.expr(b, frame)?) {
                (Const::Array(x), Const::Array(y)) => {
                    let n = x.len().max(y.len());
                    let mut out = Vec::with_capacity(n);
                    for k in 0..n {
                        out.push(match (x.get(k), y.get(k)) {
                            (Some(p), Some(q)) => binary(BinOp::Mul, p, q)?,
                            (Some(p), None) | (None, Some(p)) => p.clone(),
                            (None, None) => unreachable!(),
                        });
                    }
                    Const::Array(out)
                }
                _ => Const::Void, // void flows through
            },
            TExprKind::Index(a, i) => match (self.expr(a, frame)?, self.expr(i, frame)?) {
                (Const::Array(items), Const::Int(i)) => usize::try_from(i)
                    .ok()
                    .and_then(|i| items.get(i).cloned())
                    .unwrap_or(Const::Void),
                (Const::Str(s), Const::Int(i)) => usize::try_from(i)
                    .ok()
                    .and_then(|i| s.chars().nth(i))
                    .map_or(Const::Void, |c| Const::Str(c.to_string())),
                _ => Const::Void,
            },
            TExprKind::Local(l) => frame.locals[*l].clone().expect("local is defined"),
            TExprKind::Call { inst, args } => {
                let args = args
                    .iter()
                    .map(|a| self.expr(a, frame))
                    .collect::<Eval<Vec<_>>>()?;
                self.call(*inst, args)?
            }
            TExprKind::Dispatch { args, cases, .. } => {
                let args = args
                    .iter()
                    .map(|a| self.expr(a, frame))
                    .collect::<Eval<Vec<_>>>()?;
                let fits = |c: &DispatchCase| {
                    c.tests.iter().zip(&c.lens).zip(&args).all(|((t, len), a)| {
                        t.is_none_or(|t| t.contains(a.kind()))
                            && len.is_none_or(|len| match a {
                                Const::Array(items) => len.accepts(items.len()),
                                _ => false,
                            })
                    })
                };
                // No definition accepts the values: a runtime error.
                let Some(case) = cases.iter().find(|c| fits(c)) else {
                    return Err(NotConstant);
                };
                self.call(case.inst, args)?
            }
            // A side effect: the global must run when the program runs.
            TExprKind::Echo(_) => return Err(NotConstant),
            // Some argument never finishes (it returns from the function, or
            // never ends), so neither does this.
            TExprKind::Diverge(args) => {
                for a in args {
                    self.expr(a, frame)?;
                }
                return Err(NotConstant);
            }
            TExprKind::Ret(value) => {
                let v = match value {
                    Some(v) => self.expr(v, frame)?,
                    None => Const::Void,
                };
                return Err(Stop::Return(v));
            }
            TExprKind::Over => return Err(Stop::Over),
            TExprKind::Jump => return Err(Stop::Jump),
            TExprKind::Binary { op, lhs, rhs } => {
                let a = self.expr(lhs, frame)?;
                let b = self.expr(rhs, frame)?;
                binary(*op, &a, &b)?
            }
            TExprKind::Is(x, passes) => Const::Bool(passes.contains(self.expr(x, frame)?.kind())),
            TExprKind::Neg(x) => match self.expr(x, frame)? {
                Const::Int(v) => Const::Int(0i64.wrapping_sub(v)),
                Const::Float(v) => Const::Float(-0.0 - v),
                Const::Void => Const::Void,
                _ => return Err(NotConstant), // a runtime operator error
            },
            // A runtime type check: if the value does not fit, the program
            // fails at run time, so leave that to the program.
            TExprKind::Convert(x) => {
                let v = self.expr(x, frame)?;
                if !e.ty.contains(v.kind()) {
                    return Err(NotConstant);
                }
                v
            }
            TExprKind::Check {
                scrutinee,
                arms,
                else_arm,
                ..
            } => {
                let s = self.expr(scrutinee, frame)?;
                for (pattern, body) in arms {
                    let p = self.expr(pattern, frame)?;
                    if matches(&s, &p) {
                        return self.expr(body, frame);
                    }
                }
                match else_arm {
                    Some(body) => self.expr(body, frame)?,
                    None => unreachable!("typeck proved the arms cover every value"),
                }
            }
            // The variables the block gives new values are the frame's.
            TExprKind::Block { stmts, .. } => self.stmts(stmts, frame)?,
        })
    }
}

/// A built-in function on a value; void flows through.
fn builtin(b: Builtin, v: Const) -> Const {
    // Constants have no owners: a copy is the value itself.
    if b == Builtin::Copy {
        return v;
    }
    let items: Vec<Const> = match &v {
        Const::Array(items) => items.clone(),
        Const::Str(s) => s.chars().map(|c| Const::Str(c.to_string())).collect(),
        _ => return Const::Void,
    };
    // Turn a run of characters back into one string.
    let rebuild = |part: &[Const]| match &v {
        Const::Str(_) => Const::Str(
            part.iter()
                .map(|c| match c {
                    Const::Str(s) => s.as_str(),
                    _ => unreachable!(),
                })
                .collect(),
        ),
        _ => Const::Array(part.to_vec()),
    };
    match b {
        Builtin::Len => Const::Int(items.len() as i64),
        Builtin::Reverse => {
            let mut reversed = items.clone();
            reversed.reverse();
            rebuild(&reversed)
        }
        Builtin::Head => items.first().cloned().unwrap_or(Const::Void),
        Builtin::Last => items.last().cloned().unwrap_or(Const::Void),
        _ if items.is_empty() => Const::Void,
        Builtin::Tail => rebuild(&items[1..]),
        Builtin::Init => rebuild(&items[..items.len() - 1]),
        Builtin::Copy => unreachable!("handled above"),
    }
}

/// The ints from..to.
fn range(from: Const, to: Const) -> Eval<Vec<Const>> {
    let (Const::Int(from), Const::Int(to)) = (from, to) else {
        unreachable!("range bounds are ints")
    };
    // A huge range would not fit in memory: not a constant.
    if to.saturating_sub(from) > FUEL as i64 {
        return Err(NotConstant);
    }
    Ok((from..to).map(Const::Int).collect())
}

/// Does a `check` pattern match? Void matches only void.
fn matches(value: &Const, pattern: &Const) -> bool {
    match (value, pattern) {
        (Const::Void, Const::Void) => true,
        (Const::Void, _) | (_, Const::Void) => false,
        _ => binary(BinOp::Eq, value, pattern).is_ok_and(|r| r == Const::Bool(true)),
    }
}

/// `x % y` as `rt.mod` computes it: the remainder with the sign of `y`
/// (or 0), so `-7 % 3` is 2. `y` is not 0.
fn floored_mod(x: i64, y: i64) -> i64 {
    // `wrapping_rem` is 0 for `i64::MIN % -1`, which overflows otherwise.
    let r = x.wrapping_rem(y);
    if r != 0 && (r < 0) != (y < 0) {
        r + y
    } else {
        r
    }
}

/// An operator on two values, exactly as the compiled code computes it.
fn binary(op: BinOp, a: &Const, b: &Const) -> Eval<Const> {
    use Const::*;
    if binop_result(op, a.kind(), b.kind()).is_none() {
        return Err(NotConstant); // a runtime operator error
    }
    // Nothing flows through.
    if *a == Void || *b == Void {
        return Ok(Void);
    }
    let numeric = |c: &Const| matches!(c, Int(_) | Float(_));
    Ok(match op {
        BinOp::Concat | BinOp::Index | BinOp::ElemMul | BinOp::And | BinOp::Or => {
            unreachable!("evaluated separately")
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => match (a, b) {
            (Str(x), Str(y)) => Str(format!("{x}{y}")),
            // Machine integers wrap around on overflow.
            (Int(x), Int(y)) if op != BinOp::Div => Int(match op {
                BinOp::Add => x.wrapping_add(*y),
                BinOp::Sub => x.wrapping_sub(*y),
                _ => x.wrapping_mul(*y),
            }),
            _ => {
                let (x, y) = (a.to_float(), b.to_float());
                Float(match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    _ => x / y,
                })
            }
        },
        BinOp::Mod => match (a, b) {
            // `x % 0` stops the program at run time.
            (Int(_), Int(0)) => return Err(NotConstant),
            (Int(x), Int(y)) => Int(floored_mod(*x, *y)),
            _ => unreachable!("only ints, see `binop_result`"),
        },
        BinOp::Eq | BinOp::Ne => {
            let eq = match (a, b) {
                (Int(x), Int(y)) => x == y,
                (Array(x), Array(y)) => x == y,
                (Bool(x), Bool(y)) => x == y,
                (Str(x), Str(y)) => x == y,
                _ if numeric(a) && numeric(b) => a.to_float() == b.to_float(),
                // Values of different kinds are never equal.
                _ => false,
            };
            Bool(eq == (op == BinOp::Eq))
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ordering = match (a, b) {
                // Strings compare byte by byte, like `rt.str_cmp`.
                (Str(x), Str(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
                (Int(x), Int(y)) => Some(x.cmp(y)),
                // Floats: no ordering with NaN, so every comparison is false.
                _ => a.to_float().partial_cmp(&b.to_float()),
            };
            Bool(ordering.is_some_and(|o| match op {
                BinOp::Lt => o.is_lt(),
                BinOp::Le => o.is_le(),
                BinOp::Gt => o.is_gt(),
                _ => o.is_ge(),
            }))
        }
    })
}
