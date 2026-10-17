//! Parsing the textual `.lir` form back into a `Module`.
//!
//! Grammar (whitespace and newlines are insignificant, `;` starts a comment):
//!
//! ```text
//! module   := (extern | data | func)*
//! extern   := "extern" @name "(" types ")" ["->" type]
//! data     := "data" @name "=" "{" item ("," item)* "}"
//! item     := "i64" int | "bytes" string | "zero" int
//! func     := "func" @name "(" [%v ":" type ("," ...)*] ")" ["->" type] "{" block+ "}"
//! block    := label ["(" %v ":" type, ... ")"] ":" inst* term
//! inst     := [%v "="] opcode operands
//! term     := "jmp" target | "br" %c "," target "," target | "ret" [%v] | "unreachable"
//! target   := label ["(" %v, ... ")"]
//! ```
//!
//! Why can LIR be read back in? Three reasons:
//!
//! 1. The runtime library (`runtime/*.lir`) is written by hand in LIR. Low-level
//!    code such as a memory allocator or a float printer needs pointers and
//!    raw memory, which Luna does not have.
//! 2. `--emit=ir` output can be edited and recompiled, which is handy for
//!    experimenting with the backends without touching the frontend.
//! 3. The tests print every program's IR, parse it back and run it again, so
//!    the printer and parser are checked against each other on real code.
//!
//! This is a second, much simpler instance of the lexer + recursive descent
//! parser design used for Luna itself.

use std::collections::HashMap;

use super::*;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Global(String),
    Local(String),
    Word(String),
    Str(Vec<u8>),
    Punct(char),
    Arrow,
    Eof,
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'$')
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let mut out = Vec::new();
    let perr = |line, m: &str| ParseError {
        line,
        message: m.to_string(),
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b';' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'@' | b'%' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && is_name_char(b[i]) {
                    i += 1;
                }
                if start == i {
                    return Err(perr(line, "expected a name after `@`/`%`"));
                }
                let name = src[start..i].to_string();
                out.push((
                    if c == b'@' {
                        Tok::Global(name)
                    } else {
                        Tok::Local(name)
                    },
                    line,
                ));
            }
            b'"' => {
                i += 1;
                let mut s = Vec::new();
                loop {
                    if i >= b.len() || b[i] == b'\n' {
                        return Err(perr(line, "unterminated string"));
                    }
                    match b[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            let e = *b.get(i + 1).unwrap_or(&0);
                            i += 2;
                            s.push(match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'r' => b'\r',
                                b'0' => 0,
                                b'\\' => b'\\',
                                b'"' => b'"',
                                b'x' => {
                                    let hex = src
                                        .get(i..i + 2)
                                        .ok_or_else(|| perr(line, "bad \\x escape"))?;
                                    i += 2;
                                    u8::from_str_radix(hex, 16)
                                        .map_err(|_| perr(line, "bad \\x escape"))?
                                }
                                _ => return Err(perr(line, "unknown escape")),
                            });
                        }
                        x => {
                            s.push(x);
                            i += 1;
                        }
                    }
                }
                out.push((Tok::Str(s), line));
            }
            b'-' if b.get(i + 1) == Some(&b'>') => {
                out.push((Tok::Arrow, line));
                i += 2;
            }
            b'(' | b')' | b'{' | b'}' | b',' | b':' | b'=' => {
                out.push((Tok::Punct(c as char), line));
                i += 1;
            }
            _ if is_name_char(c) || c == b'-' || c == b'+' => {
                let start = i;
                i += 1;
                while i < b.len()
                    && (is_name_char(b[i])
                        || ((b[i] == b'-' || b[i] == b'+') && matches!(b[i - 1], b'e' | b'E')))
                {
                    i += 1;
                }
                out.push((Tok::Word(src[start..i].to_string()), line));
            }
            _ => return Err(perr(line, &format!("unexpected character `{}`", c as char))),
        }
    }
    out.push((Tok::Eof, line));
    Ok(out)
}

pub fn parse_module(src: &str) -> Result<Module, ParseError> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let mut m = Module::default();
    while p.peek() != &Tok::Eof {
        let kw = p.word()?;
        match kw.as_str() {
            "extern" => m.externs.push(p.extern_decl()?),
            "data" => m.data.push(p.data()?),
            "func" => m.funcs.push(p.func()?),
            _ => {
                return Err(p.error(&format!(
                    "expected `func`, `extern` or `data`, found `{kw}`"
                )));
            }
        }
    }
    Ok(m)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

/// Per-function parsing state.
///
/// Text names (`%sum`, `loop`) are mapped to numbers as they are seen. A
/// name can be *used* before it is *defined*: a jump to a block further
/// down, or a value defined in a block that appears later in the text.
/// `value`/`block` therefore create an entry on first sight, and `defined`
/// remembers which ones have really been defined, so that duplicate
/// definitions are rejected.
struct FnState {
    values: Vec<ValueInfo>,
    names: HashMap<String, Value>,
    defined: Vec<bool>,
    blocks: Vec<Block>,
    block_names: HashMap<String, BlockId>,
    block_defined: Vec<bool>,
}

impl FnState {
    fn value(&mut self, name: &str) -> Value {
        if let Some(&v) = self.names.get(name) {
            return v;
        }
        let v = Value(self.values.len() as u32);
        self.values.push(ValueInfo {
            ty: Type::I64,
            name: Some(name.to_string()),
        });
        self.defined.push(false);
        self.names.insert(name.to_string(), v);
        v
    }

    fn define(&mut self, name: &str, ty: Type) -> Result<Value, String> {
        let v = self.value(name);
        if self.defined[v.0 as usize] {
            return Err(format!("value %{name} is defined more than once"));
        }
        self.defined[v.0 as usize] = true;
        self.values[v.0 as usize].ty = ty;
        Ok(v)
    }

    fn block(&mut self, name: &str) -> BlockId {
        if let Some(&b) = self.block_names.get(name) {
            return b;
        }
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(Block {
            name: name.to_string(),
            params: Vec::new(),
            insts: Vec::new(),
            term: None,
        });
        self.block_defined.push(false);
        self.block_names.insert(name.to_string(), id);
        id
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].0
    }

    fn line(&self) -> usize {
        self.toks[self.pos].1
    }

    fn error(&self, m: &str) -> ParseError {
        ParseError {
            line: self.line(),
            message: m.to_string(),
        }
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].0.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn punct(&mut self, c: char) -> Result<(), ParseError> {
        if self.peek() == &Tok::Punct(c) {
            self.next();
            Ok(())
        } else {
            Err(self.error(&format!("expected `{c}`, found {:?}", self.peek())))
        }
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.peek() == &Tok::Punct(c) {
            self.next();
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> Result<String, ParseError> {
        match self.next() {
            Tok::Word(w) => Ok(w),
            t => {
                self.pos -= 1;
                Err(self.error(&format!("expected a keyword, found {t:?}")))
            }
        }
    }

    fn global(&mut self) -> Result<String, ParseError> {
        match self.next() {
            Tok::Global(g) => Ok(g),
            t => {
                self.pos -= 1;
                Err(self.error(&format!("expected `@name`, found {t:?}")))
            }
        }
    }

    fn local(&mut self) -> Result<String, ParseError> {
        match self.next() {
            Tok::Local(l) => Ok(l),
            t => {
                self.pos -= 1;
                Err(self.error(&format!("expected `%value`, found {t:?}")))
            }
        }
    }

    fn int(&mut self) -> Result<i64, ParseError> {
        let w = self.word()?;
        let parsed = if let Some(hex) = w.strip_prefix("0x") {
            u64::from_str_radix(hex, 16).map(|v| v as i64).ok()
        } else {
            w.parse::<i64>().ok()
        };
        parsed.ok_or_else(|| self.error(&format!("expected an integer, found `{w}`")))
    }

    fn ty(&mut self) -> Result<Type, ParseError> {
        match self.word()?.as_str() {
            "i64" => Ok(Type::I64),
            "f64" => Ok(Type::F64),
            w => Err(self.error(&format!("expected a type, found `{w}`"))),
        }
    }

    fn ret_type(&mut self) -> Result<Option<Type>, ParseError> {
        if self.peek() == &Tok::Arrow {
            self.next();
            Ok(Some(self.ty()?))
        } else {
            Ok(None)
        }
    }

    fn extern_decl(&mut self) -> Result<Extern, ParseError> {
        let name = self.global()?;
        self.punct('(')?;
        let mut params = Vec::new();
        if !self.eat_punct(')') {
            loop {
                params.push(self.ty()?);
                if self.eat_punct(')') {
                    break;
                }
                self.punct(',')?;
            }
        }
        let ret = self.ret_type()?;
        Ok(Extern { name, params, ret })
    }

    fn data(&mut self) -> Result<Data, ParseError> {
        let name = self.global()?;
        self.punct('=')?;
        self.punct('{')?;
        let mut items = Vec::new();
        loop {
            let kind = self.word()?;
            items.push(match kind.as_str() {
                "i64" => DataItem::I64(self.int()?),
                "zero" => DataItem::Zero(self.int()? as usize),
                "bytes" => match self.next() {
                    Tok::Str(s) => DataItem::Bytes(s),
                    _ => return Err(self.error("expected a string after `bytes`")),
                },
                _ => return Err(self.error(&format!("unknown data item `{kind}`"))),
            });
            if self.eat_punct('}') {
                break;
            }
            self.punct(',')?;
        }
        Ok(Data { name, items })
    }

    fn func(&mut self) -> Result<Function, ParseError> {
        let name = self.global()?;
        let mut st = FnState {
            values: Vec::new(),
            names: HashMap::new(),
            defined: Vec::new(),
            blocks: Vec::new(),
            block_names: HashMap::new(),
            block_defined: Vec::new(),
        };
        self.punct('(')?;
        let mut params = Vec::new();
        let mut param_vals = Vec::new();
        if !self.eat_punct(')') {
            loop {
                let n = self.local()?;
                self.punct(':')?;
                let t = self.ty()?;
                param_vals.push(st.define(&n, t).map_err(|e| self.error(&e))?);
                params.push(t);
                if self.eat_punct(')') {
                    break;
                }
                self.punct(',')?;
            }
        }
        let ret = self.ret_type()?;
        self.punct('{')?;

        let mut first = true;
        let mut order = Vec::new();
        while !self.eat_punct('}') {
            let label = self.word()?;
            let b = st.block(&label);
            if st.block_defined[b.0 as usize] {
                return Err(self.error(&format!("block `{label}` is defined twice")));
            }
            st.block_defined[b.0 as usize] = true;
            order.push(b);
            if first {
                st.blocks[b.0 as usize].params = param_vals.clone();
            }
            if self.eat_punct('(') {
                if first {
                    return Err(self.error("the entry block takes the function's parameters and cannot declare its own"));
                }
                loop {
                    let n = self.local()?;
                    self.punct(':')?;
                    let t = self.ty()?;
                    let v = st.define(&n, t).map_err(|e| self.error(&e))?;
                    st.blocks[b.0 as usize].params.push(v);
                    if self.eat_punct(')') {
                        break;
                    }
                    self.punct(',')?;
                }
            }
            self.punct(':')?;
            first = false;
            self.block_body(&mut st, b)?;
        }

        for (i, d) in st.defined.iter().enumerate() {
            if !d {
                let n = st.values[i].name.clone().unwrap_or_default();
                return Err(
                    self.error(&format!("in @{name}: value %{n} is used but never defined"))
                );
            }
        }
        for (i, d) in st.block_defined.iter().enumerate() {
            if !d {
                return Err(self.error(&format!(
                    "in @{name}: block `{}` is used but never defined",
                    st.blocks[i].name
                )));
            }
        }
        if order.is_empty() {
            return Err(self.error(&format!("function @{name} has no blocks")));
        }
        // Lay blocks out in textual order and renumber references.
        let mut remap = vec![BlockId(0); st.blocks.len()];
        for (new, old) in order.iter().enumerate() {
            remap[old.0 as usize] = BlockId(new as u32);
        }
        let mut blocks: Vec<Option<Block>> = st.blocks.into_iter().map(Some).collect();
        let mut laid_out = Vec::new();
        for old in &order {
            let mut b = blocks[old.0 as usize].take().unwrap();
            if let Some(t) = &mut b.term {
                match t {
                    Terminator::Jmp(x, _) => *x = remap[x.0 as usize],
                    Terminator::Br(_, x, _, y, _) => {
                        *x = remap[x.0 as usize];
                        *y = remap[y.0 as usize];
                    }
                    _ => {}
                }
            }
            laid_out.push(b);
        }
        Ok(Function {
            name,
            params,
            ret,
            blocks: laid_out,
            values: st.values,
            comment: None,
        })
    }

    fn operand(&mut self, st: &mut FnState) -> Result<Value, ParseError> {
        let n = self.local()?;
        Ok(st.value(&n))
    }

    fn operands2(&mut self, st: &mut FnState) -> Result<(Value, Value), ParseError> {
        let a = self.operand(st)?;
        self.punct(',')?;
        let b = self.operand(st)?;
        Ok((a, b))
    }

    fn target(&mut self, st: &mut FnState) -> Result<(BlockId, Vec<Value>), ParseError> {
        let label = self.word()?;
        let b = st.block(&label);
        let mut args = Vec::new();
        if self.eat_punct('(') && !self.eat_punct(')') {
            loop {
                args.push(self.operand(st)?);
                if self.eat_punct(')') {
                    break;
                }
                self.punct(',')?;
            }
        }
        Ok((b, args))
    }

    fn memty(&mut self) -> Result<MemTy, ParseError> {
        match self.word()?.as_str() {
            "i64" => Ok(MemTy::I64),
            "f64" => Ok(MemTy::F64),
            "u8" => Ok(MemTy::U8),
            w => Err(self.error(&format!("expected i64, f64 or u8, found `{w}`"))),
        }
    }

    fn block_body(&mut self, st: &mut FnState, b: BlockId) -> Result<(), ParseError> {
        loop {
            let result = if let Tok::Local(n) = self.peek().clone() {
                self.next();
                self.punct('=')?;
                Some(n)
            } else {
                None
            };
            let opcode = self.word()?;
            let term = match opcode.as_str() {
                "jmp" => {
                    let (t, args) = self.target(st)?;
                    Some(Terminator::Jmp(t, args))
                }
                "br" => {
                    let c = self.operand(st)?;
                    self.punct(',')?;
                    let (t, ta) = self.target(st)?;
                    self.punct(',')?;
                    let (e, ea) = self.target(st)?;
                    Some(Terminator::Br(c, t, ta, e, ea))
                }
                "ret" => {
                    if let Tok::Local(_) = self.peek() {
                        let v = self.operand(st)?;
                        Some(Terminator::Ret(Some(v)))
                    } else {
                        Some(Terminator::Ret(None))
                    }
                }
                "unreachable" => Some(Terminator::Unreachable),
                _ => None,
            };
            if let Some(t) = term {
                if result.is_some() {
                    return Err(self.error("a terminator does not produce a value"));
                }
                st.blocks[b.0 as usize].term = Some(t);
                return Ok(());
            }

            let (op, ty): (Op, Option<Type>) = match opcode.as_str() {
                "iconst" => (Op::Iconst(self.int()?), Some(Type::I64)),
                "fconst" => {
                    let w = self.word()?;
                    let v: f64 = w
                        .parse()
                        .map_err(|_| self.error(&format!("bad float `{w}`")))?;
                    (Op::Fconst(v), Some(Type::F64))
                }
                "iadd" | "isub" | "imul" | "sdiv" | "srem" | "udiv" | "urem" | "and" | "or"
                | "xor" | "shl" | "shr" | "sar" => {
                    let op = match opcode.as_str() {
                        "iadd" => IBinOp::Add,
                        "isub" => IBinOp::Sub,
                        "imul" => IBinOp::Mul,
                        "sdiv" => IBinOp::SDiv,
                        "srem" => IBinOp::SRem,
                        "udiv" => IBinOp::UDiv,
                        "urem" => IBinOp::URem,
                        "and" => IBinOp::And,
                        "or" => IBinOp::Or,
                        "xor" => IBinOp::Xor,
                        "shl" => IBinOp::Shl,
                        "shr" => IBinOp::Shr,
                        _ => IBinOp::Sar,
                    };
                    let (a, c) = self.operands2(st)?;
                    (Op::IBin(op, a, c), Some(Type::I64))
                }
                "fadd" | "fsub" | "fmul" | "fdiv" => {
                    let op = match opcode.as_str() {
                        "fadd" => FBinOp::Add,
                        "fsub" => FBinOp::Sub,
                        "fmul" => FBinOp::Mul,
                        _ => FBinOp::Div,
                    };
                    let (a, c) = self.operands2(st)?;
                    (Op::FBin(op, a, c), Some(Type::F64))
                }
                "icmp" => {
                    let c = match self.word()?.as_str() {
                        "eq" => ICond::Eq,
                        "ne" => ICond::Ne,
                        "slt" => ICond::Slt,
                        "sle" => ICond::Sle,
                        "sgt" => ICond::Sgt,
                        "sge" => ICond::Sge,
                        "ult" => ICond::Ult,
                        "ule" => ICond::Ule,
                        "ugt" => ICond::Ugt,
                        "uge" => ICond::Uge,
                        w => return Err(self.error(&format!("unknown icmp condition `{w}`"))),
                    };
                    let (a, x) = self.operands2(st)?;
                    (Op::Icmp(c, a, x), Some(Type::I64))
                }
                "fcmp" => {
                    let c = match self.word()?.as_str() {
                        "eq" => FCond::Eq,
                        "ne" => FCond::Ne,
                        "lt" => FCond::Lt,
                        "le" => FCond::Le,
                        "gt" => FCond::Gt,
                        "ge" => FCond::Ge,
                        w => return Err(self.error(&format!("unknown fcmp condition `{w}`"))),
                    };
                    let (a, x) = self.operands2(st)?;
                    (Op::Fcmp(c, a, x), Some(Type::I64))
                }
                "sitofp" => (Op::Sitofp(self.operand(st)?), Some(Type::F64)),
                "fptosi" => (Op::Fptosi(self.operand(st)?), Some(Type::I64)),
                "bitcast" => {
                    let a = self.operand(st)?;
                    if self.word()? != "to" {
                        return Err(self.error("expected `to` in bitcast"));
                    }
                    (Op::Bitcast(a), Some(self.ty()?))
                }
                "load" => {
                    let t = self.memty()?;
                    let p = self.operand(st)?;
                    self.punct(',')?;
                    let off = self.int()? as i32;
                    (Op::Load(t, p, off), Some(t.value_type()))
                }
                "store" => {
                    let t = self.memty()?;
                    let p = self.operand(st)?;
                    self.punct(',')?;
                    let off = self.int()? as i32;
                    self.punct(',')?;
                    let v = self.operand(st)?;
                    (Op::Store(t, p, off, v), None)
                }
                "alloca" => (Op::Alloca(self.int()? as u32), Some(Type::I64)),
                "addr" => (Op::Addr(self.global()?), Some(Type::I64)),
                "call" => {
                    let ty = match self.word()?.as_str() {
                        "i64" => Some(Type::I64),
                        "f64" => Some(Type::F64),
                        "void" => None,
                        w => {
                            return Err(self
                                .error(&format!("expected the call's result type, found `{w}`")));
                        }
                    };
                    let callee = self.global()?;
                    self.punct('(')?;
                    let mut args = Vec::new();
                    if !self.eat_punct(')') {
                        loop {
                            args.push(self.operand(st)?);
                            if self.eat_punct(')') {
                                break;
                            }
                            self.punct(',')?;
                        }
                    }
                    (Op::Call(callee, args), ty)
                }
                _ => return Err(self.error(&format!("unknown instruction `{opcode}`"))),
            };
            let result_val = match (&result, ty) {
                (Some(n), Some(t)) => Some(st.define(n, t).map_err(|e| self.error(&e))?),
                (None, None) => None,
                (Some(_), None) => {
                    return Err(self.error(&format!("`{opcode}` does not produce a value")));
                }
                (None, Some(_)) if opcode == "call" => {
                    return Err(self.error("a call with a result type must assign its result"));
                }
                (None, Some(_)) => {
                    return Err(self.error(&format!("the result of `{opcode}` must be assigned")));
                }
            };
            st.blocks[b.0 as usize].insts.push(Inst {
                result: result_val,
                op,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::print::print_module;

    #[test]
    fn roundtrip() {
        let src = r#"extern @kernel32.ExitProcess(i64)

data @msg = { i64 3, bytes "hi\n" }

func @f(%a: i64, %x: f64) -> i64 {
entry:
  %one = iconst 1
  %y = fadd %x, %x
  %c = fcmp lt %x, %y
  br %c, loop(%a), done
loop(%i: i64):
  %j = isub %i, %one
  %z = icmp sgt %j, %one
  br %z, loop(%j), done
done:
  %p = addr @msg
  %n = load i64 %p, 0
  call void @kernel32.ExitProcess(%n)
  ret %n
}
"#;
        let m = parse_module(src).unwrap();
        let printed = print_module(&m);
        assert_eq!(printed, src);
        let again = parse_module(&printed).unwrap();
        assert_eq!(print_module(&again), printed);
    }
}
