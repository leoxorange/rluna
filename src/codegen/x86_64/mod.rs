//! x86-64 code generation from LIR (Windows x64 calling convention).
//!
//! This first backend is deliberately simple: every SSA value lives in its
//! own 8-byte stack slot, and each instruction loads its operands into
//! scratch registers, computes, and stores the result back. It produces
//! correct code without a register allocator; LIR keeps enough information
//! to add one later.
//!
//! Frame layout (rbp-relative, growing down):
//!
//! ```text
//!   [rbp + 16 + 8*i]   incoming stack arguments (i >= 4)
//!   [rbp + 8]          return address
//!   [rbp]              saved rbp
//!   value slots, alloca regions, block-argument scratch
//!   outgoing stack arguments, then 32 bytes of shadow space at [rsp]
//! ```
//!
//! Compare with the AArch64 backend: the LIR → machine mapping is the same
//! idea, and the differences come from the CPU and the platform's rules.
//!
//! - x86 instructions mostly have two operands, the first also being the
//!   destination (`add rax, rcx` means rax += rcx), and they vary in length
//!   from 1 to 15 bytes.
//! - `call` pushes the return address on the stack (AArch64 puts it in a
//!   register), which is why incoming arguments start at rbp + 16.
//! - The Windows x64 convention assigns argument registers by *position*
//!   (argument 2 goes in rdx or xmm1, whatever argument 1 was), and the
//!   caller must reserve 32 bytes of "shadow space" where the callee may
//!   spill its four register arguments.

pub mod asm;

use std::collections::HashMap;

use crate::codegen::{CodeFunc, import_symbol};
use crate::lir::{
    FBinOp, FCond, Function, IBinOp, ICond, MemTy, Module, Op, Terminator, Type, Value,
};
use asm::*;

pub fn compile_module(m: &Module) -> Vec<CodeFunc> {
    let externs: HashMap<&str, ()> = m.externs.iter().map(|e| (e.name.as_str(), ())).collect();
    m.funcs
        .iter()
        .map(|f| compile_function(f, &externs))
        .collect()
}

struct Frame {
    slots: Vec<i32>,
    allocas: HashMap<Value, i32>,
    scratch: i32,
    size: i32,
}

fn layout(f: &Function) -> Frame {
    let mut offset = 0i32;
    let mut slots = Vec::with_capacity(f.values.len());
    for _ in &f.values {
        offset += 8;
        slots.push(-offset);
    }
    let mut allocas = HashMap::new();
    let mut max_call_args = 0usize;
    let mut max_block_args = 0usize;
    for b in &f.blocks {
        for inst in &b.insts {
            match &inst.op {
                Op::Alloca(n) => {
                    offset += ((*n as i32) + 7) & !7;
                    allocas.insert(inst.result.unwrap(), -offset);
                }
                Op::Call(_, args) => max_call_args = max_call_args.max(args.len()),
                _ => {}
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
    offset += 8 * max_block_args as i32;
    let scratch = -offset;
    // Shadow space plus arguments 5, 6, ...; sized for the largest call.
    let outgoing = 32 + 8 * max_call_args.saturating_sub(4) as i32;
    // The ABI requires rsp to be 16-byte aligned at every `call`. On entry
    // rsp is 8 off (the return address was pushed); `push rbp` fixes that,
    // so the frame size itself must be a multiple of 16.
    let size = (offset + outgoing + 15) & !15;
    Frame {
        slots,
        allocas,
        scratch,
        size,
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

    fn ld(&mut self, r: Reg, v: Value) {
        let s = self.slot(v);
        self.a.load(r, RBP, s);
    }

    fn st(&mut self, v: Value, r: Reg) {
        let s = self.slot(v);
        self.a.store(RBP, s, r);
    }

    fn ldx(&mut self, x: Xmm, v: Value) {
        let s = self.slot(v);
        self.a.load_sd(x, RBP, s);
    }

    fn stx(&mut self, v: Value, x: Xmm) {
        let s = self.slot(v);
        self.a.store_sd(RBP, s, x);
    }

    /// Function entry: save the caller's rbp, make rbp point at our frame
    /// (a fixed reference while rsp moves), reserve the frame, and copy the
    /// register arguments into their slots before any call overwrites them.
    fn prologue(&mut self) {
        self.a.push_rbp();
        self.a.mov_rr(RBP, RSP);
        if self.frame.size > 0 {
            self.a.sub_rsp(self.frame.size);
        }
        let params = self.f.blocks[0].params.clone();
        for (i, &p) in params.iter().enumerate() {
            if i < 4 {
                match self.ty(p) {
                    Type::I64 => self.st(p, ARG_REGS[i]),
                    Type::F64 => self.stx(p, Xmm(i as u8)),
                }
            } else {
                self.a.load(RAX, RBP, 16 + 8 * i as i32);
                self.st(p, RAX);
            }
        }
    }

    fn epilogue(&mut self) {
        self.a.mov_rr(RSP, RBP);
        self.a.pop_rbp();
        self.a.ret();
    }

    fn inst(&mut self, result: Option<Value>, op: &Op) {
        let d = result;
        match op {
            Op::Iconst(c) => {
                self.a.mov_imm(RAX, *c);
                self.st(d.unwrap(), RAX);
            }
            Op::Fconst(c) => {
                self.a.mov_imm(RAX, c.to_bits() as i64);
                self.st(d.unwrap(), RAX);
            }
            Op::IBin(op, x, y) => {
                self.ld(RAX, *x);
                self.ld(RCX, *y);
                match op {
                    IBinOp::Add => self.a.alu(Alu::Add, RAX, RCX),
                    IBinOp::Sub => self.a.alu(Alu::Sub, RAX, RCX),
                    IBinOp::And => self.a.alu(Alu::And, RAX, RCX),
                    IBinOp::Or => self.a.alu(Alu::Or, RAX, RCX),
                    IBinOp::Xor => self.a.alu(Alu::Xor, RAX, RCX),
                    IBinOp::Mul => self.a.imul(RAX, RCX),
                    IBinOp::Shl => self.a.shift_cl(Shift::Shl, RAX),
                    IBinOp::Shr => self.a.shift_cl(Shift::Shr, RAX),
                    IBinOp::Sar => self.a.shift_cl(Shift::Sar, RAX),
                    // x86 division divides the 128-bit number rdx:rax and
                    // leaves the quotient in rax and the remainder in rdx.
                    // So rdx must first be filled with rax's sign (`cqo`)
                    // or with zero for unsigned division.
                    IBinOp::SDiv | IBinOp::SRem => {
                        self.a.cqo();
                        self.a.div(RCX, true);
                    }
                    IBinOp::UDiv | IBinOp::URem => {
                        self.a.zero_rdx();
                        self.a.div(RCX, false);
                    }
                }
                let out = if matches!(op, IBinOp::SRem | IBinOp::URem) {
                    RDX
                } else {
                    RAX
                };
                self.st(d.unwrap(), out);
            }
            Op::FBin(op, x, y) => {
                self.ldx(Xmm(0), *x);
                self.ldx(Xmm(1), *y);
                let sse = match op {
                    FBinOp::Add => Sse::Add,
                    FBinOp::Sub => Sse::Sub,
                    FBinOp::Mul => Sse::Mul,
                    FBinOp::Div => Sse::Div,
                };
                self.a.sse(sse, Xmm(0), Xmm(1));
                self.stx(d.unwrap(), Xmm(0));
            }
            Op::Icmp(c, x, y) => {
                self.ld(RAX, *x);
                self.ld(RCX, *y);
                self.a.alu(Alu::Cmp, RAX, RCX);
                let cc = match c {
                    ICond::Eq => Cond::E,
                    ICond::Ne => Cond::NE,
                    ICond::Slt => Cond::L,
                    ICond::Sle => Cond::LE,
                    ICond::Sgt => Cond::G,
                    ICond::Sge => Cond::GE,
                    ICond::Ult => Cond::B,
                    ICond::Ule => Cond::BE,
                    ICond::Ugt => Cond::A,
                    ICond::Uge => Cond::AE,
                };
                self.a.setcc(cc, RAX);
                self.a.movzx8(RAX, RAX);
                self.st(d.unwrap(), RAX);
            }
            Op::Fcmp(c, x, y) => {
                self.ldx(Xmm(0), *x);
                self.ldx(Xmm(1), *y);
                // ucomisd sets ZF=PF=CF=1 for unordered (NaN) operands.
                // IEEE 754 says every comparison with NaN is false (except
                // `!=`), so the parity flag (PF) has to be checked too.
                // For `<` and `<=` the operands are swapped so the "above"
                // conditions can be used, which are already false when CF=1.
                match c {
                    FCond::Eq | FCond::Ne => {
                        self.a.ucomisd(Xmm(0), Xmm(1));
                        if *c == FCond::Eq {
                            self.a.setcc(Cond::E, RAX);
                            self.a.setcc(Cond::NP, RCX);
                            self.a.alu8(Alu::And, RAX, RCX);
                        } else {
                            self.a.setcc(Cond::NE, RAX);
                            self.a.setcc(Cond::P, RCX);
                            self.a.alu8(Alu::Or, RAX, RCX);
                        }
                    }
                    FCond::Gt => {
                        self.a.ucomisd(Xmm(0), Xmm(1));
                        self.a.setcc(Cond::A, RAX);
                    }
                    FCond::Ge => {
                        self.a.ucomisd(Xmm(0), Xmm(1));
                        self.a.setcc(Cond::AE, RAX);
                    }
                    FCond::Lt => {
                        self.a.ucomisd(Xmm(1), Xmm(0));
                        self.a.setcc(Cond::A, RAX);
                    }
                    FCond::Le => {
                        self.a.ucomisd(Xmm(1), Xmm(0));
                        self.a.setcc(Cond::AE, RAX);
                    }
                }
                self.a.movzx8(RAX, RAX);
                self.st(d.unwrap(), RAX);
            }
            Op::Sitofp(x) => {
                self.ld(RAX, *x);
                self.a.cvtsi2sd(Xmm(0), RAX);
                self.stx(d.unwrap(), Xmm(0));
            }
            Op::Fptosi(x) => {
                self.ldx(Xmm(0), *x);
                self.a.cvttsd2si(RAX, Xmm(0));
                self.st(d.unwrap(), RAX);
            }
            Op::Bitcast(x) => {
                self.ld(RAX, *x);
                self.st(d.unwrap(), RAX);
            }
            Op::Load(t, p, off) => {
                self.ld(RAX, *p);
                match t {
                    MemTy::U8 => self.a.load_u8(RAX, RAX, *off),
                    MemTy::I64 | MemTy::F64 => self.a.load(RAX, RAX, *off),
                }
                self.st(d.unwrap(), RAX);
            }
            Op::Store(t, p, off, v) => {
                self.ld(RAX, *p);
                self.ld(RCX, *v);
                match t {
                    MemTy::U8 => self.a.store_u8(RAX, *off, RCX),
                    MemTy::I64 | MemTy::F64 => self.a.store(RAX, *off, RCX),
                }
            }
            Op::Alloca(_) => {
                let r = d.unwrap();
                let off = self.frame.allocas[&r];
                self.a.lea(RAX, RBP, off);
                self.st(r, RAX);
            }
            Op::Addr(sym) => {
                let sym = if self.externs.contains_key(sym.as_str()) {
                    import_symbol(sym)
                } else {
                    sym.clone()
                };
                self.a.lea_symbol(RAX, &sym);
                self.st(d.unwrap(), RAX);
            }
            Op::Call(callee, args) => {
                // Stack arguments first (they use rax), then register arguments.
                for (i, &arg) in args.iter().enumerate().skip(4) {
                    self.ld(RAX, arg);
                    self.a.store(RSP, 32 + 8 * (i as i32 - 4), RAX);
                }
                for (i, &arg) in args.iter().enumerate().take(4) {
                    match self.ty(arg) {
                        Type::I64 => self.ld(ARG_REGS[i], arg),
                        Type::F64 => self.ldx(Xmm(i as u8), arg),
                    }
                }
                if self.externs.contains_key(callee.as_str()) {
                    self.a.call_indirect(&import_symbol(callee));
                } else {
                    self.a.call(callee);
                }
                if let Some(r) = d {
                    match self.ty(r) {
                        Type::I64 => self.st(r, RAX),
                        Type::F64 => self.stx(r, Xmm(0)),
                    }
                }
            }
        }
    }

    /// Copy branch arguments into the target block's parameters. Goes through
    /// a scratch area so arguments may freely refer to the parameters.
    fn block_args(&mut self, target: crate::lir::BlockId, args: &[Value]) {
        let params = self.f.blocks[target.0 as usize].params.clone();
        if args.len() == 1 {
            self.ld(RAX, args[0]);
            self.st(params[0], RAX);
            return;
        }
        for (i, &arg) in args.iter().enumerate() {
            self.ld(RAX, arg);
            self.a.store(RBP, self.frame.scratch + 8 * i as i32, RAX);
        }
        for (i, &p) in params.iter().enumerate() {
            self.a.load(RAX, RBP, self.frame.scratch + 8 * i as i32);
            self.st(p, RAX);
        }
    }

    fn jump_to(&mut self, target: crate::lir::BlockId, current: usize) {
        if target.0 as usize != current + 1 {
            self.a.jmp(self.labels[target.0 as usize]);
        }
    }

    fn terminator(&mut self, t: &Terminator, current: usize) {
        match t {
            Terminator::Jmp(target, args) => {
                self.block_args(*target, args);
                self.jump_to(*target, current);
            }
            Terminator::Br(c, then, then_args, els, else_args) => {
                self.ld(RAX, *c);
                self.a.test(RAX, RAX);
                if then_args.is_empty() {
                    self.a.jcc(Cond::NE, self.labels[then.0 as usize]);
                    self.block_args(*els, else_args);
                    self.jump_to(*els, current);
                } else if else_args.is_empty() {
                    self.a.jcc(Cond::E, self.labels[els.0 as usize]);
                    self.block_args(*then, then_args);
                    self.jump_to(*then, current);
                } else {
                    let else_edge = self.a.new_label();
                    self.a.jcc(Cond::E, else_edge);
                    self.block_args(*then, then_args);
                    self.a.jmp(self.labels[then.0 as usize]);
                    self.a.bind(else_edge);
                    self.block_args(*els, else_args);
                    self.jump_to(*els, current);
                }
            }
            Terminator::Ret(v) => {
                if let Some(v) = v {
                    match self.ty(*v) {
                        Type::I64 => self.ld(RAX, *v),
                        Type::F64 => self.ldx(Xmm(0), *v),
                    }
                }
                self.epilogue();
            }
            Terminator::Unreachable => self.a.ud2(),
        }
    }
}
