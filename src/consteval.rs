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
//!   must print every time it is used;
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
use crate::typeck::{InstId, TExpr, TExprKind, TFunc, TStmt, Tir, binop_result, for_each_expr_mut};
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
}

impl Const {
    fn kind(&self) -> Kind {
        match self {
            Const::Int(_) => Kind::Int,
            Const::Float(_) => Kind::Float,
            Const::Str(_) => Kind::Str,
            Const::Bool(_) => Kind::Bool,
            Const::Void => Kind::Void,
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

/// Why evaluation stopped: in every case the global is not a constant.
struct NotConstant;

type Eval<T> = Result<T, NotConstant>;

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

/// Replace calls of constant globals in `e` with their values.
fn substitute(e: &mut TExpr, consts: &HashMap<InstId, Const>) {
    if let TExprKind::Call { inst, .. } = &e.kind
        && let Some(c) = consts.get(inst)
    {
        let kind = match c.clone() {
            Const::Int(v) => TExprKind::Int(v),
            Const::Float(v) => TExprKind::Float(v),
            Const::Str(s) => TExprKind::Str(s),
            Const::Bool(b) => TExprKind::Bool(b),
            Const::Void => unreachable!("void globals are not substituted"),
        };
        let literal = TExpr {
            kind,
            ty: Ty::of(c.kind()),
            span: e.span,
        };
        // The call's static type can be wider than the value's kind (a
        // global whose `check` gives `int|string`). Then the literal must
        // still be boxed into that type, as the call's result would be.
        e.kind = if literal.ty == e.ty {
            literal.kind
        } else {
            TExprKind::Convert(Box::new(literal))
        };
        return;
    }
    match &mut e.kind {
        TExprKind::Int(_)
        | TExprKind::Float(_)
        | TExprKind::Str(_)
        | TExprKind::Bool(_)
        | TExprKind::Local(_) => {}
        TExprKind::Call { args, .. }
        | TExprKind::Dispatch { args, .. }
        | TExprKind::Diverge(args) => args.iter_mut().for_each(|a| substitute(a, consts)),
        TExprKind::Binary { lhs, rhs, .. } => {
            substitute(lhs, consts);
            substitute(rhs, consts);
        }
        TExprKind::Neg(x) | TExprKind::Echo(x) | TExprKind::Convert(x) => substitute(x, consts),
        TExprKind::Check {
            scrutinee,
            arms,
            else_arm,
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
        if self.depth == MAX_DEPTH {
            return Err(NotConstant);
        }
        self.depth += 1;
        let f: &TFunc = &self.tir.funcs[inst];
        let mut locals: Vec<Option<Const>> = args.into_iter().map(Some).collect();
        locals.resize(f.local_tys.len(), None);
        let mut frame = Frame { locals };
        let result = self.stmts(&f.body, &mut frame)?;
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
                    var,
                    start,
                    end,
                    body,
                    ..
                } => {
                    let (Const::Int(from), Const::Int(to)) =
                        (self.expr(start, frame)?, self.expr(end, frame)?)
                    else {
                        unreachable!("loop bounds are ints")
                    };
                    for i in from..to {
                        // Every iteration costs fuel, even an empty body.
                        self.fuel = self.fuel.checked_sub(1).ok_or(NotConstant)?;
                        frame.locals[*var] = Some(Const::Int(i));
                        self.stmts(body, frame)?;
                    }
                }
            }
        }
        Ok(result)
    }

    fn expr(&mut self, e: &TExpr, frame: &mut Frame) -> Eval<Const> {
        self.fuel = self.fuel.checked_sub(1).ok_or(NotConstant)?;
        Ok(match &e.kind {
            TExprKind::Int(v) => Const::Int(*v),
            TExprKind::Float(v) => Const::Float(*v),
            TExprKind::Str(s) => Const::Str(s.clone()),
            TExprKind::Bool(b) => Const::Bool(*b),
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
                let fits = |tests: &[Option<Ty>]| {
                    tests
                        .iter()
                        .zip(&args)
                        .all(|(t, a)| t.is_none_or(|t| t.contains(a.kind())))
                };
                // No definition accepts the values: a runtime error.
                let Some(case) = cases.iter().find(|c| fits(&c.tests)) else {
                    return Err(NotConstant);
                };
                self.call(case.inst, args)?
            }
            // A side effect: the global must run when the program runs.
            TExprKind::Echo(_) => return Err(NotConstant),
            // Some argument never finishes, so neither does this.
            TExprKind::Diverge(_) => return Err(NotConstant),
            TExprKind::Binary { op, lhs, rhs } => {
                let a = self.expr(lhs, frame)?;
                let b = self.expr(rhs, frame)?;
                binary(*op, &a, &b)?
            }
            TExprKind::Neg(x) => match self.expr(x, frame)? {
                Const::Int(v) => Const::Int(0i64.wrapping_sub(v)),
                Const::Float(v) => Const::Float(-0.0 - v),
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
            } => {
                let s = self.expr(scrutinee, frame)?;
                for (pattern, body) in arms {
                    let p = self.expr(pattern, frame)?;
                    if binary(BinOp::Eq, &s, &p)? == Const::Bool(true) {
                        return self.expr(body, frame);
                    }
                }
                match else_arm {
                    Some(body) => self.expr(body, frame)?,
                    None => unreachable!("typeck proved the arms cover every value"),
                }
            }
        })
    }
}

/// An operator on two values, exactly as the compiled code computes it.
fn binary(op: BinOp, a: &Const, b: &Const) -> Eval<Const> {
    use Const::*;
    if binop_result(op, a.kind(), b.kind()).is_none() {
        return Err(NotConstant); // a runtime operator error
    }
    let numeric = |c: &Const| matches!(c, Int(_) | Float(_));
    Ok(match op {
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
        BinOp::Eq | BinOp::Ne => {
            let eq = match (a, b) {
                (Int(x), Int(y)) => x == y,
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
