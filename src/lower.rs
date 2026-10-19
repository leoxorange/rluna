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

use std::collections::{HashMap, HashSet};

use crate::ast::BinOp;
use crate::hir::{HPattern, Native};
use crate::lir::{
    self, BlockId, Data, DataItem, FBinOp, FCond, FunctionBuilder, IBinOp, ICond, MemTy, Module,
    Type, Value,
};
use crate::ownership::Use;
use crate::typeck::{
    LenTest, LoopSource, TClause, TExpr, TExprKind, TFunc, TStmt, Tir, binop_result,
    gives_element_itself, visit_children,
};
use crate::typefns::{TypeFns, load_elem, load_field, new_record, store_elem, store_field};
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
    let mut fns = TypeFns::default();
    fns.trails = tir.trails.clone();
    let mut module = Module::default();
    for f in &tir.funcs {
        let func = FnLower::new(tir, f, &mut strings, &mut fns).run();
        module.funcs.push(func);
    }
    let names: Vec<String> = tir.funcs.iter().map(|f| f.name.clone()).collect();
    guard_recursion(&mut module.funcs, &names, &mut strings);

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
        fns.drop(&mut b, v, main.ret);
    }
    b.ret(None);
    module.funcs.push(b.finish());
    module.funcs.push(out_hook(fns.captures));
    fns.finish(&mut module);

    module.data = strings.data;
    module
}

// -- stack overflow -------------------------------------------------------------

/// Every call takes some stack, which ends; past its end the program would
/// crash without a word. Only a function that can call itself again,
/// directly or through others, can go arbitrarily deep, so each of those
/// first checks that the stack has not come down to `rt.stack_limit`, and
/// otherwise stops the program with a `StackOverflowError` naming it. (A
/// call in tail position does not go deeper: see `tail_calls`.)
fn guard_recursion(funcs: &mut [lir::Function], names: &[String], strings: &mut Strings) {
    let index: HashMap<String, usize> = funcs
        .iter()
        .enumerate()
        .map(|(k, f)| (f.name.clone(), k))
        .collect();
    let callees: Vec<Vec<usize>> = funcs
        .iter()
        .map(|f| {
            f.blocks
                .iter()
                .flat_map(|b| &b.insts)
                .filter_map(|i| match &i.op {
                    lir::Op::Call(name, _) => index.get(name).copied(),
                    _ => None,
                })
                .collect()
        })
        .collect();
    for k in 0..funcs.len() {
        // Can `k` be reached from what it calls?
        let mut seen = vec![false; funcs.len()];
        let mut todo = callees[k].clone();
        let mut recursive = false;
        while let Some(c) = todo.pop() {
            if c == k {
                recursive = true;
                break;
            }
            if !std::mem::replace(&mut seen[c], true) {
                todo.extend(&callees[c]);
            }
        }
        if recursive {
            funcs[k] = guard_stack(funcs[k].clone(), &names[k], strings);
        }
    }
}

/// Put the stack check in front of `func`'s entry block. The address of a
/// slot in its frame tells how far down the stack is.
fn guard_stack(func: lir::Function, name: &str, strings: &mut Strings) -> lir::Function {
    let mut b = FunctionBuilder::resume(func);
    let ok = b.new_block("stack_ok");
    let overflow = b.new_block("stack_overflow");
    let entry = &mut b.func.blocks[0];
    let (insts, term) = (std::mem::take(&mut entry.insts), entry.term.take());
    let block = &mut b.func.blocks[ok.0 as usize];
    (block.insts, block.term) = (insts, term);
    let here = b.alloca(8);
    let slot = b.addr("rt.stack_limit");
    let limit = b.load(MemTy::I64, slot, 0);
    let room = b.icmp(ICond::Uge, here, limit);
    b.br(room, ok, vec![], overflow, vec![]);
    b.switch_to(overflow);
    let sym = strings.intern(name);
    let header = b.addr(&sym);
    let header_size = b.iconst(8);
    let name = b.ibin(IBinOp::Add, header, header_size);
    b.call("rt.stack_overflow", vec![name], None);
    b.unreachable();
    b.finish()
}

// -- tail calls ---------------------------------------------------------------

/// The calls whose value is the function's result, with nothing left to do
/// after them but the drops: the last expression of the body and every
/// `ret` value, and inside them the arms of a `check` and the last
/// expression of a block. Only a call giving exactly the function's result
/// type counts (no conversion follows it). Also whether one of them may
/// call `f` itself in a way that reuses its frame (see `reuses_frame`).
fn tail_calls(tir: &Tir, f: &TFunc, assigned: &[bool]) -> (HashSet<*const TExpr>, bool) {
    struct Scan<'a> {
        tir: &'a Tir,
        f: &'a TFunc,
        assigned: &'a [bool],
        out: HashSet<*const TExpr>,
        reuses: bool,
    }
    impl Scan<'_> {
        fn mark(&mut self, e: &TExpr) {
            let (tir, f, assigned) = (self.tir, self.f, self.assigned);
            match &e.kind {
                TExprKind::Call { inst, args } if f.discard || e.ty == f.ret => {
                    self.out.insert(e);
                    self.reuses |=
                        reuses_frame(tir, f, assigned, *inst, args, &tir.funcs[*inst].params);
                }
                TExprKind::Dispatch { args, cases, .. } if f.discard || e.ty == f.ret => {
                    self.out.insert(e);
                    self.reuses |= cases
                        .iter()
                        .any(|c| reuses_frame(tir, f, assigned, c.inst, args, &c.params));
                }
                TExprKind::Check { arms, else_arm, .. } => {
                    for (_, body) in arms {
                        self.mark(body);
                    }
                    if let Some(e) = else_arm {
                        self.mark(e);
                    }
                }
                TExprKind::Block { stmts, .. } => {
                    if let Some(TStmt::Expr(e)) = stmts.last() {
                        self.mark(e);
                    }
                }
                _ => {}
            }
        }

        fn rets(&mut self, e: &TExpr) {
            if let TExprKind::Ret(Some(v)) = &e.kind {
                self.mark(v);
            }
            visit_children(e, &mut |c| self.rets(c));
        }
    }
    let mut scan = Scan {
        tir,
        f,
        assigned,
        out: HashSet::new(),
        reuses: false,
    };
    if let Some(TStmt::Expr(e)) = f.body.last() {
        scan.mark(e);
    }
    crate::typeck::for_each_expr(&f.body, &mut |e| scan.rets(e));
    (scan.out, scan.reuses)
}

/// Can a call of `inst` in tail position (see `tail_calls`) start `f`'s
/// body again instead? It must call `f` itself, and the drops before the
/// jump must not free what it borrows: a borrowed argument must be a value
/// that outlives `f`'s frame (a number, a string literal, a borrowed
/// parameter, or part of one), already of the parameter's type.
fn reuses_frame(
    tir: &Tir,
    f: &TFunc,
    assigned: &[bool],
    inst: usize,
    args: &[TExpr],
    params: &[Ty],
) -> bool {
    fn lasting(a: &TExpr, f: &TFunc, assigned: &[bool]) -> bool {
        if !a.ty.needs_drop() {
            return true;
        }
        match &a.kind {
            TExprKind::Str(_) => true,
            TExprKind::Local(l) => *l < f.params.len() && f.borrowed[*l] && !assigned[*l],
            TExprKind::Index(x, _) if gives_element_itself(x.ty) => lasting(x, f, assigned),
            TExprKind::Field(base, _) if !a.ty.is_copy() => lasting(base, f, assigned),
            _ => false,
        }
    }
    std::ptr::eq(&tir.funcs[inst], f)
        && args
            .iter()
            .zip(params)
            .zip(&f.borrowed)
            .all(|((a, &to), &b)| !b || (a.ty == to && lasting(a, f, assigned)))
}

/// Whether evaluating `e` may leave it halfway, through a `ret`, `over` or
/// `jump` inside. (One that belongs to a loop inside `e` does not, but
/// counting it only costs a pending entry.)
fn may_leave(e: &TExpr) -> bool {
    let mut found = matches!(
        e.kind,
        TExprKind::Ret(_) | TExprKind::Over | TExprKind::Jump
    );
    if !found {
        visit_children(e, &mut |c| found |= may_leave(c));
    }
    found
}

/// Mark the variables the statements give values, at any depth.
fn assigned_locals(stmts: &[TStmt], out: &mut [bool]) {
    fn expr(e: &TExpr, out: &mut [bool]) {
        if let TExprKind::Block { stmts, .. } = &e.kind {
            assigned_locals(stmts, out);
        } else {
            visit_children(e, &mut |c| expr(c, out));
        }
    }
    for s in stmts {
        match s {
            TStmt::Let { local, value } => {
                out[*local] = true;
                expr(value, out);
            }
            TStmt::Expr(e) => expr(e, out),
            TStmt::Loop { source, body, .. } => {
                source.exprs().into_iter().for_each(|e| expr(e, out));
                assigned_locals(body, out);
            }
        }
    }
}

/// `rt.out_hook`, which the runtime's `rt.out` calls with every piece of
/// output (see `runtime/core.lir`): it gives the bytes to the string being
/// built, if the program prints values into strings and one is being
/// built, and returns 1; otherwise it returns 0.
pub fn out_hook(captures: bool) -> lir::Function {
    let mut b = FunctionBuilder::new("rt.out_hook", &[Type::I64, Type::I64], Some(Type::I64));
    b.func.comment = Some("where output goes: the terminal, or a string being built".into());
    let zero = b.iconst(0);
    if captures {
        let (p, n) = (b.param(0), b.param(1));
        let capture = b.addr("rt.capture");
        let building = b.load(MemTy::I64, capture, 0);
        let capturing = b.icmp(ICond::Ne, building, zero);
        let take = b.new_block("take");
        let leave = b.new_block("leave");
        b.br(capturing, take, vec![], leave, vec![]);
        b.switch_to(take);
        b.call("rt.capture_out", vec![p, n], None);
        let one = b.iconst(1);
        b.ret(Some(one));
        b.switch_to(leave);
    }
    b.ret(Some(zero));
    b.finish()
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

/// Going through a loop or comprehension source (see `FnLower::iter_begin`).
struct Iter<'s> {
    kind: IterKind,
    /// The counter's first state.
    init: Value,
    /// The type of the elements.
    elem: Ty,
    /// The source moved into the loop: each element moves out of it into
    /// the variable. Otherwise the source is borrowed, and so is each
    /// element.
    moving: bool,
    /// The source, if the loop owns it (it moved in, or it is a temporary)
    /// and must free it at the end.
    owned: Option<&'s TExpr>,
}

impl Iter<'_> {
    /// The variable owns each element: one moved out of the source, or a
    /// character of a string (a new string either way).
    fn owns_elements(&self) -> bool {
        self.elem.needs_drop() && (self.moving || matches!(self.kind, IterKind::Str { .. }))
    }
}

/// A value that must be freed later, by the code that finishes with it, or
/// earlier if a `ret` leaves the function first (see `FnLower::ret`).
struct Pending {
    /// The expression whose evaluation is waiting for this drop: a
    /// borrowed element (`head (f x)`), whose array lives until the element
    /// is no longer used; or a `check`'s value; or a loop's source.
    key: *const TExpr,
    value: Value,
    ty: Ty,
}

#[derive(Clone, Copy)]
enum IterKind {
    /// The counter is the number itself.
    Range { from: Value, to: Value },
    /// The counter is the position of the element.
    Array { array: Value, len: Value },
    /// The counter is the byte position of the character.
    Str { s: Value, len: Value },
}

/// Where a `check` arm continues to the merge block: its block, its
/// value, and its variables there (owners, values and their types).
struct ArmEnd {
    block: BlockId,
    value: Value,
    owned: Owned,
    locals: Vec<Option<Value>>,
    local_tys: Vec<Ty>,
}

/// Which variables currently own a value that must be dropped.
#[derive(Clone)]
struct Owned {
    locals: Vec<bool>,
}

impl Owned {
    /// Owners that still own their value on *both* paths.
    fn meet(&self, other: &Owned) -> Owned {
        Owned {
            locals: self
                .locals
                .iter()
                .zip(&other.locals)
                .map(|(x, y)| *x && *y)
                .collect(),
        }
    }
}

struct FnLower<'a> {
    tir: &'a Tir,
    f: &'a TFunc,
    strings: &'a mut Strings,
    /// Freeing, copying and printing functions generated per type.
    fns: &'a mut TypeFns,
    b: FunctionBuilder,
    /// The LIR value each variable holds at the current point. In SSA form
    /// (see `lir`) a variable is just a name for the value last given to it,
    /// so no memory is needed: `x = x * 2` makes `x` name the new value. Only
    /// a loop needs more: a variable updated in its body becomes a parameter
    /// of the loop's header block.
    locals: Vec<Option<Value>>,
    /// The type of each variable's current value.
    local_tys: Vec<Ty>,
    /// Owners whose value has not been moved yet at the current point.
    owned: Owned,
    /// Variables seen with a narrower type in the current `check` arms.
    views: Vec<View>,
    /// Variables holding a borrowed value (`@` parameters, variables of
    /// loops over `@a`): never owned, yet they have a value.
    lent: Vec<bool>,
    /// Values to free later, innermost last.
    pending: Vec<Pending>,
    /// The current code follows a `ret`: it never runs, and the paths that
    /// join after `check` arms or loops must not count it.
    dead: bool,
    /// The loops being lowered, innermost last, for `over` and `jump`.
    loops: Vec<LoopCx>,
    /// The calls in tail position (see `tail_calls`), and the block that
    /// starts the body again with new parameters, if one of them calls
    /// this function.
    tail: HashSet<*const TExpr>,
    restart: Option<BlockId>,
    /// The parameters the body gives new values.
    assigned: Vec<bool>,
}

/// Where `over` and `jump` go in a loop being lowered.
struct LoopCx {
    header: BlockId,
    exit: BlockId,
    /// The arguments of the header that start the next round, before the
    /// carried variables.
    next: [Value; 2],
    carried: Vec<(usize, Ty)>,
    inner: Vec<usize>,
    /// How many values were pending when the body started: those after are
    /// the body's, freed when a round ends early.
    pending: usize,
    /// The body has an `over`: a moving loop may leave elements in its
    /// source.
    over: bool,
}

/// A variable of a maybe type (`string?`) inside a `check` arm where it
/// cannot be void (see `typeck::Narrowing`). The variable still owns its box,
/// `boxed`; inside the arm it reads as the payload taken from the box.
struct View {
    local: usize,
    boxed: Value,
    /// The variable's type outside the arm.
    ty: Ty,
    /// The arm moved the payload out (and freed the box).
    moved_out: bool,
}

impl<'a> FnLower<'a> {
    fn new(
        tir: &'a Tir,
        f: &'a TFunc,
        strings: &'a mut Strings,
        fns: &'a mut TypeFns,
    ) -> FnLower<'a> {
        let params: Vec<Type> = f.params.iter().map(|&t| repr(t)).collect();
        let ret = if f.discard { None } else { ret_repr(f.ret) };
        let mut b = FunctionBuilder::new(f.symbol.clone(), &params, ret);
        b.func.comment = Some(f.signature());
        let n = f.local_tys.len();
        let mut assigned = vec![false; n];
        let (mut tail, mut reuses) = (HashSet::new(), false);
        if f.native.is_none() {
            assigned_locals(&f.body, &mut assigned);
            (tail, reuses) = tail_calls(tir, f, &assigned);
        }
        // A call of this function in tail position reuses its frame: it
        // jumps back to the start of the body, whose block takes the
        // parameters, so a deep recursion runs like a loop.
        let mut restart = None;
        let mut starts: Vec<Value> = (0..f.params.len()).map(|i| b.param(i)).collect();
        if reuses {
            let block = b.new_block("restart");
            let ps: Vec<Value> = params.iter().map(|&t| b.block_param(block, t)).collect();
            b.jmp(block, starts);
            b.switch_to(block);
            starts = ps;
            restart = Some(block);
        }
        // The parameters are the first variables. Arguments are moved into
        // the call, so the function owns them, except those it borrows.
        let mut locals: Vec<Option<Value>> = starts.into_iter().map(Some).collect();
        locals.resize(n, None);
        let mut local_tys = f.params.clone();
        local_tys.resize(n, Ty::NEVER);
        let mut owned: Vec<bool> = f
            .params
            .iter()
            .zip(&f.borrowed)
            .map(|(t, b)| t.needs_drop() && !b)
            .collect();
        owned.resize(n, false);
        let mut lent = f.borrowed.clone();
        lent.resize(n, false);
        FnLower {
            tir,
            f,
            strings,
            fns,
            b,
            locals,
            local_tys,
            owned: Owned { locals: owned },
            views: Vec::new(),
            lent,
            pending: Vec::new(),
            dead: false,
            loops: Vec::new(),
            tail,
            restart,
            assigned,
        }
    }

    fn run(mut self) -> lir::Function {
        match &self.f.native {
            Some(Native::Runtime(symbol)) => return self.native(symbol),
            Some(Native::Intrinsic(i)) => return self.intrinsic(&i.name),
            None => {}
        }
        self.take_apart_patterns();
        let returns = !self.f.discard && ret_repr(self.f.ret).is_some();
        let mut result = None;
        for (i, stmt) in self.f.body.iter().enumerate() {
            // The last statement's value is the result and moves to the
            // caller; the values of other statements are discarded.
            match stmt {
                TStmt::Expr(e) if returns && i + 1 == self.f.body.len() => {
                    result = Some((self.expr(e, Use::Move), e.ty));
                }
                _ => self.stmt(stmt),
            }
        }
        if self.dead {
            // Every path left through `ret`.
            self.b.unreachable();
            return self.b.finish();
        }
        let result = result.map(|(v, ty)| self.convert(v, ty, self.f.ret));
        // The end of the function's scope: drop everything still owned.
        self.drop_owned(&self.owned.clone(), None);
        self.b.ret(result);
        self.b.finish()
    }

    /// A `native` function passes its arguments to the runtime function
    /// that gives it, which takes them as they are represented here (a
    /// string as its pointer, a maybe value as a box) and returns its
    /// result the same way.
    fn native(mut self, symbol: &str) -> lir::Function {
        let args = (0..self.f.params.len()).map(|i| self.b.param(i)).collect();
        let ret = if self.f.discard {
            None
        } else {
            ret_repr(self.f.ret)
        };
        let result = self.b.call(symbol, args, ret);
        self.drop_owned(&self.owned.clone(), None);
        self.b.ret(result);
        self.b.finish()
    }

    /// What `echo` prints for `v`, printed into a new string instead.
    fn text(&mut self, v: Value, ty: Ty) -> Value {
        let save = self.b.alloca(16);
        self.fns.captures = true;
        self.b.call("rt.capture_begin", vec![save], None);
        self.fns.print(&mut self.b, v, ty, false);
        self.b
            .call("rt.capture_end", vec![save], Some(Type::I64))
            .unwrap()
    }

    /// A function the compiler gives (see `ast::Intrinsic`), on its first
    /// parameter, borrowed:
    ///
    /// - `len`: the number of elements, or of characters;
    /// - `slice from to`: a copy of elements `from` up to (not including)
    ///   `to`, both kept inside the value;
    /// - `at i`: a copy of element `i`, or the program stops with an
    ///   IndexError;
    /// - `copy`: a copy of the whole value;
    /// - `text`: what `echo` prints for the value, as a new string.
    ///
    /// The runtime does the work for strings, whose characters it can find;
    /// what is done for an array depends on its elements' type.
    fn intrinsic(mut self, name: &str) -> lir::Function {
        let (v, ty) = (self.b.param(0), self.f.params[0]);
        let arg = |this: &Self, k: usize| this.b.param(k);
        let call = |this: &mut Self, f: &str, args: Vec<Value>| {
            this.b.call(f, args, Some(Type::I64)).unwrap()
        };
        let result = match (name, ty.elem()) {
            ("copy", _) => self.fns.clone(&mut self.b, v, ty),
            ("text", _) => self.text(v, ty),
            ("len", None) => call(&mut self, "rt.str_chars", vec![v]),
            ("slice", None) => {
                let (from, to) = (arg(&self, 1), arg(&self, 2));
                call(&mut self, "rt.str_slice", vec![v, from, to])
            }
            ("at", None) => {
                let i = arg(&self, 1);
                call(&mut self, "rt.str_at", vec![v, i])
            }
            ("len", Some(_)) => self.b.load(MemTy::I64, v, 0),
            ("slice", Some(elem)) => {
                let n = self.b.load(MemTy::I64, v, 0);
                let zero = self.b.iconst(0);
                let (from, to) = (arg(&self, 1), arg(&self, 2));
                let from = call(&mut self, "rt.clamp", vec![from, zero, n]);
                let to = call(&mut self, "rt.clamp", vec![to, from, n]);
                let count = self.b.ibin(IBinOp::Sub, to, from);
                let part = call(&mut self, "rt.array_new", vec![count]);
                self.count_loop(count, |this, k| {
                    let at = this.b.ibin(IBinOp::Add, from, k);
                    let e = load_elem(&mut this.b, v, at, elem);
                    let e = this.fns.clone(&mut this.b, e, elem);
                    store_elem(&mut this.b, part, k, elem, e);
                });
                part
            }
            ("at", Some(elem)) => {
                let n = self.b.load(MemTy::I64, v, 0);
                let i = arg(&self, 1);
                // A negative position is a huge one, unsigned.
                let inside = self.b.icmp(ICond::Ult, i, n);
                let found = self.b.new_block("found");
                let missing = self.b.new_block("missing");
                self.b.br(inside, found, vec![], missing, vec![]);
                self.b.switch_to(missing);
                self.b.call("rt.index_error", vec![i, n], None);
                self.b.unreachable();
                self.b.switch_to(found);
                let e = load_elem(&mut self.b, v, i, elem);
                self.fns.clone(&mut self.b, e, elem)
            }
            _ => unreachable!("unknown intrinsic `{name}`"),
        };
        // (A copy of void is no value at all.)
        let returns = ret_repr(self.f.ret).is_some();
        self.b.ret(returns.then_some(result));
        self.b.finish()
    }

    /// `ret value` / `ret`: convert the value to the function's result
    /// type, drop everything the function still owns (its variables, and
    /// the values whose drop is pending, such as what the enclosing loops
    /// go through), and return. The code that follows is dead.
    fn ret(&mut self, value: Option<&TExpr>) {
        let v = value.map(|v| (self.expr(v, Use::Move), v.ty));
        let returns = !self.f.discard && ret_repr(self.f.ret).is_some();
        let result = match v {
            Some((v, ty)) if returns => Some(self.convert(v, ty, self.f.ret)),
            Some((v, ty)) => {
                self.fns.drop(&mut self.b, v, ty);
                None
            }
            // A bare `ret` in a function that may also give a value.
            None if returns => {
                let void = self.b.iconst(0);
                Some(self.convert(void, Ty::VOID, self.f.ret))
            }
            None => None,
        };
        self.drop_owned(&self.owned.clone(), None);
        for k in (0..self.pending.len()).rev() {
            let (v, ty) = (self.pending[k].value, self.pending[k].ty);
            self.fns.drop(&mut self.b, v, ty);
        }
        self.b.ret(result);
        let after = self.b.new_block("after_ret");
        self.b.switch_to(after);
        self.dead = true;
    }

    /// A call of this function in tail position, with these arguments: as
    /// `ret` would after the call, drop everything the function still owns,
    /// then start the body again instead of calling.
    fn restart(&mut self, args: Vec<Value>) {
        self.drop_owned(&self.owned.clone(), None);
        for k in (0..self.pending.len()).rev() {
            let (v, ty) = (self.pending[k].value, self.pending[k].ty);
            self.fns.drop(&mut self.b, v, ty);
        }
        let restart = self.restart.expect("a tail call that reuses the frame");
        self.b.jmp(restart, args);
    }

    /// A parameter with a pattern `(x:y:xs)` receives an array with at
    /// least as many elements as it names (the caller tested that). The
    /// function owns the array, so its elements are *moved* into the named
    /// variables and the rest into a new array; the old array is freed.
    fn take_apart_patterns(&mut self) {
        for (p, pattern) in self.f.patterns.iter().enumerate() {
            let Some(HPattern::Cons { heads, rest }) = pattern else {
                continue;
            };
            let array = self.locals[p].expect("parameter has a value");
            let elem = self.f.params[p].elem().unwrap_or(Ty::NEVER);
            for (k, &h) in heads.iter().enumerate() {
                let at = self.b.iconst(k as i64);
                self.locals[h] = Some(load_elem(&mut self.b, array, at, elem));
                self.local_tys[h] = elem;
                self.owned.locals[h] = elem.needs_drop();
            }
            let len = self.b.load(MemTy::I64, array, 0);
            let skip = self.b.iconst(heads.len() as i64);
            let n = self.b.ibin(IBinOp::Sub, len, skip);
            let tail = self
                .b
                .call("rt.array_new", vec![n], Some(Type::I64))
                .unwrap();
            self.count_loop(n, |this, i| {
                let at = this.b.ibin(IBinOp::Add, skip, i);
                let e = load_elem(&mut this.b, array, at, elem);
                store_elem(&mut this.b, tail, i, elem, e);
            });
            self.b.call("rt.free", vec![array], None);
            self.owned.locals[p] = false;
            self.locals[*rest] = Some(tail);
            self.local_tys[*rest] = Ty::array(elem);
            self.owned.locals[*rest] = true;
        }
    }

    fn stmt(&mut self, stmt: &TStmt) {
        match stmt {
            TStmt::Let { local, value } => {
                let v = self.expr(value, Use::Move);
                // A new value replaces the old one, which is dropped now:
                // in `s = s + "!"` the old `s` was only read, so it is
                // still owned until this point.
                if self.owned.locals[*local] {
                    let old = self.locals[*local].expect("owned variable has a value");
                    self.fns.drop(&mut self.b, old, self.local_tys[*local]);
                }
                self.locals[*local] = Some(v);
                self.local_tys[*local] = value.ty;
                self.owned.locals[*local] = value.ty.needs_drop();
            }
            // A value thrown away is dropped, unless it is borrowed.
            TStmt::Expr(e) if e.is_borrowed() => {
                let v = self.expr(e, Use::Borrow);
                self.drop_temp(e, v);
            }
            TStmt::Expr(e) => {
                let v = self.expr(e, Use::Move);
                self.fns.drop(&mut self.b, v, e.ty);
            }
            TStmt::Loop {
                pos,
                var,
                source,
                borrowed,
                body,
                carried,
                inner,
            } => self.loop_stmt(*pos, *var, source, *borrowed, body, carried, inner),
        }
    }

    /// `loop var in [start..end]:` becomes
    ///
    /// ```text
    ///   entry:            from = start, to = end
    ///                     jmp loop(from, carried values...)
    ///   loop(i, ...):     br i < to, loop_body, loop_end
    ///   loop_body:        var = i; the body
    ///                     drop the body's own variables
    ///                     jmp loop(i + 1, carried values...)
    ///   loop_end:         the carried variables hold the loop's parameters
    /// ```
    ///
    /// Going through an array or a string has the same shape, with the
    /// position as the counter and the element (or the character) as `var`
    /// (see `Iter`).
    ///
    /// The variables the body updates (`carried`) are passed around the loop
    /// as block parameters: SSA's way of saying "this value differs from
    /// one iteration to the next". `typeck` gave each of them one type for
    /// the whole loop, so values are converted (boxed) to it when entering
    /// the loop and at the end of every iteration.
    #[allow(clippy::too_many_arguments)]
    fn loop_stmt(
        &mut self,
        pos: Option<usize>,
        var: usize,
        source: &LoopSource,
        borrowed: bool,
        body: &[TStmt],
        carried: &[(usize, Ty)],
        inner: &[usize],
    ) {
        let it = self.iter_begin(source, borrowed);
        let zero = self.b.iconst(0);
        let mut args = vec![it.init, zero];
        for &(l, ty) in carried {
            args.push(self.carry(l, ty));
        }
        let header = self.b.new_block("loop");
        let counter = self.b.block_param(header, Type::I64);
        // The element's position (0, 1, ...), for `loop i, x in ...`.
        let position = self.b.block_param(header, Type::I64);
        let params: Vec<Value> = carried
            .iter()
            .map(|&(_, ty)| self.b.block_param(header, repr(ty)))
            .collect();
        let body_block = self.b.new_block("loop_body");
        let exit = self.b.new_block("loop_end");
        self.b.jmp(header, args);

        self.b.switch_to(header);
        let more = self.iter_more(&it, counter);
        self.b.br(more, body_block, vec![], exit, params.clone());
        let after: Vec<Value> = carried
            .iter()
            .map(|&(_, ty)| self.b.block_param(exit, repr(ty)))
            .collect();
        let at_header = self.owned.clone();
        for (&(l, _), &p) in carried.iter().zip(&params) {
            self.locals[l] = Some(p);
        }

        self.b.switch_to(body_block);
        let (element, next) = self.iter_step(&it, counter);
        self.bind_element(var, element, &it);
        if let Some(p) = pos {
            self.locals[p] = Some(position);
            self.local_tys[p] = Ty::INT;
        }
        let one = self.b.iconst(1);
        let next_position = self.b.ibin(IBinOp::Add, position, one);
        self.loops.push(LoopCx {
            header,
            exit,
            next: [next, next_position],
            carried: carried.to_vec(),
            inner: inner.to_vec(),
            pending: self.pending.len(),
            over: false,
        });
        for stmt in body {
            self.stmt(stmt);
        }
        let cx = self.loops.pop().expect("the loop is open");
        if std::mem::take(&mut self.dead) {
            // The body always returns: no next iteration.
            self.b.unreachable();
        } else {
            // The body's own variables end with the iteration.
            for &l in inner {
                if self.owned.locals[l] {
                    let v = self.locals[l].expect("owned variable has a value");
                    self.fns.drop(&mut self.b, v, self.local_tys[l]);
                    self.owned.locals[l] = false;
                }
            }
            let mut args = vec![next, next_position];
            for &(l, ty) in carried {
                args.push(self.carry(l, ty));
            }
            self.b.jmp(header, args);
        }
        for &l in inner {
            self.owned.locals[l] = false;
        }

        // After the loop: the ownership checker made sure every outside
        // variable is owned exactly as it was before the loop.
        self.b.switch_to(exit);
        for (&(l, ty), &p) in carried.iter().zip(&after) {
            self.locals[l] = Some(p);
            self.local_tys[l] = ty;
        }
        self.owned = at_header;
        self.iter_end(&it, cx.over);
    }

    /// `over` (`to_exit`) or `jump`: end this round of the innermost loop
    /// now. The body's own variables and the values it has pending are
    /// freed, as at the end of the body; then the loop ends, or goes on with
    /// its next round.
    fn leave_round(&mut self, to_exit: bool) {
        let saved = (
            self.locals.clone(),
            self.local_tys.clone(),
            self.owned.clone(),
        );
        let cx = self.loops.last_mut().expect("resolved inside a loop");
        cx.over |= to_exit;
        let (inner, carried, mark) = (cx.inner.clone(), cx.carried.clone(), cx.pending);
        let (header, exit, next) = (cx.header, cx.exit, cx.next);
        let mut dropped = Owned {
            locals: vec![false; self.owned.locals.len()],
        };
        for &l in &inner {
            dropped.locals[l] = self.owned.locals[l];
        }
        self.drop_owned(&dropped, None);
        for k in (mark..self.pending.len()).rev() {
            let (v, ty) = (self.pending[k].value, self.pending[k].ty);
            self.fns.drop(&mut self.b, v, ty);
        }
        // A carried variable seen through a view goes on as its box.
        for view in &self.views {
            if !view.moved_out && carried.iter().any(|&(l, _)| l == view.local) {
                self.locals[view.local] = Some(view.boxed);
                self.local_tys[view.local] = view.ty;
            }
        }
        let mut args = if to_exit { vec![] } else { next.to_vec() };
        for &(l, ty) in &carried {
            args.push(self.carry(l, ty));
        }
        self.b.jmp(if to_exit { exit } else { header }, args);
        (self.locals, self.local_tys, self.owned) = saved;
        let after = self
            .b
            .new_block(if to_exit { "after_over" } else { "after_jump" });
        self.b.switch_to(after);
        self.dead = true;
    }

    // -- going through ranges, arrays and strings -------------------------------

    /// Start going through `source`: evaluate it, and find the first state
    /// of the counter (a number for a range, a position otherwise). The
    /// source moves into the loop, unless it is `borrowed` (`in @a`).
    ///
    /// A source the loop owns is freed when the loop ends, or by a `ret`
    /// inside it (it is pending until then). Each element taken out of it
    /// leaves a placeholder behind, an immortal empty object, so that what
    /// is freed then is only what has not been taken yet.
    fn iter_begin<'s>(&mut self, source: &'s LoopSource, borrowed: bool) -> Iter<'s> {
        match source {
            LoopSource::Range(start, end) => {
                let from = self.expr(start, Use::Borrow);
                let to = self.expr(end, Use::Borrow);
                self.drop_temp(start, from);
                self.drop_temp(end, to);
                Iter {
                    kind: IterKind::Range { from, to },
                    init: from,
                    elem: Ty::INT,
                    moving: false,
                    owned: None,
                }
            }
            LoopSource::Each(e) => {
                let usage = if borrowed { Use::Borrow } else { Use::Move };
                let v = self.expr(e, usage);
                let owned = !borrowed || !e.is_borrowed();
                if owned {
                    self.pending.push(Pending {
                        key: e,
                        value: v,
                        ty: e.ty,
                    });
                }
                let len = self.b.load(MemTy::I64, v, 0);
                let init = self.b.iconst(0);
                let (kind, elem) = match e.ty.elem() {
                    Some(elem) => (IterKind::Array { array: v, len }, elem),
                    None => (IterKind::Str { s: v, len }, Ty::STR),
                };
                Iter {
                    kind,
                    init,
                    elem,
                    moving: !borrowed,
                    owned: owned.then_some(e),
                }
            }
        }
    }

    /// Give the loop variable `var` the element `element` of `it`.
    fn bind_element(&mut self, var: usize, element: Value, it: &Iter) {
        self.locals[var] = Some(element);
        self.local_tys[var] = it.elem;
        self.owned.locals[var] = it.owns_elements();
        self.lent[var] = matches!(it.kind, IterKind::Array { .. }) && !it.moving;
    }

    /// The immortal object left in an array slot whose element was moved
    /// out: freeing it does nothing. An empty string and an empty array
    /// have the same layout (`{ len 0 }`), and a box holds the void; a
    /// record's drop function skips immortal objects, so the empty string
    /// does for records too.
    fn placeholder(&mut self, elem: Ty) -> Value {
        if elem.is_union() {
            self.void_box()
        } else {
            self.string_const("")
        }
    }

    /// Whether `state` is still inside the source.
    fn iter_more(&mut self, it: &Iter, state: Value) -> Value {
        let end = match it.kind {
            IterKind::Range { to, .. } => to,
            IterKind::Array { len, .. } | IterKind::Str { len, .. } => len,
        };
        self.b.icmp(ICond::Slt, state, end)
    }

    /// The element at `state` (owned by the caller) and the next state.
    fn iter_step(&mut self, it: &Iter, state: Value) -> (Value, Value) {
        match it.kind {
            IterKind::Range { .. } => {
                let one = self.b.iconst(1);
                (state, self.b.ibin(IBinOp::Add, state, one))
            }
            IterKind::Array { array, .. } => {
                let e = load_elem(&mut self.b, array, state, it.elem);
                // Moving the element out: the slot gets a placeholder.
                if it.moving && it.elem.needs_drop() {
                    let p = self.placeholder(it.elem);
                    store_elem(&mut self.b, array, state, it.elem, p);
                }
                let one = self.b.iconst(1);
                (e, self.b.ibin(IBinOp::Add, state, one))
            }
            IterKind::Str { s, .. } => {
                let w = self
                    .b
                    .call("rt.utf8_width", vec![s, state], Some(Type::I64))
                    .unwrap();
                let next = self.b.ibin(IBinOp::Add, state, w);
                let c = self
                    .b
                    .call("rt.str_sub", vec![s, state, next], Some(Type::I64))
                    .unwrap();
                (c, next)
            }
        }
    }

    /// How many elements the source has.
    fn iter_count(&mut self, it: &Iter) -> Value {
        match it.kind {
            // max(to - from, 0), without a branch: `d >> 63` is all ones
            // exactly when d is negative.
            IterKind::Range { from, to } => {
                let d = self.b.ibin(IBinOp::Sub, to, from);
                let sixty_three = self.b.iconst(63);
                let sign = self.b.ibin(IBinOp::Sar, d, sixty_three);
                let all = self.b.iconst(-1);
                let keep = self.b.ibin(IBinOp::Xor, sign, all);
                self.b.ibin(IBinOp::And, d, keep)
            }
            IterKind::Array { len, .. } => len,
            IterKind::Str { s, .. } => self
                .b
                .call("rt.str_chars", vec![s], Some(Type::I64))
                .unwrap(),
        }
    }

    /// Done going through: a source the loop owns is freed. If its elements
    /// were moved out, only the array itself is left to free, unless the
    /// loop may have ended early (`left`): then the elements not taken yet
    /// are freed too (the taken ones left placeholders).
    fn iter_end(&mut self, it: &Iter, left: bool) {
        let Some(e) = it.owned else {
            return;
        };
        let pending = self.unpend(e).expect("the source is pending");
        match it.kind {
            IterKind::Array { array, .. } if it.moving && !left => {
                self.b.call("rt.free", vec![array], None);
            }
            _ => self.fns.drop(&mut self.b, pending.0, pending.1),
        }
    }

    /// `[body | var in source, guard...]`: an array with `body` for each
    /// element that passes the guards. The array is allocated for every
    /// element; with guards, its length is set at the end to the number of
    /// elements kept.
    fn comprehension(
        &mut self,
        body: &TExpr,
        var: usize,
        source: &LoopSource,
        borrowed: bool,
        guards: &[&TExpr],
        ty: Ty,
    ) -> Value {
        let it = self.iter_begin(source, borrowed);
        let n = self.iter_count(&it);
        let result = self
            .b
            .call("rt.array_new", vec![n], Some(Type::I64))
            .unwrap();
        let elem = ty.elem().expect("an array type");
        let header = self.b.new_block("each");
        let state = self.b.block_param(header, Type::I64);
        let k = self.b.block_param(header, Type::I64);
        let body_block = self.b.new_block("each_body");
        let exit = self.b.new_block("each_end");
        let zero = self.b.iconst(0);
        self.b.jmp(header, vec![it.init, zero]);
        self.b.switch_to(header);
        let more = self.iter_more(&it, state);
        self.b.br(more, body_block, vec![], exit, vec![]);
        self.b.switch_to(body_block);
        let (element, next) = self.iter_step(&it, state);
        self.bind_element(var, element, &it);
        // An element failing a guard is dropped and skipped.
        let skip = self.b.new_block("skip");
        for g in guards {
            let pass = self.expr(g, Use::Borrow);
            let keep = self.b.new_block("keep");
            self.b.br(pass, keep, vec![], skip, vec![]);
            self.b.switch_to(keep);
        }
        let v = self.expr(body, Use::Move);
        store_elem(&mut self.b, result, k, elem, v);
        if self.owned.locals[var] {
            self.fns.drop(&mut self.b, element, it.elem);
            self.owned.locals[var] = false;
        }
        let one = self.b.iconst(1);
        let k_next = self.b.ibin(IBinOp::Add, k, one);
        self.b.jmp(header, vec![next, k_next]);
        self.b.switch_to(skip);
        if it.owns_elements() {
            self.fns.drop(&mut self.b, element, it.elem);
        }
        self.b.jmp(header, vec![next, k]);
        self.b.switch_to(exit);
        if !guards.is_empty() {
            self.b.store(MemTy::I64, result, 0, k);
        }
        self.iter_end(&it, false);
        result
    }

    /// A comprehension with several generators or positions: its size is
    /// not known up front, so it builds its array in a growing buffer, a
    /// stack cell `{ array, len, capacity }` that `rt.vec_push` appends to.
    fn comprehension_general(&mut self, body: &TExpr, clauses: &[TClause], ty: Ty) -> Value {
        let elem = ty.elem().expect("an array type");
        let cell = self.b.alloca(24);
        let zero = self.b.iconst(0);
        let empty = self
            .b
            .call("rt.array_new", vec![zero], Some(Type::I64))
            .unwrap();
        self.b.store(MemTy::I64, cell, 0, empty);
        self.b.store(MemTy::I64, cell, 8, zero);
        self.b.store(MemTy::I64, cell, 16, zero);
        self.clause(clauses, None, &mut |this| {
            let v = this.expr(body, Use::Move);
            let bits = match repr(elem) {
                Type::F64 => this.b.bitcast(v, Type::I64),
                Type::I64 => v,
            };
            this.b.call("rt.vec_push", vec![cell, bits], None);
        });
        let array = self.b.load(MemTy::I64, cell, 0);
        let len = self.b.load(MemTy::I64, cell, 8);
        self.b.store(MemTy::I64, array, 0, len);
        array
    }

    /// The code for `clauses`, then `emit` for each combination that passes.
    /// A generator is a loop around the rest; a guard that fails jumps to
    /// `skip`, the innermost generator's "next element" block.
    fn clause(
        &mut self,
        clauses: &[TClause],
        skip: Option<BlockId>,
        emit: &mut dyn FnMut(&mut Self),
    ) {
        match clauses.split_first() {
            None => emit(self),
            Some((TClause::Guard(g), rest)) => {
                let pass = self.expr(g, Use::Borrow);
                let keep = self.b.new_block("keep");
                let skip = skip.expect("a generator comes first");
                self.b.br(pass, keep, vec![], skip, vec![]);
                self.b.switch_to(keep);
                self.clause(rest, Some(skip), emit);
            }
            Some((
                TClause::Gen {
                    pos,
                    var,
                    source,
                    borrowed,
                },
                rest,
            )) => {
                let it = self.iter_begin(source, *borrowed);
                let header = self.b.new_block("each");
                let state = self.b.block_param(header, Type::I64);
                let position = self.b.block_param(header, Type::I64);
                let body = self.b.new_block("each_body");
                let next_element = self.b.new_block("each_skip");
                let exit = self.b.new_block("each_end");
                let zero = self.b.iconst(0);
                self.b.jmp(header, vec![it.init, zero]);
                self.b.switch_to(header);
                let more = self.iter_more(&it, state);
                self.b.br(more, body, vec![], exit, vec![]);
                self.b.switch_to(body);
                let (element, next) = self.iter_step(&it, state);
                let one = self.b.iconst(1);
                let next_position = self.b.ibin(IBinOp::Add, position, one);
                self.bind_element(*var, element, &it);
                if let Some(p) = pos {
                    self.locals[*p] = Some(position);
                    self.local_tys[*p] = Ty::INT;
                }
                self.clause(rest, Some(next_element), emit);
                // The element is done with: dropped unless it was moved
                // into the result.
                if self.owned.locals[*var] {
                    self.fns.drop(&mut self.b, element, it.elem);
                }
                self.b.jmp(header, vec![next, next_position]);
                // A guard failed: the element was not moved.
                self.b.switch_to(next_element);
                if it.owns_elements() {
                    self.fns.drop(&mut self.b, element, it.elem);
                }
                self.b.jmp(header, vec![next, next_position]);
                self.owned.locals[*var] = false;
                self.b.switch_to(exit);
                self.iter_end(&it, false);
            }
        }
    }

    /// A loop `for i in 0..n` around `body`, in the current function.
    fn count_loop(&mut self, n: Value, mut body: impl FnMut(&mut Self, Value)) {
        let header = self.b.new_block("count");
        let i = self.b.block_param(header, Type::I64);
        let inner = self.b.new_block("count_body");
        let done = self.b.new_block("count_end");
        let zero = self.b.iconst(0);
        self.b.jmp(header, vec![zero]);
        self.b.switch_to(header);
        let more = self.b.icmp(ICond::Slt, i, n);
        self.b.br(more, inner, vec![], done, vec![]);
        self.b.switch_to(inner);
        body(self, i);
        let one = self.b.iconst(1);
        let next = self.b.ibin(IBinOp::Add, i, one);
        self.b.jmp(header, vec![next]);
        self.b.switch_to(done);
    }

    // -- arrays ---------------------------------------------------------------

    /// A new array with the elements of two arrays (converted to `elem`,
    /// the result's element type). The elements move into it, and the two
    /// arrays they were in are freed.
    fn concat_arrays(&mut self, a: Value, ea: Ty, b: Value, eb: Ty, elem: Ty) -> Value {
        let la = self.b.load(MemTy::I64, a, 0);
        let lb = self.b.load(MemTy::I64, b, 0);
        let n = self.b.ibin(IBinOp::Add, la, lb);
        let r = self
            .b
            .call("rt.array_new", vec![n], Some(Type::I64))
            .unwrap();
        self.count_loop(la, |this, i| {
            let e = load_elem(&mut this.b, a, i, ea);
            let e = this.convert(e, ea, elem);
            store_elem(&mut this.b, r, i, elem, e);
        });
        self.count_loop(lb, |this, i| {
            let e = load_elem(&mut this.b, b, i, eb);
            let e = this.convert(e, eb, elem);
            let at = this.b.ibin(IBinOp::Add, la, i);
            store_elem(&mut this.b, r, at, elem, e);
        });
        self.b.call("rt.free", vec![a], None);
        self.b.call("rt.free", vec![b], None);
        r
    }

    /// 1 if `v` of type `ty` holds an array (rather than void).
    fn is_array(&mut self, v: Value, ty: Ty) -> Value {
        match ty.as_kind() {
            Some(Kind::Array) => self.b.iconst(1),
            Some(_) => self.b.iconst(0),
            None => {
                let tag = self.b.load(MemTy::I64, v, 0);
                let array_tag = self.b.iconst(Kind::Array.tag());
                self.b.icmp(ICond::Eq, tag, array_tag)
            }
        }
    }

    /// `a ++ b`: if either is void, void; otherwise a new array with the
    /// elements of both. It takes both operands: their elements move into
    /// the result, so nothing is copied.
    fn concat(&mut self, a: &TExpr, b: &TExpr, ty: Ty) -> Value {
        let v = self.operands(&[(a, Use::Move), (b, Use::Move)]);
        let (va, vb) = (v[0], v[1]);
        let elem = ty.elem().unwrap_or(Ty::NEVER);
        let (ea, eb) = (
            a.ty.elem().unwrap_or(Ty::NEVER),
            b.ty.elem().unwrap_or(Ty::NEVER),
        );
        if a.ty.as_kind() == Some(Kind::Array) && b.ty.as_kind() == Some(Kind::Array) {
            return self.concat_arrays(va, ea, vb, eb, elem);
        }
        let xa = self.is_array(va, a.ty);
        let xb = self.is_array(vb, b.ty);
        let both = self.b.ibin(IBinOp::And, xa, xb);
        self.or_void_else(
            both,
            ty,
            |this| {
                let pa = this.array_in(va, a.ty);
                let pb = this.array_in(vb, b.ty);
                let r = this.concat_arrays(pa, ea, pb, eb, elem);
                // The boxes the arrays came in.
                for (v, t) in [(va, a.ty), (vb, b.ty)] {
                    if t.is_union() {
                        this.b.call("rt.free", vec![v], None);
                    }
                }
                (r, Ty::array(elem))
            },
            |this| {
                this.fns.drop(&mut this.b, va, a.ty);
                this.fns.drop(&mut this.b, vb, b.ty);
            },
        )
    }

    /// `a ** b`: if either is void, void; otherwise a new array as long as
    /// the longer one, with the product of each pair of elements and, past
    /// the end of the shorter array, the longer one's elements. It takes
    /// both operands. (Their elements are numbers, so the result is built
    /// with copies of them, and the operands are dropped.)
    fn elem_mul(&mut self, a: &TExpr, b: &TExpr, ty: Ty) -> Value {
        let v = self.operands(&[(a, Use::Move), (b, Use::Move)]);
        let (va, vb) = (v[0], v[1]);
        let r = if a.ty.as_kind() == Some(Kind::Array) && b.ty.as_kind() == Some(Kind::Array) {
            self.elem_mul_arrays(va, a.ty, vb, b.ty, ty)
        } else {
            let xa = self.is_array(va, a.ty);
            let xb = self.is_array(vb, b.ty);
            let both = self.b.ibin(IBinOp::And, xa, xb);
            self.or_void(both, ty, |this| {
                // The arrays inside the boxes, still owned by them.
                let pa = this.array_in(va, a.ty);
                let pb = this.array_in(vb, b.ty);
                let ta = a.ty.only(Kind::Array);
                let tb = b.ty.only(Kind::Array);
                let array_ty = ty.only(Kind::Array);
                (this.elem_mul_arrays(pa, ta, pb, tb, array_ty), array_ty)
            })
        };
        self.fns.drop(&mut self.b, va, a.ty);
        self.fns.drop(&mut self.b, vb, b.ty);
        r
    }

    /// The array in `v` of type `ty` (known to hold one), without taking it.
    fn array_in(&mut self, v: Value, ty: Ty) -> Value {
        if ty.as_kind() == Some(Kind::Array) {
            v
        } else {
            self.b.load(MemTy::I64, v, 8)
        }
    }

    fn elem_mul_arrays(&mut self, a: Value, ta: Ty, b: Value, tb: Ty, ty: Ty) -> Value {
        let (ea, eb) = (
            ta.elem().unwrap_or(Ty::NEVER),
            tb.elem().unwrap_or(Ty::NEVER),
        );
        let elem = ty.elem().expect("an array type");
        let la = self.b.load(MemTy::I64, a, 0);
        let lb = self.b.load(MemTy::I64, b, 0);
        let a_longer = self.b.icmp(ICond::Sgt, la, lb);
        let n = self.b.new_block("longer");
        let len = self.b.block_param(n, Type::I64);
        self.b.br(a_longer, n, vec![la], n, vec![lb]);
        self.b.switch_to(n);
        let r = self
            .b
            .call("rt.array_new", vec![len], Some(Type::I64))
            .unwrap();
        self.count_loop(len, |this, i| {
            let in_a = this.b.icmp(ICond::Slt, i, la);
            let in_b = this.b.icmp(ICond::Slt, i, lb);
            let both = this.b.ibin(IBinOp::And, in_a, in_b);
            let store = this.b.new_block("elem_store");
            let v = this.b.block_param(store, repr(elem));
            let pair = this.b.new_block("elem_pair");
            let single = this.b.new_block("elem_single");
            let only_a = this.b.new_block("elem_only_a");
            let only_b = this.b.new_block("elem_only_b");
            this.b.br(both, pair, vec![], single, vec![]);
            this.b.switch_to(pair);
            if ea.is_never() || eb.is_never() {
                // One side is the empty array `[]`: there are no pairs.
                this.b.unreachable();
            } else {
                let x = load_elem(&mut this.b, a, i, ea);
                let y = load_elem(&mut this.b, b, i, eb);
                let p = this.binary(BinOp::Mul, x, ea, y, eb, elem);
                this.b.jmp(store, vec![p]);
            }
            this.b.switch_to(single);
            this.b.br(in_a, only_a, vec![], only_b, vec![]);
            for (block, array, e) in [(only_a, a, ea), (only_b, b, eb)] {
                this.b.switch_to(block);
                let x = load_elem(&mut this.b, array, i, e);
                let x = this.fns.clone(&mut this.b, x, e);
                let x = this.convert(x, e, elem);
                this.b.jmp(store, vec![x]);
            }
            this.b.switch_to(store);
            store_elem(&mut this.b, r, i, elem, v);
        });
        r
    }

    /// `a !! i`: the element (or character) at position `i`, or void when
    /// `i` is out of range or either side is void. The element is the one
    /// in the array, borrowed (see `TExpr::is_borrowed`), unless it is a
    /// number, copied as usual. A character is a new string.
    fn index(&mut self, e: &TExpr, a: &TExpr, i: &TExpr, ty: Ty) -> Value {
        let itself = gives_element_itself(a.ty);
        let v = self.operands(&[(a, Use::Borrow), (i, Use::Borrow)]);
        let (va, vi) = (v[0], v[1]);
        // The position, and whether there is one (it may be void).
        let (has_pos, pos) = if i.ty == Ty::INT {
            (self.b.iconst(1), vi)
        } else if i.ty == Ty::VOID {
            (self.b.iconst(0), self.b.iconst(0))
        } else {
            let tag = self.b.load(MemTy::I64, vi, 0);
            let int_tag = self.b.iconst(Kind::Int.tag());
            let is_int = self.b.icmp(ICond::Eq, tag, int_tag);
            (is_int, self.b.load(MemTy::I64, vi, 8))
        };
        let r = self.dispatch(va, a.ty, ty, |this, kind, av| {
            let merge = this.b.new_block("index_end");
            let result = this.b.block_param(merge, repr(ty));
            let hit = this.b.new_block("index_hit");
            let miss = this.b.new_block("index_miss");
            match kind {
                Kind::Array => {
                    let elem = a.ty.elem().unwrap_or(Ty::NEVER);
                    let len = this.b.load(MemTy::I64, av, 0);
                    let zero = this.b.iconst(0);
                    let not_negative = this.b.icmp(ICond::Sge, pos, zero);
                    let below_len = this.b.icmp(ICond::Slt, pos, len);
                    let inside = this.b.ibin(IBinOp::And, not_negative, below_len);
                    let ok = this.b.ibin(IBinOp::And, inside, has_pos);
                    this.b.br(ok, hit, vec![], miss, vec![]);
                    this.b.switch_to(hit);
                    let x = load_elem(&mut this.b, av, pos, elem);
                    let x = if itself {
                        this.convert_borrowed(x, elem, ty)
                    } else {
                        let x = this.fns.clone(&mut this.b, x, elem);
                        this.convert(x, elem, ty)
                    };
                    this.b.jmp(merge, vec![x]);
                }
                Kind::Str => {
                    let found = this.b.new_block("index_found");
                    this.b.br(has_pos, found, vec![], miss, vec![]);
                    this.b.switch_to(found);
                    let c = this
                        .b
                        .call("rt.str_char_at", vec![av, pos], Some(Type::I64))
                        .unwrap();
                    let zero = this.b.iconst(0);
                    let some = this.b.icmp(ICond::Ne, c, zero);
                    this.b.br(some, hit, vec![], miss, vec![]);
                    this.b.switch_to(hit);
                    let c = this.convert(c, Ty::STR, ty);
                    this.b.jmp(merge, vec![c]);
                }
                _ => {
                    // void flows through (and never finds an element)
                    this.b.jmp(miss, vec![]);
                    this.b.switch_to(hit);
                    this.b.unreachable();
                }
            }
            this.b.switch_to(miss);
            let void = this.b.iconst(0);
            let void = this.convert(void, Ty::VOID, ty);
            this.b.jmp(merge, vec![void]);
            this.b.switch_to(merge);
            Some((ty, result))
        });
        self.keep_or_drop(e, itself, a, va);
        self.drop_temp(i, vi);
        r
    }

    /// After `a !! i` (the expression `e`): if the
    /// result is the element itself, a temporary `a` must live as long as
    /// the element is used, so its drop waits (see `release`); otherwise
    /// `a` is done with.
    fn keep_or_drop(&mut self, e: &TExpr, itself: bool, a: &TExpr, va: Value) {
        if !itself {
            self.drop_temp(a, va);
        } else if !a.is_borrowed() {
            self.pending.push(Pending {
                key: e,
                value: va,
                ty: a.ty,
            });
        }
    }

    // -- trails ---------------------------------------------------------------

    /// A value of a trail: each field's value goes into a variable of its
    /// own (owned, so a `ret` meanwhile frees it), and then into the new
    /// record, which takes them all.
    fn construct(&mut self, trail: &str, steps: &[(usize, TExpr)], slots: &[usize]) -> Value {
        for (l, v) in steps {
            let x = self.expr(v, Use::Move);
            self.locals[*l] = Some(x);
            self.local_tys[*l] = v.ty;
            self.owned.locals[*l] = v.ty.needs_drop();
        }
        let fields: Vec<Ty> = self.tir.trails[trail]
            .fields
            .iter()
            .map(|(_, t)| *t)
            .collect();
        let record = new_record(&mut self.b, fields.len());
        for (k, (&l, fty)) in slots.iter().zip(fields).enumerate() {
            let x = self.locals[l].expect("every field has a value");
            let x = self.convert(x, self.local_tys[l], fty);
            store_field(&mut self.b, record, k, fty, x);
            self.owned.locals[l] = false;
        }
        record
    }

    /// Field `k` of `base` (the expression `e`): the field itself, still in
    /// the record (see `TExpr::is_borrowed`), or a copy of a number; void
    /// when the value may be void and is.
    fn field(&mut self, e: &TExpr, base: &TExpr, k: usize) -> Value {
        let itself = e.is_borrowed();
        let vb = self.expr(base, Use::Borrow);
        let trail = base.ty.trail().expect("typeck checked it is a record");
        let fty = self.tir.trails[trail].fields[k].1;
        let ty = e.ty;
        let r = self.dispatch(vb, base.ty, ty, |this, kind, record| {
            let r = match kind {
                Kind::Record => {
                    let x = load_field(&mut this.b, record, k, fty);
                    if itself {
                        this.convert_borrowed(x, fty, ty)
                    } else {
                        let x = this.fns.clone(&mut this.b, x, fty);
                        this.convert(x, fty, ty)
                    }
                }
                _ => {
                    let void = this.b.iconst(0);
                    this.convert(void, Ty::VOID, ty)
                }
            };
            Some((ty, r))
        });
        self.keep_or_drop(e, itself, base, vb);
        r
    }

    /// `cond ? f() : void`, as a value of type `ty`. `f` returns its value
    /// and that value's type.
    fn or_void(&mut self, cond: Value, ty: Ty, f: impl FnOnce(&mut Self) -> (Value, Ty)) -> Value {
        self.or_void_else(cond, ty, f, |_| {})
    }

    /// `or_void`, running `none` before giving void.
    fn or_void_else(
        &mut self,
        cond: Value,
        ty: Ty,
        f: impl FnOnce(&mut Self) -> (Value, Ty),
        none_first: impl FnOnce(&mut Self),
    ) -> Value {
        let merge = self.b.new_block("or_void_end");
        let result = self.b.block_param(merge, repr(ty));
        let some = self.b.new_block("some");
        let none = self.b.new_block("none");
        self.b.br(cond, some, vec![], none, vec![]);
        self.b.switch_to(some);
        let (v, vty) = f(self);
        let v = self.convert(v, vty, ty);
        self.b.jmp(merge, vec![v]);
        self.b.switch_to(none);
        none_first(self);
        let void = self.b.iconst(0);
        let void = self.convert(void, Ty::VOID, ty);
        self.b.jmp(merge, vec![void]);
        self.b.switch_to(merge);
        result
    }

    /// `[a..b]` as an array of ints.
    fn range_array(&mut self, start: &TExpr, end: &TExpr) -> Value {
        let from = self.expr(start, Use::Borrow);
        let to = self.expr(end, Use::Borrow);
        let it = Iter {
            kind: IterKind::Range { from, to },
            init: from,
            elem: Ty::INT,
            moving: false,
            owned: None,
        };
        let n = self.iter_count(&it);
        let r = self
            .b
            .call("rt.array_new", vec![n], Some(Type::I64))
            .unwrap();
        self.count_loop(n, |this, i| {
            let v = this.b.ibin(IBinOp::Add, from, i);
            store_elem(&mut this.b, r, i, Ty::INT, v);
        });
        r
    }

    /// The value of variable `l` converted to its type across a loop, `ty`.
    /// A converted (boxed) value is owned by the variable, as the old one
    /// was. A variable without a value (moved) passes a placeholder.
    fn carry(&mut self, l: usize, ty: Ty) -> Value {
        let v = self.locals[l].expect("carried variable has a value");
        let from = self.local_tys[l];
        if from == ty {
            return v;
        }
        let has_value = self.owned.locals[l] || !from.needs_drop();
        self.local_tys[l] = ty;
        self.owned.locals[l] = has_value && ty.needs_drop();
        if has_value {
            self.convert(v, from, ty)
        } else {
            self.dummy(ty)
        }
    }

    // -- ownership and drops --------------------------------------------------
    //
    // Every heap value has exactly one owner, as checked by `ownership`:
    //
    // - a variable owns its value until the value is moved away (`y = x`,
    //   `f x`, returned, or the result of a `check` arm) or replaced by a
    //   new value (`x = ...`, which drops the old one); whatever is still
    //   owned when the function ends is dropped, and a loop body's own
    //   variables are dropped at the end of each iteration;
    // - a *temporary* (a fresh value from a call, an operator or a `check`)
    //   that is only read, as in `echo (a + b)`, is dropped right after
    //   that read;
    // - a *borrowed* value (see `TExpr::is_borrowed`) is never dropped by
    //   whoever reads it: a value read through its owner's name (`echo x`),
    //   whose owner drops it later, or an element still inside its array
    //   (`echo (head a)`). If that array is itself a temporary, as in
    //   `echo (head (f x))`, it lives until the element is no longer used,
    //   and is dropped then.
    //
    // All of this is decided at compile time. The generated code just calls
    // `rt.free` at the right places, with no reference counts or flags.

    /// Done reading the value `v` of `e`: drop it if it is a temporary. A
    /// borrowed value is not dropped, but a temporary it was borrowed from
    /// is.
    fn drop_temp(&mut self, e: &TExpr, v: Value) {
        if e.is_borrowed() {
            self.release(e);
        } else {
            self.unpend(e);
            self.fns.drop(&mut self.b, v, e.ty);
        }
    }

    /// Evaluate the operands of one operation, in order. If a later one may
    /// leave (`ret`, `over`, `jump`), the values of the earlier ones that
    /// are still to be used or dropped are pending while it runs, so that
    /// leaving frees them; afterwards they are the operation's again.
    fn operands(&mut self, items: &[(&TExpr, Use)]) -> Vec<Value> {
        let mut vals = Vec::with_capacity(items.len());
        let mut held = Vec::new();
        for (k, &(e, usage)) in items.iter().enumerate() {
            let v = self.expr(e, usage);
            vals.push(v);
            let owned = e.ty.needs_drop() && (usage == Use::Move || !e.is_borrowed());
            if owned && items[k + 1..].iter().any(|(later, _)| may_leave(later)) {
                self.pending.push(Pending {
                    key: e,
                    value: v,
                    ty: e.ty,
                });
                held.push(e);
            }
        }
        for e in held {
            self.unpend(e);
        }
        vals
    }

    /// Drop the temporaries the borrowed value `e` was taken from.
    fn release(&mut self, e: &TExpr) {
        if let Some((v, ty)) = self.unpend(e) {
            self.fns.drop(&mut self.b, v, ty);
        }
        match &e.kind {
            TExprKind::Index(a, _) | TExprKind::Convert(a) | TExprKind::Field(a, _)
                if a.is_borrowed() =>
            {
                self.release(a)
            }
            _ => {}
        }
    }

    /// Take the pending drop of `key`'s evaluation off the list (it is
    /// about to happen, or the value went elsewhere).
    fn unpend(&mut self, key: &TExpr) -> Option<(Value, Ty)> {
        let k = self
            .pending
            .iter()
            .rposition(|p| std::ptr::eq(p.key, key))?;
        let p = self.pending.remove(k);
        Some((p.value, p.ty))
    }

    /// Drop the values of the owners in `owned`, except those also in
    /// `keep`.
    fn drop_owned(&mut self, owned: &Owned, keep: Option<&Owned>) {
        for (l, &own) in owned.locals.iter().enumerate() {
            if own && !keep.is_some_and(|k| k.locals[l]) {
                // A variable seen through a view still owns its box.
                let (v, ty) = match self.views.iter().rev().find(|w| w.local == l) {
                    Some(view) => (view.boxed, view.ty),
                    None => (
                        self.locals[l].expect("owned variable has a value"),
                        self.local_tys[l],
                    ),
                };
                self.fns.drop(&mut self.b, v, ty);
            }
        }
    }

    /// The end of a `check` arm: record where it continues to the merge
    /// block, unless it cannot get there (it returned, or stopped).
    fn arm_end(&mut self, ends: &mut Vec<ArmEnd>, value: Value) {
        if std::mem::take(&mut self.dead) {
            self.b.unreachable();
        } else if !self.b.is_terminated() {
            ends.push(ArmEnd {
                block: self.b.current(),
                value,
                owned: self.owned.clone(),
                locals: self.locals.clone(),
                local_tys: self.local_tys.clone(),
            });
        }
    }

    /// Enter a `check` arm where variable `l` is not void: read its payload
    /// with the narrowed type `ty`. Returns whether a view was opened.
    fn narrow_enter(&mut self, l: usize, ty: Ty) -> bool {
        let boxed = self.locals[l].expect("narrowed variable has a value");
        let orig = self.local_tys[l];
        // A variable whose value was moved away is never read again. (A
        // borrowed one owns nothing, yet has its value.)
        if orig.needs_drop() && !self.owned.locals[l] && !self.lent[l] {
            return false;
        }
        let payload = match ty.as_kind() {
            Some(k) => self.unbox_payload(k, boxed),
            None => boxed, // still a box, of the other kinds
        };
        self.locals[l] = Some(payload);
        self.local_tys[l] = ty;
        self.views.push(View {
            local: l,
            boxed,
            ty: orig,
            moved_out: false,
        });
        true
    }

    /// Leave the arm: the variable is its box again, unless the arm moved
    /// the payload away.
    fn narrow_exit(&mut self) {
        let view = self.views.pop().expect("a view is open");
        self.locals[view.local] = Some(view.boxed);
        self.local_tys[view.local] = view.ty;
        if view.moved_out {
            self.owned.locals[view.local] = false;
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
            TExprKind::Local(l) => {
                let v = self.locals[*l].expect("local used before definition");
                // A boxed number is copied, not moved (its type is `Copy`).
                if moved && e.ty.is_copy() {
                    return self.fns.clone(&mut self.b, v, e.ty);
                }
                if moved {
                    // Moving a payload out of a variable seen through a view:
                    // the payload goes on, and the box around it is freed.
                    if let Some(view) = self.views.iter_mut().rev().find(|w| w.local == *l) {
                        if view.boxed != v {
                            self.b.call("rt.free", vec![view.boxed], None);
                        }
                        view.moved_out = true;
                    }
                    self.owned.locals[*l] = false;
                }
                v
            }
            TExprKind::Void => self.b.iconst(0),
            TExprKind::Ret(value) => {
                self.ret(value.as_deref());
                self.dummy(e.ty)
            }
            TExprKind::Over | TExprKind::Jump => {
                self.leave_round(matches!(e.kind, TExprKind::Over));
                self.dummy(e.ty)
            }
            TExprKind::Array(items) => {
                let elem = e.ty.elem().unwrap_or(Ty::NEVER);
                let items: Vec<_> = items.iter().map(|i| (i, Use::Move)).collect();
                let vals = self.operands(&items);
                let n = self.b.iconst(vals.len() as i64);
                let array = self
                    .b
                    .call("rt.array_new", vec![n], Some(Type::I64))
                    .unwrap();
                for (k, v) in vals.into_iter().enumerate() {
                    let k = self.b.iconst(k as i64);
                    store_elem(&mut self.b, array, k, elem, v);
                }
                array
            }
            TExprKind::Range(start, end) => self.range_array(start, end),
            TExprKind::Comprehension { body, clauses } => match &clauses[..] {
                // One generator (without a position) and guards: the array
                // can be allocated at its largest size up front.
                [
                    TClause::Gen {
                        pos: None,
                        var,
                        source,
                        borrowed,
                    },
                    rest @ ..,
                ] if rest.iter().all(|c| matches!(c, TClause::Guard(_))) => {
                    let guards: Vec<&TExpr> = rest
                        .iter()
                        .map(|c| match c {
                            TClause::Guard(g) => g,
                            TClause::Gen { .. } => unreachable!(),
                        })
                        .collect();
                    self.comprehension(body, *var, source, *borrowed, &guards, e.ty)
                }
                _ => self.comprehension_general(body, clauses, e.ty),
            },
            TExprKind::Construct {
                trail,
                steps,
                slots,
            } => self.construct(trail, steps, slots),
            TExprKind::Field(base, k) => self.field(e, base, *k),
            // What `echo` would print, printed into a new string instead.
            TExprKind::Textualize(inner) => {
                let v = self.expr(inner, Use::Borrow);
                let text = self.text(v, inner.ty);
                self.drop_temp(inner, v);
                text
            }
            TExprKind::Concat(a, b) => self.concat(a, b, e.ty),
            TExprKind::ElemMul(a, b) => self.elem_mul(a, b, e.ty),
            TExprKind::Index(a, i) => self.index(e, a, i, e.ty),
            // Arguments move into the callee, except those of parameters it
            // borrows (`@a`): the caller keeps them, and drops them after the
            // call if they are temporaries.
            TExprKind::Call { inst, args } => {
                let callee = &self.tir.funcs[*inst];
                let operands: Vec<_> = args
                    .iter()
                    .zip(&callee.borrowed)
                    .map(|(a, &b)| (a, if b { Use::Borrow } else { Use::Move }))
                    .collect();
                let vals = self.operands(&operands);
                if self.tail.contains(&(e as *const TExpr))
                    && reuses_frame(
                        self.tir,
                        self.f,
                        &self.assigned,
                        *inst,
                        args,
                        &callee.params,
                    )
                {
                    self.restart(vals);
                    let after = self.b.new_block("after_tail_call");
                    self.b.switch_to(after);
                    self.dead = true;
                    return self.dummy(e.ty);
                }
                let ret = if callee.discard {
                    None
                } else {
                    ret_repr(callee.ret)
                };
                let r = match self.b.call(&callee.symbol, vals.clone(), ret) {
                    Some(v) => v,
                    None => self.dummy(e.ty),
                };
                for ((a, &b), &v) in args.iter().zip(&callee.borrowed).zip(&vals) {
                    if b {
                        self.drop_temp(a, v);
                    }
                }
                r
            }
            // A call decided at run time:
            //
            //   test case 1 --yes--> convert args, call 1 --+
            //       |no                                    |
            //   test case 2 --yes--> convert args, call 2 --+--> dispatch_end(result)
            //       |no                                    |
            //   rt.no_match (only if the cases may all fail)
            //
            // A test checks the tags of the boxed arguments whose type is
            // not known well enough at compile time.
            TExprKind::Dispatch {
                name,
                args,
                cases,
                exhaustive,
            } => {
                // Every case borrows the same parameters (see `hir`).
                let borrowed = &self.tir.funcs[cases[0].inst].borrowed;
                let operands: Vec<_> = args
                    .iter()
                    .zip(borrowed)
                    .map(|(a, &b)| (a, if b { Use::Borrow } else { Use::Move }))
                    .collect();
                let vals = self.operands(&operands);
                let merge = self.b.new_block("dispatch_end");
                let result = self.b.block_param(merge, repr(e.ty));
                for case in cases {
                    let take = self.b.new_block("case");
                    let next = self.b.new_block("next_case");
                    // One test after the other, each leaving for the next
                    // case when it fails: a length is only read once the
                    // kind test has shown the value is an array.
                    for (j, (&v, a)) in vals.iter().zip(args).enumerate() {
                        if let Some(t) = case.tests[j] {
                            debug_assert!(a.ty.is_union(), "only boxed values are tested");
                            let tag = self.b.load(MemTy::I64, v, 0);
                            let hit = self.tag_in(tag, t);
                            let pass = self.b.new_block("test_pass");
                            self.b.br(hit, pass, vec![], next, vec![]);
                            self.b.switch_to(pass);
                        }
                        if let Some(test) = case.lens[j] {
                            let array = if a.ty.is_union() {
                                self.b.load(MemTy::I64, v, 8)
                            } else {
                                v
                            };
                            let len = self.b.load(MemTy::I64, array, 0);
                            let hit = match test {
                                LenTest::Empty => {
                                    let zero = self.b.iconst(0);
                                    self.b.icmp(ICond::Eq, len, zero)
                                }
                                LenTest::AtLeast(n) => {
                                    let n = self.b.iconst(n as i64);
                                    self.b.icmp(ICond::Sge, len, n)
                                }
                            };
                            let pass = self.b.new_block("test_pass");
                            self.b.br(hit, pass, vec![], next, vec![]);
                            self.b.switch_to(pass);
                        }
                    }
                    self.b.jmp(take, vec![]);
                    self.b.switch_to(take);
                    if self.tail.contains(&(e as *const TExpr))
                        && reuses_frame(
                            self.tir,
                            self.f,
                            &self.assigned,
                            case.inst,
                            args,
                            &case.params,
                        )
                    {
                        // The arguments are only converted (`reuses_frame`
                        // made sure a borrowed one needs no conversion).
                        let converted: Vec<Value> = vals
                            .iter()
                            .zip(args)
                            .zip(&case.params)
                            .map(|((&v, a), &to)| self.convert(v, a.ty, to))
                            .collect();
                        self.restart(converted);
                        self.b.switch_to(next);
                        continue;
                    }
                    let converted: Vec<Value> = vals
                        .iter()
                        .zip(args)
                        .zip(&case.params)
                        .zip(borrowed)
                        .map(|(((&v, a), &to), &b)| {
                            if b {
                                self.convert_borrowed(v, a.ty, to)
                            } else {
                                self.convert(v, a.ty, to)
                            }
                        })
                        .collect();
                    let callee = &self.tir.funcs[case.inst];
                    let ret = if callee.discard {
                        None
                    } else {
                        ret_repr(callee.ret)
                    };
                    let callee_ret = if callee.discard { Ty::VOID } else { callee.ret };
                    let r = match self.b.call(&callee.symbol, converted, ret) {
                        Some(v) => self.convert(v, callee_ret, e.ty),
                        // A definition giving void: the result is the void
                        // (boxed, if the call may also give something else).
                        None if callee_ret == Ty::VOID => {
                            let void = self.b.iconst(0);
                            self.convert(void, Ty::VOID, e.ty)
                        }
                        None => self.dummy(e.ty),
                    };
                    self.b.jmp(merge, vec![r]);
                    self.b.switch_to(next);
                }
                if *exhaustive {
                    self.b.unreachable();
                } else {
                    let msg = self.string_const(name);
                    self.b.call("rt.no_match", vec![msg], None);
                    self.b.unreachable();
                }
                self.b.switch_to(merge);
                for ((a, &b), &v) in args.iter().zip(borrowed).zip(&vals) {
                    if b {
                        self.drop_temp(a, v);
                    }
                }
                result
            }
            // The call is never reached: its arguments are only evaluated
            // (and the ones that come before the one that never finishes
            // are freed on the way out, as pending).
            TExprKind::Diverge(args) => {
                let operands: Vec<_> = args.iter().map(|a| (a, Use::Borrow)).collect();
                let vals = self.operands(&operands);
                for (a, v) in args.iter().zip(vals) {
                    if !a.ty.is_never() {
                        self.drop_temp(a, v);
                    }
                }
                self.dummy(e.ty)
            }
            TExprKind::Binary { op, lhs, rhs } => {
                let v = self.operands(&[(lhs, Use::Borrow), (rhs, Use::Borrow)]);
                let (l, r) = (v[0], v[1]);
                if e.ty.is_never() {
                    return self.dummy(e.ty);
                }
                let result = self.binary(*op, l, lhs.ty, r, rhs.ty, e.ty);
                // The operands were only read; the result is a new value.
                self.drop_temp(lhs, l);
                self.drop_temp(rhs, r);
                result
            }
            TExprKind::Is(inner, passes) => {
                let v = self.expr(inner, Use::Borrow);
                if e.ty.is_never() {
                    return self.dummy(e.ty);
                }
                // Only a union's kind is left to test: the value is a box,
                // whose tag says which kind it holds.
                let hit = if passes.mask() == inner.ty.mask() {
                    self.b.iconst(1)
                } else if passes.is_never() {
                    self.b.iconst(0)
                } else {
                    let tag = self.b.load(MemTy::I64, v, 0);
                    self.tag_in(tag, *passes)
                };
                self.drop_temp(inner, v);
                hit
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
                    // Nothing flows through.
                    Kind::Void => Some((Ty::VOID, this.b.iconst(0))),
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
                narrowing,
                compares,
                carried,
            } => {
                let s = self.expr(scrutinee, Use::Borrow);
                // A temporary being checked lives until the end of the
                // `check` (or a `ret` in an arm).
                if !scrutinee.is_borrowed() {
                    self.pending.push(Pending {
                        key: &**scrutinee,
                        value: s,
                        ty: scrutinee.ty,
                    });
                }
                let merge = self.b.new_block("check_end");
                let result = self.b.block_param(merge, repr(e.ty));
                // The variables the arms give new values (in the blocks of
                // `if`s) come to the merge block as parameters, like the
                // variables a loop updates.
                let params: Vec<Value> = carried
                    .iter()
                    .map(|&(_, ty)| self.b.block_param(merge, repr(ty)))
                    .collect();
                // Every arm, and every pattern tested, starts from the
                // variables as they are before the `check`.
                let (start_locals, start_tys) = (self.locals.clone(), self.local_tys.clone());
                let start_owned = self.owned.clone();
                let mut ends: Vec<ArmEnd> = Vec::new();
                // Lower one arm's body, seeing the narrowed variable if the
                // arm cannot run with it void.
                let arm_body = |this: &mut Self, body: &TExpr, narrowed: bool| {
                    let view = match narrowing {
                        Some(n) if narrowed => this.narrow_enter(n.local, n.ty),
                        _ => false,
                    };
                    let v = this.expr(body, Use::Move);
                    if view {
                        this.narrow_exit();
                    }
                    v
                };
                for (k, (pat, body)) in arms.iter().enumerate() {
                    self.locals.clone_from(&start_locals);
                    self.local_tys.clone_from(&start_tys);
                    let p = self.expr(pat, Use::Borrow);
                    let cond = if scrutinee.ty.is_never() || pat.ty.is_never() {
                        self.b.iconst(0)
                    } else if let Some(inst) = compares[k] {
                        // a value of a trail: equal when `compare` gives 0
                        let symbol = self.tir.funcs[inst].symbol.clone();
                        let order = self.b.call(&symbol, vec![s, p], Some(Type::I64)).unwrap();
                        let zero = self.b.iconst(0);
                        self.b.icmp(ICond::Eq, order, zero)
                    } else {
                        self.matches(s, scrutinee.ty, p, pat.ty)
                    };
                    self.drop_temp(pat, p);
                    let arm = self.b.new_block("arm");
                    let next = self.b.new_block("next");
                    self.b.br(cond, arm, vec![], next, vec![]);
                    self.b.switch_to(arm);
                    let before = self.owned.clone();
                    let narrowed = narrowing.as_ref().is_some_and(|n| n.arms[k]);
                    let v = arm_body(self, body, narrowed);
                    self.arm_end(&mut ends, v);
                    self.owned = before;
                    self.b.switch_to(next);
                }
                self.locals.clone_from(&start_locals);
                self.local_tys.clone_from(&start_tys);
                match else_arm {
                    Some(body) => {
                        let narrowed = narrowing.as_ref().is_some_and(|n| n.else_arm);
                        let v = arm_body(self, body, narrowed);
                        self.arm_end(&mut ends, v);
                    }
                    // Typeck proved a bool `check` covers both values, so
                    // falling through every arm cannot happen. `unreachable`
                    // says so (and traps if the proof were ever wrong).
                    None => self.b.unreachable(),
                }
                // Each arm converts the carried variables to their type
                // after the `check` (see `carry`), which may change what
                // it owns.
                let mut args = Vec::new();
                for end in &mut ends {
                    self.b.switch_to(end.block);
                    self.owned = end.owned.clone();
                    self.locals = std::mem::take(&mut end.locals);
                    self.local_tys = std::mem::take(&mut end.local_tys);
                    let mut a = vec![end.value];
                    a.extend(carried.iter().map(|&(l, ty)| self.carry(l, ty)));
                    args.push(a);
                    end.owned = self.owned.clone();
                    end.locals = std::mem::take(&mut self.locals);
                    end.local_tys = std::mem::take(&mut self.local_tys);
                }
                let after = ends
                    .iter()
                    .map(|end| end.owned.clone())
                    .reduce(|a, b| a.meet(&b))
                    .unwrap_or(start_owned);
                // If every arm returned, the code after the `check` is dead.
                self.dead = ends.is_empty();
                for (end, a) in ends.into_iter().zip(args) {
                    self.b.switch_to(end.block);
                    self.locals = end.locals;
                    self.local_tys = end.local_tys;
                    self.drop_owned(&end.owned, Some(&after));
                    self.b.jmp(merge, a);
                }
                self.owned = after;
                self.locals = start_locals;
                self.local_tys = start_tys;
                for (&(l, ty), &p) in carried.iter().zip(&params) {
                    self.locals[l] = Some(p);
                    self.local_tys[l] = ty;
                }
                self.b.switch_to(merge);
                if scrutinee.ty.is_never() {
                    self.unpend(scrutinee);
                } else {
                    self.drop_temp(scrutinee, s);
                }
                result
            }
            // The block of an `if`: its value is a new value, which the last
            // statement gives up (see `ownership`). The block's own
            // variables end with it.
            TExprKind::Block { stmts, inner } => {
                let n = stmts.len();
                let mut value = None;
                for (i, stmt) in stmts.iter().enumerate() {
                    match stmt {
                        TStmt::Expr(v) if i + 1 == n => value = Some(self.expr(v, Use::Move)),
                        _ => self.stmt(stmt),
                    }
                }
                let value = value.unwrap_or_else(|| self.dummy(e.ty));
                if !self.dead {
                    for &l in inner {
                        if self.owned.locals[l] {
                            let v = self.locals[l].expect("owned variable has a value");
                            self.fns.drop(&mut self.b, v, self.local_tys[l]);
                        }
                    }
                }
                for &l in inner {
                    self.owned.locals[l] = false;
                }
                value
            }
            TExprKind::Convert(inner) => {
                let v = self.expr(inner, usage);
                // A borrowed value is converted without taking it: unboxing
                // leaves its box alone.
                if usage == Use::Borrow && inner.is_borrowed() {
                    self.convert_borrowed(v, inner.ty, e.ty)
                } else {
                    self.convert(v, inner.ty, e.ty)
                }
            }
        }
    }

    // -- representation changes -------------------------------------------

    /// Wrap a concrete value in a heap-allocated `{ tag, payload }` box.
    fn box_value(&mut self, kind: Kind, v: Value) -> Value {
        // There is only one void: every boxed void is the same immortal box
        // in the runtime's data, never allocated and never freed.
        if kind == Kind::Void {
            return self.void_box();
        }
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

    fn void_box(&mut self) -> Value {
        let header = self.b.addr("rt.void_box");
        let header_size = self.b.iconst(8);
        self.b.ibin(IBinOp::Add, header, header_size)
    }

    fn unbox_payload(&mut self, kind: Kind, boxed: Value) -> Value {
        let mt = if kind == Kind::Float {
            MemTy::F64
        } else {
            MemTy::I64
        };
        self.b.load(mt, boxed, 8)
    }

    /// 1 if the kind with `tag` is in `ty`, else 0. The test reuses the
    /// bit-set encoding of `Ty`: the kind with tag t is bit t-1.
    fn tag_in(&mut self, tag: Value, ty: Ty) -> Value {
        let one = self.b.iconst(1);
        let shift = self.b.ibin(IBinOp::Sub, tag, one);
        let bit = self.b.ibin(IBinOp::Shl, one, shift);
        let mask = self.b.iconst(ty.mask());
        let hit = self.b.ibin(IBinOp::And, bit, mask);
        let zero = self.b.iconst(0);
        self.b.icmp(ICond::Ne, hit, zero)
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
        let hit = self.tag_in(tag, allowed);
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

    /// `convert` for a borrowed value, which stays with its owner: unboxing
    /// reads the payload and leaves the box alone, and boxing builds a
    /// box on the stack (with an immortal header, so nothing frees it),
    /// which is enough for a value that only lives while it is read.
    fn convert_borrowed(&mut self, v: Value, from: Ty, to: Ty) -> Value {
        if from == to {
            return v;
        }
        if from.is_never() {
            return self.dummy(to);
        }
        match (from.as_kind(), to.as_kind()) {
            (Some(Kind::Void), None) => self.void_box(),
            (Some(k), None) => {
                let cell = self.b.alloca(24);
                let immortal = self.b.iconst(IMMORTAL);
                self.b.store(MemTy::I64, cell, 0, immortal);
                let tag = self.b.iconst(k.tag());
                self.b.store(MemTy::I64, cell, 8, tag);
                let payload = if k == Kind::Float {
                    self.b.bitcast(v, Type::I64)
                } else {
                    v
                };
                self.b.store(MemTy::I64, cell, 16, payload);
                let header_size = self.b.iconst(8);
                self.b.ibin(IBinOp::Add, cell, header_size)
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
            (Some(_), Some(_)) => v,
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
                    Some(rk) => {
                        let v = this.concrete_binary(op, lt.only(ka), lv, rt.only(kb), rv);
                        Some((Ty::of(rk), v))
                    }
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
    /// 1 if the `check` pattern `p` matches the value `s`, else 0: equal
    /// values, where void matches only void.
    fn matches(&mut self, s: Value, sty: Ty, p: Value, pty: Ty) -> Value {
        self.dispatch(s, sty, Ty::BOOL, |this, ka, sv| {
            let r = this.dispatch(p, pty, Ty::BOOL, |this, kb, pv| {
                let r = match (ka, kb) {
                    (Kind::Void, Kind::Void) => this.b.iconst(1),
                    (Kind::Void, _) | (_, Kind::Void) => this.b.iconst(0),
                    _ => this.concrete_binary(BinOp::Eq, sty.only(ka), sv, pty.only(kb), pv),
                };
                Some((Ty::BOOL, r))
            });
            Some((Ty::BOOL, r))
        })
    }

    fn concrete_binary(&mut self, op: BinOp, ta: Ty, a: Value, tb: Ty, b: Value) -> Value {
        use Kind::*;
        let (ka, kb) = (
            ta.as_kind().expect("one kind"),
            tb.as_kind().expect("one kind"),
        );
        // Nothing flows through: an operation on void gives void.
        if ka == Void || kb == Void {
            return self.b.iconst(0);
        }
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
            // Two ints (see `binop_result`): the runtime stops the program
            // on `x % 0` instead of letting the machine trap.
            BinOp::Mod => self.b.call("rt.mod", vec![a, b], Some(Type::I64)).unwrap(),
            BinOp::Concat | BinOp::Index | BinOp::ElemMul | BinOp::And | BinOp::Or => {
                unreachable!("lowered separately")
            }
            BinOp::Eq | BinOp::Ne => {
                let eq = if ka == Array && kb == Array {
                    if ta == tb {
                        self.fns.eq(&mut self.b, a, b, ta)
                    } else {
                        // Arrays of different element types are equal only
                        // when both are empty.
                        let la = self.b.load(MemTy::I64, a, 0);
                        let lb = self.b.load(MemTy::I64, b, 0);
                        let both = self.b.ibin(IBinOp::Or, la, lb);
                        let zero = self.b.iconst(0);
                        self.b.icmp(ICond::Eq, both, zero)
                    }
                } else if ka == Int && kb == Int || ka == Bool && kb == Bool {
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
