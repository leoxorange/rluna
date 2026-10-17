//! AArch64 code generation from LIR (Apple arm64 calling convention).
//!
//! Like the x86-64 backend, this one is deliberately simple: every SSA value
//! lives in its own 8-byte stack slot, and each instruction loads its
//! operands into scratch registers (x9, x10, d0, d1), computes, and stores
//! the result back.
//!
//! Frame layout:
//!
//! ```text
//!   [x29 + 16 + 8*i]   incoming stack arguments
//!   [x29 + 8]          saved x30 (return address)
//!   [x29]              saved x29
//!   block-argument scratch, alloca regions, value slots
//!   [sp + 8*i]         outgoing stack arguments
//! ```
//!
//! Integer arguments go in x0-x7 and float arguments in d0-d7, counted
//! separately; the rest go on the stack in 8-byte slots. Results come back
//! in x0 or d0.
//!
//! Why give every value a stack slot? A CPU has few registers (31 here) but
//! a function can have thousands of values. Deciding which values share
//! registers is *register allocation*, the hardest part of a real backend.
//! Keeping everything in memory skips it entirely: the code is correct and
//! easy to follow, at the cost of a load and store around every operation.
//!
//! Why follow the platform's calling convention exactly? Our functions call
//! `write` and `mmap` in libSystem, and dyld calls our `rt.start`. Code
//! compiled by different compilers can only call each other if they agree
//! where arguments, return values and the return address go. Apple
//! documents those rules (the "ABI") and every compiler on the platform
//! follows them.
//!
//! Every AArch64 instruction is exactly 4 bytes, and constants and offsets
//! must fit in a few bits of that word. Much of `asm.rs` deals with values
//! that do not fit (big constants, far stack slots), using the scratch
//! register x16.

pub mod asm;

use std::collections::HashMap;

use crate::codegen::{CodeFunc, import_symbol};
use crate::lir::{
    BlockId, FBinOp, FCond, Function, IBinOp, ICond, MemTy, Module, Op, Terminator, Type, Value,
};
use asm::*;

pub fn compile_module(m: &Module) -> Vec<CodeFunc> {
    let externs: HashMap<&str, ()> = m.externs.iter().map(|e| (e.name.as_str(), ())).collect();
    m.funcs
        .iter()
        .map(|f| compile_function(f, &externs))
        .collect()
}

/// Where an argument is passed. The same classification is used both for
/// calls we make and for our own parameters, so caller and callee always
/// agree.
#[derive(Clone, Copy)]
enum ArgLoc {
    X(X),
    D(D),
    /// Index of the 8-byte stack slot.
    Stack(i32),
}

fn classify(types: impl Iterator<Item = Type>) -> Vec<ArgLoc> {
    let (mut ints, mut floats, mut stack) = (0u8, 0u8, 0i32);
    types
        .map(|t| match t {
            Type::I64 if ints < 8 => {
                ints += 1;
                ArgLoc::X(X(ints - 1))
            }
            Type::F64 if floats < 8 => {
                floats += 1;
                ArgLoc::D(D(floats - 1))
            }
            _ => {
                stack += 1;
                ArgLoc::Stack(stack - 1)
            }
        })
        .collect()
}

/// The stack frame: the function's private memory, reserved on entry by
/// moving `sp` down and released on return by moving it back.
///
/// All offsets are sp-relative and non-negative. The `ldr`/`str` encodings
/// with a positive scaled offset reach 32 KiB from `sp` in one instruction,
/// while negative offsets from x29 reach only 256 bytes.
struct Frame {
    slots: Vec<i32>,
    allocas: HashMap<Value, i32>,
    scratch: i32,
    size: i32,
}

/// Decide the frame layout before emitting any code, because the prologue
/// (the first instructions) must already know the total size.
fn layout(f: &Function) -> Frame {
    // Outgoing arguments must sit at the very bottom of the frame, at
    // [sp + 0...], because that is where the callee looks for them (just
    // above its own frame). One area sized for the largest call is reused
    // by all calls.
    let mut outgoing = 0i32;
    let mut max_block_args = 0usize;
    for b in &f.blocks {
        for inst in &b.insts {
            if let Op::Call(_, args) = &inst.op {
                let stack = classify(args.iter().map(|&a| f.value_ty(a)))
                    .iter()
                    .filter(|l| matches!(l, ArgLoc::Stack(_)))
                    .count();
                outgoing = outgoing.max(8 * stack as i32);
            }
        }
        match &b.term {
            Some(Terminator::Jmp(_, a)) => max_block_args = max_block_args.max(a.len()),
            Some(Terminator::Br(_, _, a, _, c)) => {
                max_block_args = max_block_args.max(a.len().max(c.len()))
            }
            _ => {}
        }
    }
    let mut offset = outgoing;
    let mut slots = Vec::with_capacity(f.values.len());
    for _ in &f.values {
        slots.push(offset);
        offset += 8;
    }
    let mut allocas = HashMap::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let Op::Alloca(n) = &inst.op {
                allocas.insert(inst.result.unwrap(), offset);
                offset += ((*n as i32) + 7) & !7;
            }
        }
    }
    let scratch = offset;
    offset += 8 * max_block_args as i32;
    Frame {
        slots,
        allocas,
        scratch,
        // The ABI requires sp to be a multiple of 16 at all times; on
        // AArch64 the hardware itself faults on a misaligned sp access.
        size: (offset + 15) & !15,
    }
}

struct Emitter<'a> {
    f: &'a Function,
    frame: Frame,
    a: Asm,
    labels: Vec<Label>,
    externs: &'a HashMap<&'a str, ()>,
}

fn compile_function(f: &Function, externs: &HashMap<&str, ()>) -> CodeFunc {
    let frame = layout(f);
    let mut e = Emitter {
        f,
        frame,
        a: Asm::default(),
        labels: Vec::new(),
        externs,
    };
    // One label per block, created up front: a jump may target a block that
    // has not been emitted yet (forward jump), so its address is unknown
    // until later. Labels record the jump and patch it in
    // `resolve_labels`, once every block's position is known.
    e.labels = f.blocks.iter().map(|_| e.a.new_label()).collect();
    e.a.comment(format!("func {}", f.name));
    e.prologue();
    for (i, block) in f.blocks.iter().enumerate() {
        e.a.bind(e.labels[i]);
        e.a.comment(format!("block {}", block.name));
        for inst in &block.insts {
            e.inst(inst.result, &inst.op);
        }
        e.terminator(block.term.as_ref().expect("block without terminator"), i);
    }
    e.a.resolve_labels();
    CodeFunc {
        name: f.name.clone(),
        code: e.a.code,
        relocs: e.a.relocs,
        listing: e.a.listing,
    }
}

impl<'a> Emitter<'a> {
    fn slot(&self, v: Value) -> i32 {
        self.frame.slots[v.0 as usize]
    }

    fn ty(&self, v: Value) -> Type {
        self.f.value_ty(v)
    }

    fn ld(&mut self, r: X, v: Value) {
        let s = self.slot(v);
        self.a.mem(Mem::LdrX, r, SP, s);
    }

    fn st(&mut self, v: Value, r: X) {
        let s = self.slot(v);
        self.a.mem(Mem::StrX, r, SP, s);
    }

    fn ldd(&mut self, d: D, v: Value) {
        let s = self.slot(v);
        self.a.ldr_d(d, SP, s);
    }

    fn std(&mut self, v: Value, d: D) {
        let s = self.slot(v);
        self.a.str_d(d, SP, s);
    }

    /// Function entry. `bl` put the return address in x30 (the link
    /// register), and our own calls will overwrite x30, so it is saved on
    /// the stack together with the caller's frame pointer x29. x29 then
    /// points at this pair, which forms a linked list of frames that
    /// debuggers and crash reports follow to print a backtrace.
    fn prologue(&mut self) {
        self.a.push_frame();
        self.a.add_imm(FP, SP, 0);
        if self.frame.size > 0 {
            self.a.add_imm(SP, SP, -(self.frame.size as i64));
        }
        // Arguments arrive in registers, which the first call would
        // overwrite. Copy them into their slots right away.
        let params = self.f.blocks[0].params.clone();
        let locs = classify(params.iter().map(|&p| self.ty(p)));
        for (&p, loc) in params.iter().zip(locs) {
            match loc {
                ArgLoc::X(r) => self.st(p, r),
                ArgLoc::D(d) => self.std(p, d),
                ArgLoc::Stack(i) => {
                    self.a.mem(Mem::LdrX, X9, FP, 16 + 8 * i);
                    self.st(p, X9);
                }
            }
        }
    }

    /// Undo the prologue: `mov sp, x29` drops the whole frame at once
    /// (whatever its size), then restore x29/x30 and `ret` jumps to x30.
    fn epilogue(&mut self) {
        self.a.add_imm(SP, FP, 0);
        self.a.pop_frame();
        self.a.ret();
    }

    fn inst(&mut self, result: Option<Value>, op: &Op) {
        let d = result;
        match op {
            Op::Iconst(c) => {
                self.a.mov_imm(X9, *c);
                self.st(d.unwrap(), X9);
            }
            // There is no instruction that loads an arbitrary f64 constant.
            // But the slot is just 8 bytes of memory: store the float's
            // bit pattern as an integer, and later `ldr d0` reads it back
            // as the float.
            Op::Fconst(c) => {
                self.a.mov_imm(X9, c.to_bits() as i64);
                self.st(d.unwrap(), X9);
            }
            Op::IBin(op, x, y) => {
                self.ld(X9, *x);
                self.ld(X10, *y);
                let alu = match op {
                    IBinOp::Add => Alu::Add,
                    IBinOp::Sub => Alu::Sub,
                    IBinOp::Mul => Alu::Mul,
                    IBinOp::And => Alu::And,
                    IBinOp::Or => Alu::Orr,
                    IBinOp::Xor => Alu::Eor,
                    IBinOp::Shl => Alu::Lsl,
                    IBinOp::Shr => Alu::Lsr,
                    IBinOp::Sar => Alu::Asr,
                    IBinOp::SDiv | IBinOp::SRem => Alu::Sdiv,
                    IBinOp::UDiv | IBinOp::URem => Alu::Udiv,
                };
                // AArch64 has no remainder instruction (x86's `idiv` gives
                // both quotient and remainder). Compute it from the
                // quotient: a % b = a - (a / b) * b, which `msub` does in
                // one instruction.
                if matches!(op, IBinOp::SRem | IBinOp::URem) {
                    // x9 - (x9 / x10) * x10
                    self.a.alu(alu, X16, X9, X10);
                    self.a.msub(X9, X16, X10, X9);
                } else {
                    self.a.alu(alu, X9, X9, X10);
                }
                self.st(d.unwrap(), X9);
            }
            Op::FBin(op, x, y) => {
                self.ldd(D(0), *x);
                self.ldd(D(1), *y);
                let falu = match op {
                    FBinOp::Add => FAlu::Add,
                    FBinOp::Sub => FAlu::Sub,
                    FBinOp::Mul => FAlu::Mul,
                    FBinOp::Div => FAlu::Div,
                };
                self.a.falu(falu, D(0), D(0), D(1));
                self.std(d.unwrap(), D(0));
            }
            Op::Icmp(c, x, y) => {
                self.ld(X9, *x);
                self.ld(X10, *y);
                // `cmp` subtracts and sets the condition flags (N, Z, C, V);
                // `cset` turns one flag condition into 0 or 1. Signed and
                // unsigned comparisons read different flags, which is why
                // `slt` maps to `lt` but `ult` to `lo`.
                self.a.cmp(X9, X10);
                let cc = match c {
                    ICond::Eq => Cond::Eq,
                    ICond::Ne => Cond::Ne,
                    ICond::Slt => Cond::Lt,
                    ICond::Sle => Cond::Le,
                    ICond::Sgt => Cond::Gt,
                    ICond::Sge => Cond::Ge,
                    ICond::Ult => Cond::Lo,
                    ICond::Ule => Cond::Ls,
                    ICond::Ugt => Cond::Hi,
                    ICond::Uge => Cond::Hs,
                };
                self.a.cset(X9, cc);
                self.st(d.unwrap(), X9);
            }
            Op::Fcmp(c, x, y) => {
                self.ldd(D(0), *x);
                self.ldd(D(1), *y);
                self.a.fcmp(D(0), D(1));
                // Unordered (NaN) sets C and V: these conditions are false
                // for it, except `ne`, which is true.
                let cc = match c {
                    FCond::Eq => Cond::Eq,
                    FCond::Ne => Cond::Ne,
                    FCond::Lt => Cond::Mi,
                    FCond::Le => Cond::Ls,
                    FCond::Gt => Cond::Gt,
                    FCond::Ge => Cond::Ge,
                };
                self.a.cset(X9, cc);
                self.st(d.unwrap(), X9);
            }
            Op::Sitofp(x) => {
                self.ld(X9, *x);
                self.a.scvtf(D(0), X9);
                self.std(d.unwrap(), D(0));
            }
            Op::Fptosi(x) => {
                self.ldd(D(0), *x);
                self.a.fcvtzs(X9, D(0));
                self.st(d.unwrap(), X9);
            }
            // In memory, an i64 and an f64 are both just 8 bytes, so
            // reinterpreting one as the other is a plain copy between slots.
            Op::Bitcast(x) => {
                self.ld(X9, *x);
                self.st(d.unwrap(), X9);
            }
            Op::Load(t, p, off) => {
                self.ld(X9, *p);
                let m = match t {
                    MemTy::U8 => Mem::LdrB,
                    MemTy::I64 | MemTy::F64 => Mem::LdrX,
                };
                self.a.mem(m, X9, X9, *off);
                self.st(d.unwrap(), X9);
            }
            Op::Store(t, p, off, v) => {
                self.ld(X9, *p);
                self.ld(X10, *v);
                let m = match t {
                    MemTy::U8 => Mem::StrB,
                    MemTy::I64 | MemTy::F64 => Mem::StrX,
                };
                self.a.mem(m, X10, X9, *off);
            }
            Op::Alloca(_) => {
                let r = d.unwrap();
                let off = self.frame.allocas[&r];
                self.a.add_imm(X9, SP, off as i64);
                self.st(r, X9);
            }
            Op::Addr(sym) => {
                let sym = if self.externs.contains_key(sym.as_str()) {
                    import_symbol(sym)
                } else {
                    sym.clone()
                };
                self.a.adr_symbol(X9, &sym);
                self.st(d.unwrap(), X9);
            }
            // A call: put each argument where the convention says, branch,
            // and fetch the result from x0/d0. Every value lives in memory,
            // so filling x0-x7 cannot clobber anything we still need.
            Op::Call(callee, args) => {
                let locs = classify(args.iter().map(|&a| self.ty(a)));
                for (&arg, loc) in args.iter().zip(&locs) {
                    match *loc {
                        ArgLoc::X(r) => self.ld(r, arg),
                        ArgLoc::D(dr) => self.ldd(dr, arg),
                        ArgLoc::Stack(i) => {
                            self.ld(X9, arg);
                            self.a.mem(Mem::StrX, X9, SP, 8 * i);
                        }
                    }
                }
                // Our own functions are in the same executable, within
                // `bl`'s ±128 MiB reach. A library function may be anywhere
                // in memory, so load its address from the GOT and branch to
                // the register (`blr`).
                if self.externs.contains_key(callee.as_str()) {
                    self.a.load_symbol(X16, &import_symbol(callee));
                    self.a.blr(X16);
                } else {
                    self.a.bl(callee);
                }
                if let Some(r) = d {
                    match self.ty(r) {
                        Type::I64 => self.st(r, X0),
                        Type::F64 => self.std(r, D(0)),
                    }
                }
            }
        }
    }

    /// Copy branch arguments into the target block's parameters. Goes through
    /// a scratch area so arguments may freely refer to the parameters.
    ///
    /// Example of the problem: in a block `loop(%x, %y)` that swaps its two
    /// parameters with `jmp loop(%y, %x)`, copying directly would set
    /// %x = %y and then %y = %x, which is now the *new* %x, so the old %x is
    /// lost. Copying all arguments out first, then all in, is the simple
    /// fix. This is the "parallel copy" problem of leaving SSA form.
    fn block_args(&mut self, target: BlockId, args: &[Value]) {
        let params = self.f.blocks[target.0 as usize].params.clone();
        if args.len() == 1 {
            self.ld(X9, args[0]);
            self.st(params[0], X9);
            return;
        }
        for (i, &arg) in args.iter().enumerate() {
            self.ld(X9, arg);
            self.a
                .mem(Mem::StrX, X9, SP, self.frame.scratch + 8 * i as i32);
        }
        for (i, &p) in params.iter().enumerate() {
            self.a
                .mem(Mem::LdrX, X9, SP, self.frame.scratch + 8 * i as i32);
            self.st(p, X9);
        }
    }

    /// Blocks are emitted in order, so a jump to the very next block can be
    /// left out: execution simply "falls through" into it.
    fn jump_to(&mut self, target: BlockId, current: usize) {
        if target.0 as usize != current + 1 {
            self.a.b(self.labels[target.0 as usize]);
        }
    }

    fn terminator(&mut self, t: &Terminator, current: usize) {
        match t {
            Terminator::Jmp(target, args) => {
                self.block_args(*target, args);
                self.jump_to(*target, current);
            }
            // A conditional branch can only jump or fall through; it cannot
            // copy block arguments on the way. So the copying for the edge
            // that has arguments is done on the fall-through path, and when
            // both edges have arguments, an extra label is made for the
            // else-edge's copies.
            Terminator::Br(c, then, then_args, els, else_args) => {
                self.ld(X9, *c);
                if then_args.is_empty() {
                    self.a.cbz(X9, true, self.labels[then.0 as usize]);
                    self.block_args(*els, else_args);
                    self.jump_to(*els, current);
                } else if else_args.is_empty() {
                    self.a.cbz(X9, false, self.labels[els.0 as usize]);
                    self.block_args(*then, then_args);
                    self.jump_to(*then, current);
                } else {
                    let else_edge = self.a.new_label();
                    self.a.cbz(X9, false, else_edge);
                    self.block_args(*then, then_args);
                    self.a.b(self.labels[then.0 as usize]);
                    self.a.bind(else_edge);
                    self.block_args(*els, else_args);
                    self.jump_to(*els, current);
                }
            }
            Terminator::Ret(v) => {
                if let Some(v) = v {
                    match self.ty(*v) {
                        Type::I64 => self.ld(X0, *v),
                        Type::F64 => self.ldd(D(0), *v),
                    }
                }
                self.epilogue();
            }
            Terminator::Unreachable => self.a.brk(),
        }
    }
}
