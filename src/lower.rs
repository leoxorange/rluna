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
//!
//! What lowering does: until now the program was a *tree* (nested
//! expressions). A CPU executes a *list* of simple instructions with jumps.
//! Lowering bridges the two:
//!
//! - nested expressions become sequences of one-operation instructions,
//!   each producing a named value: `a + b * c` becomes
//!   `%1 = imul %b, %c; %2 = iadd %a, %1`;
//! - `check` becomes basic blocks with conditional branches;
//! - Luna types become machine types (`i64`/`f64`) using the representation
//!   above, with boxing, unboxing and tag tests spelled out.
//!
//! Memory is freed here too, following the ownership rules checked by
//! `ownership`: lowering tracks which locals and parameters still own a
//! value and inserts a *drop* (`rt.free` for a string, `rt.drop_box` for a
//! box) where each owner is done with it. See "Ownership and drops" below.
//!
//! After this stage nothing knows about Luna anymore. LIR could just as well
//! have come from a C compiler, which is why the runtime can be written in
//! it and why backends for new CPUs only need to understand LIR.

use std::collections::HashMap;

use crate::ast::BinOp;
use crate::lir::{
    self, BlockId, Data, DataItem, FBinOp, FCond, FunctionBuilder, IBinOp, ICond, MemTy, Module,
    Type, Value,
};
use crate::ownership::Use;
use crate::typeck::{TExpr, TExprKind, TFunc, TStmt, Tir, binop_result};
use crate::types::{Kind, Ty};

/// Name of the function the runtime calls to run the program.
pub const ENTRY: &str = "luna.main";

/// Allocator header of objects that must never be freed.
const IMMORTAL: i64 = -1;

/// The machine type a Luna type is stored in. Everything except a plain
/// `float` fits in a 64-bit integer register: small values directly,
/// strings and boxes as pointers. Floats get `f64` because CPUs keep them
/// in separate floating-point registers and use different instructions.
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

    // A fixed-name wrapper around the user's `main`. The runtime always
    // calls `@luna.main`, whatever symbol typeck gave `main`'s instance and
    // whatever `main` returns (its value is ignored).
    // Its result is dropped like any discarded value.
    let main = &tir.funcs[tir.entry];
    let mut b = FunctionBuilder::new(ENTRY, &[], None);
    b.func.comment = Some("program entry: runs `main`".into());
    let ret = if main.discard {
        None
    } else {
        ret_repr(main.ret)
    };
    if let Some(v) = b.call(&main.symbol, Vec::new(), ret) {
        emit_drop(&mut b, v, main.ret);
    }
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
    ///
    /// String literals are not built at run time. Each distinct literal is
    /// stored once in the executable's data section, already in the runtime
    /// layout `{ len, bytes }`, and the code just takes its address.
    /// "Interning" means equal literals share one copy.
    ///
    /// The object is preceded by an allocator header marking it immortal
    /// (see `rt.alloc` in `runtime/core.lir`), so a literal can flow
    /// anywhere a heap string can, and freeing it is a harmless no-op.
    fn intern(&mut self, s: &str) -> String {
        if let Some(sym) = self.map.get(s) {
            return sym.clone();
        }
        let sym = format!("str.{}", self.data.len());
        self.data.push(Data {
            name: sym.clone(),
            items: vec![
                DataItem::I64(IMMORTAL),
                DataItem::I64(s.len() as i64),
                DataItem::Bytes(s.as_bytes().to_vec()),
            ],
        });
        self.map.insert(s.to_string(), sym.clone());
        sym
    }
}

/// Free a value that is no longer needed. Only heap values need it: a
/// string is a single allocation, a box may also own a string payload.
fn emit_drop(b: &mut FunctionBuilder, v: Value, ty: Ty) {
    if !ty.needs_drop() {
        return;
    }
    let free = if ty == Ty::STR {
        "rt.free"
    } else {
        "rt.drop_box"
    };
    b.call(free, vec![v], None);
}

/// Which parameters and locals currently own a value that must be dropped.
#[derive(Clone)]
struct Owned {
    params: Vec<bool>,
    locals: Vec<bool>,
}

impl Owned {
    /// Owners that still own their value on *both* paths.
    fn meet(&self, other: &Owned) -> Owned {
        let and = |a: &[bool], b: &[bool]| a.iter().zip(b).map(|(x, y)| *x && *y).collect();
        Owned {
            params: and(&self.params, &other.params),
            locals: and(&self.locals, &other.locals),
        }
    }
}

struct FnLower<'a> {
    tir: &'a Tir,
    f: &'a TFunc,
    strings: &'a mut Strings,
    b: FunctionBuilder,
    /// The LIR value holding each Luna local. In SSA form (see `lir`) a
    /// local is just a name for the value computed by its `x = ...`, so no
    /// memory is needed. Shadowing `x = x * 2` simply maps the new local id
    /// to the new value.
    locals: Vec<Option<Value>>,
    /// Owners whose value has not been moved yet at the current point.
    owned: Owned,
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
            // Arguments are moved into the call: the function owns them.
            owned: Owned {
                params: f.params.iter().map(|t| t.needs_drop()).collect(),
                locals: vec![false; f.local_tys.len()],
            },
        }
    }

    fn run(mut self) -> lir::Function {
        let returns = !self.f.discard && ret_repr(self.f.ret).is_some();
        let mut result = None;
        for (i, stmt) in self.f.body.iter().enumerate() {
            match stmt {
                TStmt::Let { local, value } => {
                    let v = self.expr(value, Use::Move);
                    self.locals[*local] = Some(v);
                    self.owned.locals[*local] = value.ty.needs_drop();
                }
                // The last statement's value is the result and moves to the
                // caller; the values of other statements are discarded.
                TStmt::Expr(e) => {
                    let v = self.expr(e, Use::Move);
                    if returns && i + 1 == self.f.body.len() {
                        result = Some((v, e.ty));
                    } else {
                        // Moved here (even a bare `x` line), so drop it.
                        emit_drop(&mut self.b, v, e.ty);
                    }
                }
            }
        }
        let result = result.map(|(v, ty)| self.convert(v, ty, self.f.ret));
        // The end of the function's scope: drop everything still owned.
        self.drop_owned(&self.owned.clone(), None);
        self.b.ret(result);
        self.b.finish()
    }

    // -- ownership and drops --------------------------------------------------
    //
    // Every heap value has exactly one owner, as checked by `ownership`:
    //
    // - a parameter or local owns its value until the value is moved away
    //   (`y = x`, `f x`, returned, or the result of a `check` arm); whatever
    //   is still owned when the function ends is dropped;
    // - a *temporary* (a fresh value from a call, an operator or a `check`)
    //   that is only read, as in `echo (a + b)`, is dropped right after
    //   that read;
    // - a value read through its owner's name (`echo x`) is *borrowed*: the
    //   owner keeps it and drops it later.
    //
    // All of this is decided at compile time. The generated code just calls
    // `rt.free` at the right places, with no reference counts or flags.

    /// Drop the value `v` of `e` after it has been read, unless it is
    /// borrowed from a parameter or local (whose owner drops it later) or is
    /// a string literal (which is immortal anyway).
    fn drop_temp(&mut self, e: &TExpr, v: Value) {
        if !matches!(
            e.kind,
            TExprKind::Param(_) | TExprKind::Local(_) | TExprKind::Str(_)
        ) {
            emit_drop(&mut self.b, v, e.ty);
        }
    }

    /// Drop the values of the owners in `owned`, except those also in
    /// `keep`.
    fn drop_owned(&mut self, owned: &Owned, keep: Option<&Owned>) {
        for (i, &own) in owned.params.iter().enumerate() {
            if own && !keep.is_some_and(|k| k.params[i]) {
                let v = self.b.param(i);
                emit_drop(&mut self.b, v, self.f.params[i]);
            }
        }
        for (l, &own) in owned.locals.iter().enumerate() {
            if own && !keep.is_some_and(|k| k.locals[l]) {
                let v = self.locals[l].expect("owned local has a value");
                emit_drop(&mut self.b, v, self.f.local_tys[l]);
            }
        }
    }

    /// A placeholder value for code that can never run (an expression of
    /// type `never`, or a call to a `void` function used as a value). The
    /// IR still needs *some* value of the right machine type to stay well
    /// formed, even though nothing will ever read it.
    fn dummy(&mut self, ty: Ty) -> Value {
        match repr(ty) {
            Type::F64 => self.b.fconst(0.0),
            Type::I64 => self.b.iconst(0),
        }
    }

    /// The address of a string literal's object, just past its header.
    fn string_const(&mut self, s: &str) -> Value {
        let sym = self.strings.intern(s);
        let header = self.b.addr(&sym);
        let header_size = self.b.iconst(8);
        self.b.ibin(IBinOp::Add, header, header_size)
    }

    /// Lower an expression whose value is used as `usage` says: `Move` when
    /// the value is given away (to a binding, a call, the caller), `Borrow`
    /// when it is only read (by an operator, `echo`, `check`).
    fn expr(&mut self, e: &TExpr, usage: Use) -> Value {
        let moved = usage == Use::Move && e.ty.needs_drop();
        match &e.kind {
            TExprKind::Int(v) => self.b.iconst(*v),
            TExprKind::Float(v) => self.b.fconst(*v),
            TExprKind::Str(s) => self.string_const(s),
            TExprKind::Bool(v) => self.b.iconst(*v as i64),
            TExprKind::Param(i) => {
                if moved {
                    self.owned.params[*i] = false;
                }
                self.b.param(*i)
            }
            TExprKind::Local(l) => {
                if moved {
                    self.owned.locals[*l] = false;
                }
                self.locals[*l].expect("local used before definition")
            }
            TExprKind::Call { inst, args } => {
                let args: Vec<Value> = args.iter().map(|a| self.expr(a, Use::Move)).collect();
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
                    self.expr(a, Use::Move);
                }
                self.dummy(e.ty)
            }
            TExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr(lhs, Use::Borrow);
                let r = self.expr(rhs, Use::Borrow);
                if e.ty.is_never() {
                    return self.dummy(e.ty);
                }
                let result = self.binary(*op, l, lhs.ty, r, rhs.ty, e.ty);
                // The operands were only read; the result is a new value.
                self.drop_temp(lhs, l);
                self.drop_temp(rhs, r);
                result
            }
            TExprKind::Neg(inner) => {
                let v = self.expr(inner, Use::Borrow);
                if e.ty.is_never() {
                    return self.dummy(e.ty);
                }
                let result_ty = e.ty;
                let result = self.dispatch(v, inner.ty, result_ty, |this, kind, v| match kind {
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
                });
                self.drop_temp(inner, v);
                result
            }
            TExprKind::Echo(inner) => {
                let v = self.expr(inner, Use::Borrow);
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
                    self.drop_temp(inner, v);
                }
                self.b.iconst(0)
            }
            // `check` becomes a chain of tests, like if / else if / else:
            //
            //   test arm 1 --yes--> arm1 body --+
            //       |no                         |
            //   test arm 2 --yes--> arm2 body --+--> check_end(result)
            //       |no                         |
            //   else body ----------------------+
            //
            // Every arm jumps to one merge block and passes its value as the
            // block's parameter. That parameter is the result of the whole
            // `check` (in other IRs this is called a "phi node").
            //
            // Ownership across the arms: each arm starts from the owners as
            // they were before it, and may move some of them. After the
            // `check`, an owner counts as moved if *any* arm moved it (the
            // ownership checker forbids using it again). So an arm that did
            // not move it drops it before jumping to the merge block, and
            // every path leaves the same owners behind.
            TExprKind::Check {
                scrutinee,
                arms,
                else_arm,
            } => {
                let s = self.expr(scrutinee, Use::Borrow);
                let merge = self.b.new_block("check_end");
                let result = self.b.block_param(merge, repr(e.ty));
                // (block where the arm ends, its value, owners at its end)
                let mut ends: Vec<(BlockId, Value, Owned)> = Vec::new();
                for (pat, body) in arms {
                    let p = self.expr(pat, Use::Borrow);
                    let cond = if scrutinee.ty.is_never() || pat.ty.is_never() {
                        self.b.iconst(0)
                    } else {
                        self.binary(BinOp::Eq, s, scrutinee.ty, p, pat.ty, Ty::BOOL)
                    };
                    self.drop_temp(pat, p);
                    let arm = self.b.new_block("arm");
                    let next = self.b.new_block("next");
                    self.b.br(cond, arm, vec![], next, vec![]);
                    self.b.switch_to(arm);
                    let before = self.owned.clone();
                    let v = self.expr(body, Use::Move);
                    if !self.b.is_terminated() {
                        ends.push((self.b.current(), v, self.owned.clone()));
                    }
                    self.owned = before;
                    self.b.switch_to(next);
                }
                match else_arm {
                    Some(body) => {
                        let v = self.expr(body, Use::Move);
                        if !self.b.is_terminated() {
                            ends.push((self.b.current(), v, self.owned.clone()));
                        }
                    }
                    // Typeck proved a bool `check` covers both values, so
                    // falling through every arm cannot happen. `unreachable`
                    // says so (and traps if the proof were ever wrong).
                    None => self.b.unreachable(),
                }
                let after = ends
                    .iter()
                    .map(|(_, _, owned)| owned.clone())
                    .reduce(|a, b| a.meet(&b))
                    .unwrap_or_else(|| self.owned.clone());
                for (block, v, owned) in ends {
                    self.b.switch_to(block);
                    self.drop_owned(&owned, Some(&after));
                    self.b.jmp(merge, vec![v]);
                }
                self.owned = after;
                self.b.switch_to(merge);
                if !scrutinee.ty.is_never() {
                    self.drop_temp(scrutinee, s);
                }
                result
            }
            TExprKind::Convert(inner) => {
                // Conversions only appear where the value is moved (call
                // arguments, `check` arms, return values), so `v` is owned.
                let v = self.expr(inner, usage);
                self.convert(v, inner.ty, e.ty)
            }
        }
    }

    // -- representation changes -------------------------------------------

    /// Wrap a concrete value in a heap-allocated `{ tag, payload }` box.
    fn box_value(&mut self, kind: Kind, v: Value) -> Value {
        let tag = self.b.iconst(kind.tag());
        // The payload slot is an i64. A float's 64 bits are stored
        // unchanged ("bitcast"), not converted to an integer: 2.5 stays
        // 2.5 when read back as f64.
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
    ///
    /// The test reuses the bit-set encoding of `Ty`: kind with tag t is bit
    /// t-1, so `(1 << (tag - 1)) & mask` is non-zero exactly when the
    /// kind is allowed. One test covers any set of kinds, with no chain of
    /// comparisons.
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
    ///
    /// `v` is owned and is consumed: a boxed value becomes owned by the new
    /// box, and unboxing frees the box once the payload is taken out (the
    /// payload, if a string, now belongs to the result).
    fn convert(&mut self, v: Value, from: Ty, to: Ty) -> Value {
        if from == to {
            return v;
        }
        if from.is_never() {
            return self.dummy(to);
        }
        // Four cases, by whether each side is a single kind (unboxed) or a
        // union (boxed):
        //   single -> union:  box it (always safe: typeck checked to ⊇ from)
        //   union  -> single: check the tag at run time, then unbox
        //   union  -> union:  same box; check the tag only if `to` is smaller
        //   single -> single: same kind, nothing to do
        match (from.as_kind(), to.as_kind()) {
            (Some(k), None) => {
                debug_assert!(to.contains(k));
                self.box_value(k, v)
            }
            (None, Some(k)) => {
                let tag = self.b.load(MemTy::I64, v, 0);
                self.check_tag(tag, to);
                let payload = self.unbox_payload(k, v);
                self.b.call("rt.free", vec![v], None);
                payload
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
    ///
    /// This is the heart of how dynamic typing is compiled. For a union value
    /// it generates "if tag == int { ... } else if tag == float { ... }",
    /// and inside each branch the value is unboxed to a concrete kind.
    /// `f` then only ever handles concrete kinds and emits the fast code
    /// for them. For a value of a single kind there is no test at all, which
    /// is why fully typed code pays nothing for this flexibility.
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
            // The last possible kind needs no test: if the tag was none of
            // the others, it must be this one.
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

    /// A binary operator: dispatch on the left kind, then (inside each case)
    /// on the right kind, then emit the concrete operation for that pair.
    /// For `int + int` both dispatches collapse to nothing and this emits a
    /// single `iadd`. For `any + any` it emits a 5×5 decision tree whose
    /// invalid combinations call `rt.op_error`.
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

    /// The operation for two known kinds: the actual machine-level meaning
    /// of each Luna operator. Mixed int/float arithmetic converts the int
    /// first (`sitofp`), because the CPU has no "add int to float"
    /// instruction.
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
                    // Values of different kinds are never equal.
                    self.b.iconst(0)
                };
                // `a != b` is `(a == b) xor 1`: comparisons produce 0 or 1,
                // so xor with 1 flips the boolean without a branch.
                if op == BinOp::Ne {
                    let one = self.b.iconst(1);
                    self.b.ibin(IBinOp::Xor, eq, one)
                } else {
                    eq
                }
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                // Strings are compared by a runtime function returning
                // -1/0/1 (like C's strcmp). Comparing that result with 0
                // using the same operator turns all four of < <= > >= into
                // one call plus one integer compare.
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
