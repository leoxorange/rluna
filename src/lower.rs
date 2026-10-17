//! Lowering: TIR -> LIR.
//!
//! Value representation:
//! - `int`, `boolean` (0/1), `void` (always 0): `i64`
//! - `float`: `f64`
//! - `string`: `i64` pointer to `{ len: i64, bytes... }`
//! - union types (`numeric`, `any`, ...): `i64` pointer to a box
//!   `{ tag: i64, payload: i64 }` (see `Kind::tag`)
//!
//! Operations on unions dispatch on the box tag and reuse the code for
//! concrete kinds.

use std::collections::HashMap;

use crate::ast::BinOp;
use crate::lir::{
    self, Data, DataItem, FBinOp, FCond, FunctionBuilder, IBinOp, ICond, MemTy, Module, Type, Value,
};
use crate::typeck::{TExpr, TExprKind, TFunc, TStmt, Tir, binop_result};
use crate::types::{Kind, Ty};

/// Name of the function the runtime calls to run the program.
pub const ENTRY: &str = "luna.main";

pub fn repr(ty: Ty) -> Type {
    if ty == Ty::FLOAT {
        Type::F64
    } else {
        Type::I64
    }
}

fn ret_repr(ty: Ty) -> Option<Type> {
    if ty == Ty::VOID || ty.is_never() {
        None
    } else {
        Some(repr(ty))
    }
}

pub fn lower(tir: &Tir) -> Module {
    let mut strings = Strings::default();
    let mut module = Module::default();
    for f in &tir.funcs {
        let func = FnLower::new(tir, f, &mut strings).run();
        module.funcs.push(func);
    }

    let mut b = FunctionBuilder::new(ENTRY, &[], None);
    b.func.comment = Some("program entry: runs `main`".into());
    b.call(
        &tir.funcs[tir.entry].symbol,
        Vec::new(),
        ret_repr(tir.funcs[tir.entry].ret),
    );
    b.ret(None);
    module.funcs.push(b.finish());

    module.data = strings.data;
    module
}

#[derive(Default)]
struct Strings {
    map: HashMap<String, String>,
    data: Vec<Data>,
}

impl Strings {
    /// Symbol of a constant string object with the given contents.
    fn intern(&mut self, s: &str) -> String {
        if let Some(sym) = self.map.get(s) {
            return sym.clone();
        }
        let sym = format!("str.{}", self.data.len());
        self.data.push(Data {
            name: sym.clone(),
            items: vec![
                DataItem::I64(s.len() as i64),
                DataItem::Bytes(s.as_bytes().to_vec()),
            ],
        });
        self.map.insert(s.to_string(), sym.clone());
        sym
    }
}

struct FnLower<'a> {
    tir: &'a Tir,
    f: &'a TFunc,
    strings: &'a mut Strings,
    b: FunctionBuilder,
    locals: Vec<Option<Value>>,
}

impl<'a> FnLower<'a> {
    fn new(tir: &'a Tir, f: &'a TFunc, strings: &'a mut Strings) -> FnLower<'a> {
        let params: Vec<Type> = f.params.iter().map(|&t| repr(t)).collect();
        let ret = if f.discard { None } else { ret_repr(f.ret) };
        let mut b = FunctionBuilder::new(f.symbol.clone(), &params, ret);
        b.func.comment = Some(f.signature());
        FnLower {
            tir,
            f,
            strings,
            b,
            locals: vec![None; f.local_tys.len()],
        }
    }

    fn run(mut self) -> lir::Function {
        let mut last = None;
        for stmt in &self.f.body {
            match stmt {
                TStmt::Let { local, value } => {
                    let v = self.expr(value);
                    self.locals[*local] = Some(v);
                    last = None;
                }
                TStmt::Expr(e) => last = Some((self.expr(e), e.ty)),
            }
        }
        if self.f.discard || ret_repr(self.f.ret).is_none() {
            self.b.ret(None);
        } else {
            let (v, ty) = last.expect("non-void function ends with an expression");
            let v = self.convert(v, ty, self.f.ret);
            self.b.ret(Some(v));
        }
        self.b.finish()
    }

    fn dummy(&mut self, ty: Ty) -> Value {
        match repr(ty) {
            Type::F64 => self.b.fconst(0.0),
            Type::I64 => self.b.iconst(0),
        }
    }

    fn string_const(&mut self, s: &str) -> Value {
        let sym = self.strings.intern(s);
        self.b.addr(&sym)
    }

    fn expr(&mut self, e: &TExpr) -> Value {
        match &e.kind {
            TExprKind::Int(v) => self.b.iconst(*v),
            TExprKind::Float(v) => self.b.fconst(*v),
            TExprKind::Str(s) => self.string_const(s),
            TExprKind::Bool(v) => self.b.iconst(*v as i64),
            TExprKind::Param(i) => self.b.param(*i),
            TExprKind::Local(l) => self.locals[*l].expect("local used before definition"),
            TExprKind::Call { inst, args } => {
                let args: Vec<Value> = args.iter().map(|a| self.expr(a)).collect();
                let callee = &self.tir.funcs[*inst];
                let ret = if callee.discard {
                    None
                } else {
                    ret_repr(callee.ret)
                };
                match self.b.call(&callee.symbol, args, ret) {
                    Some(v) => v,
                    None => self.dummy(e.ty),
                }
            }
            TExprKind::Diverge(args) => {
                for a in args {
                    self.expr(a);
                }
                self.dummy(e.ty)
            }
            TExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr(lhs);
                let r = self.expr(rhs);
                if e.ty.is_never() {
                    return self.dummy(e.ty);
                }
                self.binary(*op, l, lhs.ty, r, rhs.ty, e.ty)
            }
            TExprKind::Neg(inner) => {
                let v = self.expr(inner);
                if e.ty.is_never() {
                    return self.dummy(e.ty);
                }
                let result_ty = e.ty;
                self.dispatch(v, inner.ty, result_ty, |this, kind, v| match kind {
                    Kind::Int => {
                        let zero = this.b.iconst(0);
                        Some((Ty::INT, this.b.ibin(IBinOp::Sub, zero, v)))
                    }
                    Kind::Float => {
                        let zero = this.b.fconst(-0.0);
                        Some((Ty::FLOAT, this.b.fbin(FBinOp::Sub, zero, v)))
                    }
                    _ => {
                        let msg = this.string_const("-");
                        let tag = this.b.iconst(kind.tag());
                        let none = this.b.iconst(0);
                        this.b.call("rt.op_error", vec![msg, tag, none], None);
                        None
                    }
                })
            }
            TExprKind::Echo(inner) => {
                let v = self.expr(inner);
                if !inner.ty.is_never() {
                    self.dispatch(v, inner.ty, Ty::VOID, |this, kind, v| {
                        match kind {
                            Kind::Int => this.b.call("rt.print_int", vec![v], None),
                            Kind::Float => this.b.call("rt.print_float", vec![v], None),
                            Kind::Str => this.b.call("rt.print_str", vec![v], None),
                            Kind::Bool => this.b.call("rt.print_bool", vec![v], None),
                            Kind::Void => this.b.call("rt.print_void", vec![], None),
                        };
                        let z = this.b.iconst(0);
                        Some((Ty::VOID, z))
                    });
                    self.b.call("rt.print_newline", vec![], None);
                }
                self.b.iconst(0)
            }
            TExprKind::Check {
                scrutinee,
                arms,
                else_arm,
            } => {
                let s = self.expr(scrutinee);
                let merge = self.b.new_block("check_end");
                let result = self.b.block_param(merge, repr(e.ty));
                for (pat, body) in arms {
                    let p = self.expr(pat);
                    let cond = if scrutinee.ty.is_never() || pat.ty.is_never() {
                        self.b.iconst(0)
                    } else {
                        self.binary(BinOp::Eq, s, scrutinee.ty, p, pat.ty, Ty::BOOL)
                    };
                    let arm = self.b.new_block("arm");
                    let next = self.b.new_block("next");
                    self.b.br(cond, arm, vec![], next, vec![]);
                    self.b.switch_to(arm);
                    let v = self.expr(body);
                    self.b.jmp(merge, vec![v]);
                    self.b.switch_to(next);
                }
                match else_arm {
                    Some(body) => {
                        let v = self.expr(body);
                        self.b.jmp(merge, vec![v]);
                    }
                    None => self.b.unreachable(),
                }
                self.b.switch_to(merge);
                result
            }
            TExprKind::Convert(inner) => {
                let v = self.expr(inner);
                self.convert(v, inner.ty, e.ty)
            }
        }
    }

    // -- representation changes -------------------------------------------

    fn box_value(&mut self, kind: Kind, v: Value) -> Value {
        let tag = self.b.iconst(kind.tag());
        let payload = if kind == Kind::Float {
            self.b.bitcast(v, Type::I64)
        } else {
            v
        };
        self.b
            .call("rt.box", vec![tag, payload], Some(Type::I64))
            .unwrap()
    }

    fn unbox_payload(&mut self, kind: Kind, boxed: Value) -> Value {
        let mt = if kind == Kind::Float {
            MemTy::F64
        } else {
            MemTy::I64
        };
        self.b.load(mt, boxed, 8)
    }

    /// Calls the runtime's type error if `tag` is not in `allowed`.
    fn check_tag(&mut self, tag: Value, allowed: Ty) {
        let ok = self.b.new_block("tag_ok");
        let bad = self.b.new_block("tag_bad");
        let one = self.b.iconst(1);
        let shift = self.b.ibin(IBinOp::Sub, tag, one);
        let bit = self.b.ibin(IBinOp::Shl, one, shift);
        let mask = self.b.iconst(allowed.mask());
        let hit = self.b.ibin(IBinOp::And, bit, mask);
        self.b.br(hit, ok, vec![], bad, vec![]);
        self.b.switch_to(bad);
        let expected = self.string_const(&allowed.to_string());
        self.b.call("rt.type_error", vec![expected, tag], None);
        self.b.unreachable();
        self.b.switch_to(ok);
    }

    /// Convert a value of type `from` to type `to`. When `to` does not contain
    /// `from` this checks the value's kind at runtime.
    fn convert(&mut self, v: Value, from: Ty, to: Ty) -> Value {
        if from == to {
            return v;
        }
        if from.is_never() {
            return self.dummy(to);
        }
        match (from.as_kind(), to.as_kind()) {
            (Some(k), None) => {
                debug_assert!(to.contains(k));
                self.box_value(k, v)
            }
            (None, Some(k)) => {
                let tag = self.b.load(MemTy::I64, v, 0);
                self.check_tag(tag, to);
                self.unbox_payload(k, v)
            }
            (None, None) => {
                if !from.is_subset(to) {
                    let tag = self.b.load(MemTy::I64, v, 0);
                    self.check_tag(tag, to);
                }
                v
            }
            (Some(a), Some(b)) => {
                debug_assert_eq!(a, b);
                v
            }
        }
    }

    /// Run `f` once per kind that `v: ty` can have, with `v` unboxed to that
    /// kind. `f` returns the kind and value of its result, or `None` if it
    /// ended its block (e.g. with a runtime error). Results are joined into
    /// a value of type `result`.
    fn dispatch(
        &mut self,
        v: Value,
        ty: Ty,
        result: Ty,
        mut f: impl FnMut(&mut Self, Kind, Value) -> Option<(Ty, Value)>,
    ) -> Value {
        if let Some(kind) = ty.as_kind() {
            return match f(self, kind, v) {
                Some((t, r)) => self.convert(r, t, result),
                None => {
                    // The only path failed; continue in a dead block.
                    if !self.b.is_terminated() {
                        self.b.unreachable();
                    }
                    let dead = self.b.new_block("dead");
                    self.b.switch_to(dead);
                    self.dummy(result)
                }
            };
        }
        let merge = self.b.new_block("join");
        let out = self.b.block_param(merge, repr(result));
        let tag = self.b.load(MemTy::I64, v, 0);
        let kinds: Vec<Kind> = ty.kinds().collect();
        for (i, &kind) in kinds.iter().enumerate() {
            let case = self.b.new_block(&format!("is_{}", kind.name()));
            if i + 1 < kinds.len() {
                let next = self.b.new_block("not");
                let k = self.b.iconst(kind.tag());
                let is = self.b.icmp(ICond::Eq, tag, k);
                self.b.br(is, case, vec![], next, vec![]);
                self.b.switch_to(case);
                self.dispatch_case(kind, v, result, merge, &mut f);
                self.b.switch_to(next);
            } else {
                self.b.jmp(case, vec![]);
                self.b.switch_to(case);
                self.dispatch_case(kind, v, result, merge, &mut f);
            }
        }
        self.b.switch_to(merge);
        out
    }

    fn dispatch_case(
        &mut self,
        kind: Kind,
        boxed: Value,
        result: Ty,
        merge: lir::BlockId,
        f: &mut impl FnMut(&mut Self, Kind, Value) -> Option<(Ty, Value)>,
    ) {
        let payload = self.unbox_payload(kind, boxed);
        match f(self, kind, payload) {
            Some((t, r)) => {
                let r = self.convert(r, t, result);
                self.b.jmp(merge, vec![r]);
            }
            None => {
                if !self.b.is_terminated() {
                    self.b.unreachable();
                }
            }
        }
    }

    // -- operators ----------------------------------------------------------

    fn binary(&mut self, op: BinOp, l: Value, lt: Ty, r: Value, rt: Ty, result: Ty) -> Value {
        self.dispatch(l, lt, result, |this, ka, lv| {
            let inner = this.dispatch(r, rt, result, |this, kb, rv| {
                match binop_result(op, ka, kb) {
                    Some(rk) => Some((Ty::of(rk), this.concrete_binary(op, ka, lv, kb, rv))),
                    None => {
                        let msg = this.string_const(op.as_str());
                        let ta = this.b.iconst(ka.tag());
                        let tb = this.b.iconst(kb.tag());
                        this.b.call("rt.op_error", vec![msg, ta, tb], None);
                        this.b.unreachable();
                        None
                    }
                }
            });
            Some((result, inner))
        })
    }

    fn concrete_binary(&mut self, op: BinOp, ka: Kind, a: Value, kb: Kind, b: Value) -> Value {
        use Kind::*;
        let to_float =
            |this: &mut Self, k: Kind, v: Value| if k == Int { this.b.sitofp(v) } else { v };
        let numeric = |k: Kind| matches!(k, Int | Float);
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                if ka == Str {
                    return self
                        .b
                        .call("rt.str_concat", vec![a, b], Some(Type::I64))
                        .unwrap();
                }
                if ka == Int && kb == Int && op != BinOp::Div {
                    let iop = match op {
                        BinOp::Add => IBinOp::Add,
                        BinOp::Sub => IBinOp::Sub,
                        _ => IBinOp::Mul,
                    };
                    return self.b.ibin(iop, a, b);
                }
                let fa = to_float(self, ka, a);
                let fb = to_float(self, kb, b);
                let fop = match op {
                    BinOp::Add => FBinOp::Add,
                    BinOp::Sub => FBinOp::Sub,
                    BinOp::Mul => FBinOp::Mul,
                    _ => FBinOp::Div,
                };
                self.b.fbin(fop, fa, fb)
            }
            BinOp::Eq | BinOp::Ne => {
                let eq = if ka == Int && kb == Int || ka == Bool && kb == Bool {
                    self.b.icmp(ICond::Eq, a, b)
                } else if numeric(ka) && numeric(kb) {
                    let fa = to_float(self, ka, a);
                    let fb = to_float(self, kb, b);
                    self.b.fcmp(FCond::Eq, fa, fb)
                } else if ka == Str && kb == Str {
                    self.b
                        .call("rt.str_eq", vec![a, b], Some(Type::I64))
                        .unwrap()
                } else {
                    self.b.iconst((ka == Void && kb == Void) as i64)
                };
                if op == BinOp::Ne {
                    let one = self.b.iconst(1);
                    self.b.ibin(IBinOp::Xor, eq, one)
                } else {
                    eq
                }
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if ka == Str {
                    let c = self
                        .b
                        .call("rt.str_cmp", vec![a, b], Some(Type::I64))
                        .unwrap();
                    let zero = self.b.iconst(0);
                    return self.b.icmp(icond(op), c, zero);
                }
                if ka == Int && kb == Int {
                    return self.b.icmp(icond(op), a, b);
                }
                let fa = to_float(self, ka, a);
                let fb = to_float(self, kb, b);
                let c = match op {
                    BinOp::Lt => FCond::Lt,
                    BinOp::Le => FCond::Le,
                    BinOp::Gt => FCond::Gt,
                    _ => FCond::Ge,
                };
                self.b.fcmp(c, fa, fb)
            }
        }
    }
}

fn icond(op: BinOp) -> ICond {
    match op {
        BinOp::Lt => ICond::Slt,
        BinOp::Le => ICond::Sle,
        BinOp::Gt => ICond::Sgt,
        _ => ICond::Sge,
    }
}
