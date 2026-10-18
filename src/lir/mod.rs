//! LIR — Luna's low-level intermediate representation.
//!
//! LIR is a typed SSA IR in the style of Cranelift/LLVM. Machine types are
//! just `i64` and `f64` (pointers and booleans are `i64`). Control flow uses
//! basic blocks with *block parameters* instead of phi nodes. It has a
//! textual form (`.lir` files) that can be printed and parsed back, so the
//! runtime library is itself written in LIR (`runtime/*.lir`).
//!
//! Why an IR between the language and the machine? Without one, every
//! language feature would need code for every CPU. With one, the frontend
//! (lexer → lower) only has to produce LIR, and each backend (x86-64,
//! AArch64) only has to understand LIR's ~30 instructions. Adding a CPU
//! doesn't touch the frontend, and changing the language doesn't touch the
//! backends. Optimizations written on LIR help every target at once.
//!
//! **SSA** (static single assignment): every value (`%5`) is assigned
//! exactly once and never changes. That makes analysis easy: wherever `%5`
//! appears, it is the same value, defined in exactly one place. For example,
//! "is this value used anywhere?" (dead code elimination, `opt.rs`) is a
//! simple scan.
//!
//! **Basic blocks**: straight-line instruction sequences that end in exactly
//! one *terminator* (jump, branch, return). Control can only enter a block
//! at its top and leave at its bottom, so within a block, instructions run
//! in order, always. Loops and `check` arms are built from blocks and jumps.
//!
//! **Block parameters**: a value that differs depending on how control
//! arrived (a loop counter, or the result of a `check`) is a parameter of
//! the block, and each jump passes an argument for it, like calling a
//! function. This is how SSA handles "variables that change"; LLVM does the
//! same with *phi nodes*, which are equivalent but harder to read.

pub mod opt;
pub mod parse;
pub mod print;
pub mod verify;

use std::fmt;

/// The only two machine types. CPUs have two register files, integer and
/// floating point, with different instructions and calling-convention
/// rules, so the backends must know which file a value lives in.
/// Everything else (booleans, pointers, tags) is an integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    I64,
    F64,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::I64 => "i64",
            Type::F64 => "f64",
        })
    }
}

/// An SSA value, `%n` in the text form. Values are numbered per function
/// and index into `Function::values`, where their type is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Value(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// Integer operations. Division, remainder and comparisons come in signed
/// and unsigned versions because the same 64 bits mean different numbers:
/// 0xFFFF...FF is -1 signed but 2^64-1 unsigned. The machine has separate
/// instructions for each, so the IR must say which one is meant. `shr` is
/// a logical shift (fills with 0) and `sar` an arithmetic one (copies the
/// sign bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IBinOp {
    Add,
    Sub,
    Mul,
    SDiv,
    SRem,
    UDiv,
    URem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FBinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ICond {
    Eq,
    Ne,
    Slt,
    Sle,
    Sgt,
    Sge,
    Ult,
    Ule,
    Ugt,
    Uge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FCond {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Width/type of a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemTy {
    I64,
    F64,
    /// Load zero-extends a byte to i64; store writes the low byte.
    U8,
}

impl MemTy {
    pub fn value_type(self) -> Type {
        match self {
            MemTy::F64 => Type::F64,
            _ => Type::I64,
        }
    }
}

/// An instruction. Each does one small thing a CPU can do in one or a few
/// machine instructions, and operands are always values, never nested
/// expressions. That is the gap lowering closes: `a + b * c` is a tree,
/// but `Op::IBin(Add, a, t)` only accepts the already-computed `t`.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Iconst(i64),
    Fconst(f64),
    IBin(IBinOp, Value, Value),
    FBin(FBinOp, Value, Value),
    Icmp(ICond, Value, Value),
    Fcmp(FCond, Value, Value),
    /// Signed int -> float.
    Sitofp(Value),
    /// Float -> signed int, truncating.
    Fptosi(Value),
    /// Reinterpret bits between i64 and f64.
    Bitcast(Value),
    Load(MemTy, Value, i32),
    Store(MemTy, Value, i32, Value),
    /// Address of a fixed-size, 8-byte aligned region in the stack frame.
    /// Stack memory is free to allocate and is released automatically when
    /// the function returns: ideal for small scratch buffers such as the
    /// digits of a number being printed.
    Alloca(u32),
    /// Address of a data symbol or function.
    Addr(String),
    Call(String, Vec<Value>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inst {
    pub result: Option<Value>,
    pub op: Op,
}

/// How a block ends. Keeping control flow out of the instruction list (and
/// allowing exactly one terminator per block) is what makes blocks
/// "straight-line": no jump can leave from the middle of one.
///
/// `Unreachable` marks a point that execution can never reach, such as
/// after a call to `rt.exit`. It produces a trap instruction, so if the
/// assumption is ever wrong, the program stops instead of running on into
/// whatever bytes follow.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminator {
    Jmp(BlockId, Vec<Value>),
    Br(Value, BlockId, Vec<Value>, BlockId, Vec<Value>),
    Ret(Option<Value>),
    Unreachable,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub name: String,
    pub params: Vec<Value>,
    pub insts: Vec<Inst>,
    pub term: Option<Terminator>,
}

#[derive(Debug, Clone)]
pub struct ValueInfo {
    pub ty: Type,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Option<Type>,
    /// The first block is the entry; its parameters are the function's parameters.
    pub blocks: Vec<Block>,
    pub values: Vec<ValueInfo>,
    /// Free-form comment printed above the function.
    pub comment: Option<String>,
}

impl Function {
    pub fn value_ty(&self, v: Value) -> Type {
        self.values[v.0 as usize].ty
    }
}

/// An imported function. `name` has the form `library.Function`, for example
/// `kernel32.WriteFile` imports `WriteFile` from `kernel32.dll` and
/// `libSystem.write` imports `write` from `libSystem.dylib`.
#[derive(Debug, Clone)]
pub struct Extern {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Option<Type>,
}

impl Extern {
    /// The library name without its file extension.
    pub fn library(&self) -> &str {
        self.name
            .rsplit_once('.')
            .map(|(lib, _)| lib)
            .unwrap_or("kernel32")
    }

    pub fn import_name(&self) -> &str {
        self.name
            .rsplit_once('.')
            .map(|(_, f)| f)
            .unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DataItem {
    I64(i64),
    Bytes(Vec<u8>),
    Zero(usize),
}

/// A writable, 8-byte aligned data object: global memory that exists for
/// the whole run, laid out by the compiler and loaded with the program.
/// String literals and the runtime's globals (heap pointer, output buffer)
/// live here.
#[derive(Debug, Clone)]
pub struct Data {
    pub name: String,
    pub items: Vec<DataItem>,
}

impl Data {
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for item in &self.items {
            match item {
                DataItem::I64(v) => out.extend_from_slice(&v.to_le_bytes()),
                DataItem::Bytes(b) => out.extend_from_slice(b),
                DataItem::Zero(n) => out.resize(out.len() + n, 0),
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct Module {
    pub externs: Vec<Extern>,
    pub data: Vec<Data>,
    pub funcs: Vec<Function>,
}

impl Module {
    pub fn extend(&mut self, other: Module) {
        self.externs.extend(other.externs);
        self.data.extend(other.data);
        self.funcs.extend(other.funcs);
    }

    /// Signature (params, ret) of a function or extern.
    pub fn signature(&self, name: &str) -> Option<(Vec<Type>, Option<Type>)> {
        if let Some(f) = self.funcs.iter().find(|f| f.name == name) {
            return Some((f.params.clone(), f.ret));
        }
        self.externs
            .iter()
            .find(|e| e.name == name)
            .map(|e| (e.params.clone(), e.ret))
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Incrementally builds a `Function`.
///
/// Lowering works like a cursor: "switch to block X, append instructions".
/// The builder tracks the current block, hands out fresh value numbers, and
/// asserts the structural rules (nothing after a terminator, no block
/// terminated twice), so a lowering bug fails loudly here instead of
/// producing wrong machine code later.
pub struct FunctionBuilder {
    pub func: Function,
    current: BlockId,
}

impl FunctionBuilder {
    pub fn new(name: impl Into<String>, params: &[Type], ret: Option<Type>) -> FunctionBuilder {
        let mut func = Function {
            name: name.into(),
            params: params.to_vec(),
            ret,
            blocks: Vec::new(),
            values: Vec::new(),
            comment: None,
        };
        func.blocks.push(Block {
            name: "entry".into(),
            params: Vec::new(),
            insts: Vec::new(),
            term: None,
        });
        let mut b = FunctionBuilder {
            func,
            current: BlockId(0),
        };
        for &ty in params {
            let v = b.new_value(ty);
            b.func.blocks[0].params.push(v);
        }
        b
    }

    pub fn param(&self, i: usize) -> Value {
        self.func.blocks[0].params[i]
    }

    fn new_value(&mut self, ty: Type) -> Value {
        let v = Value(self.func.values.len() as u32);
        self.func.values.push(ValueInfo { ty, name: None });
        v
    }

    pub fn new_block(&mut self, name: &str) -> BlockId {
        let id = BlockId(self.func.blocks.len() as u32);
        let name = format!("{name}{}", id.0);
        self.func.blocks.push(Block {
            name,
            params: Vec::new(),
            insts: Vec::new(),
            term: None,
        });
        id
    }

    pub fn block_param(&mut self, block: BlockId, ty: Type) -> Value {
        let v = self.new_value(ty);
        self.func.blocks[block.0 as usize].params.push(v);
        v
    }

    pub fn switch_to(&mut self, block: BlockId) {
        self.current = block;
    }

    pub fn current(&self) -> BlockId {
        self.current
    }

    /// The current block already has a terminator (code after it is dead).
    pub fn is_terminated(&self) -> bool {
        self.func.blocks[self.current.0 as usize].term.is_some()
    }

    fn push(&mut self, op: Op, ty: Option<Type>) -> Option<Value> {
        let result = ty.map(|t| self.new_value(t));
        let block = &mut self.func.blocks[self.current.0 as usize];
        assert!(
            block.term.is_none(),
            "instruction added after terminator in {}",
            block.name
        );
        block.insts.push(Inst { result, op });
        result
    }

    fn push_val(&mut self, op: Op, ty: Type) -> Value {
        self.push(op, Some(ty)).unwrap()
    }

    pub fn iconst(&mut self, v: i64) -> Value {
        self.push_val(Op::Iconst(v), Type::I64)
    }

    pub fn fconst(&mut self, v: f64) -> Value {
        self.push_val(Op::Fconst(v), Type::F64)
    }

    pub fn ibin(&mut self, op: IBinOp, a: Value, b: Value) -> Value {
        self.push_val(Op::IBin(op, a, b), Type::I64)
    }

    pub fn fbin(&mut self, op: FBinOp, a: Value, b: Value) -> Value {
        self.push_val(Op::FBin(op, a, b), Type::F64)
    }

    pub fn icmp(&mut self, c: ICond, a: Value, b: Value) -> Value {
        self.push_val(Op::Icmp(c, a, b), Type::I64)
    }

    pub fn fcmp(&mut self, c: FCond, a: Value, b: Value) -> Value {
        self.push_val(Op::Fcmp(c, a, b), Type::I64)
    }

    pub fn sitofp(&mut self, a: Value) -> Value {
        self.push_val(Op::Sitofp(a), Type::F64)
    }

    pub fn bitcast(&mut self, a: Value, to: Type) -> Value {
        self.push_val(Op::Bitcast(a), to)
    }

    pub fn load(&mut self, ty: MemTy, ptr: Value, offset: i32) -> Value {
        self.push_val(Op::Load(ty, ptr, offset), ty.value_type())
    }

    pub fn store(&mut self, ty: MemTy, ptr: Value, offset: i32, value: Value) {
        self.push(Op::Store(ty, ptr, offset, value), None);
    }

    /// A region of `size` bytes in the stack frame.
    pub fn alloca(&mut self, size: u32) -> Value {
        self.push_val(Op::Alloca(size), Type::I64)
    }

    pub fn addr(&mut self, symbol: &str) -> Value {
        self.push_val(Op::Addr(symbol.to_string()), Type::I64)
    }

    pub fn call(&mut self, callee: &str, args: Vec<Value>, ret: Option<Type>) -> Option<Value> {
        self.push(Op::Call(callee.to_string(), args), ret)
    }

    fn terminate(&mut self, t: Terminator) {
        let block = &mut self.func.blocks[self.current.0 as usize];
        assert!(
            block.term.is_none(),
            "block {} terminated twice",
            block.name
        );
        block.term = Some(t);
    }

    pub fn jmp(&mut self, target: BlockId, args: Vec<Value>) {
        self.terminate(Terminator::Jmp(target, args));
    }

    pub fn br(
        &mut self,
        cond: Value,
        then: BlockId,
        then_args: Vec<Value>,
        els: BlockId,
        else_args: Vec<Value>,
    ) {
        self.terminate(Terminator::Br(cond, then, then_args, els, else_args));
    }

    pub fn ret(&mut self, v: Option<Value>) {
        self.terminate(Terminator::Ret(v));
    }

    pub fn unreachable(&mut self) {
        self.terminate(Terminator::Unreachable);
    }

    pub fn finish(self) -> Function {
        self.func
    }
}
