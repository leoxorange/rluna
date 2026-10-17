//! Printing LIR in its textual `.lir` form.

use std::fmt::Write;

use super::*;

pub fn ibin_name(op: IBinOp) -> &'static str {
    match op {
        IBinOp::Add => "iadd",
        IBinOp::Sub => "isub",
        IBinOp::Mul => "imul",
        IBinOp::SDiv => "sdiv",
        IBinOp::SRem => "srem",
        IBinOp::UDiv => "udiv",
        IBinOp::URem => "urem",
        IBinOp::And => "and",
        IBinOp::Or => "or",
        IBinOp::Xor => "xor",
        IBinOp::Shl => "shl",
        IBinOp::Shr => "shr",
        IBinOp::Sar => "sar",
    }
}

pub fn fbin_name(op: FBinOp) -> &'static str {
    match op {
        FBinOp::Add => "fadd",
        FBinOp::Sub => "fsub",
        FBinOp::Mul => "fmul",
        FBinOp::Div => "fdiv",
    }
}

pub fn icond_name(c: ICond) -> &'static str {
    match c {
        ICond::Eq => "eq",
        ICond::Ne => "ne",
        ICond::Slt => "slt",
        ICond::Sle => "sle",
        ICond::Sgt => "sgt",
        ICond::Sge => "sge",
        ICond::Ult => "ult",
        ICond::Ule => "ule",
        ICond::Ugt => "ugt",
        ICond::Uge => "uge",
    }
}

pub fn fcond_name(c: FCond) -> &'static str {
    match c {
        FCond::Eq => "eq",
        FCond::Ne => "ne",
        FCond::Lt => "lt",
        FCond::Le => "le",
        FCond::Gt => "gt",
        FCond::Ge => "ge",
    }
}

pub fn memty_name(t: MemTy) -> &'static str {
    match t {
        MemTy::I64 => "i64",
        MemTy::F64 => "f64",
        MemTy::U8 => "u8",
    }
}

pub fn escape_bytes(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            b'\n' => s.push_str("\\n"),
            b'\t' => s.push_str("\\t"),
            b'\r' => s.push_str("\\r"),
            b'\\' => s.push_str("\\\\"),
            b'"' => s.push_str("\\\""),
            0x20..=0x7e => s.push(b as char),
            _ => write!(s, "\\x{b:02x}").unwrap(),
        }
    }
    s
}

pub fn print_module(m: &Module) -> String {
    let mut out = String::new();
    for e in &m.externs {
        let ps: Vec<String> = e.params.iter().map(|t| t.to_string()).collect();
        write!(out, "extern @{}({})", e.name, ps.join(", ")).unwrap();
        if let Some(r) = e.ret {
            write!(out, " -> {r}").unwrap();
        }
        out.push('\n');
    }
    if !m.externs.is_empty() {
        out.push('\n');
    }
    for d in &m.data {
        let items: Vec<String> = d
            .items
            .iter()
            .map(|i| match i {
                DataItem::I64(v) => format!("i64 {v}"),
                DataItem::Bytes(b) => format!("bytes \"{}\"", escape_bytes(b)),
                DataItem::Zero(n) => format!("zero {n}"),
            })
            .collect();
        writeln!(out, "data @{} = {{ {} }}", d.name, items.join(", ")).unwrap();
    }
    if !m.data.is_empty() {
        out.push('\n');
    }
    for (i, f) in m.funcs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&print_function(f));
    }
    out
}

pub fn print_function(f: &Function) -> String {
    let v = |x: Value| -> String {
        match &f.values[x.0 as usize].name {
            Some(n) => format!("%{n}"),
            None => format!("%{}", x.0),
        }
    };
    let vs = |xs: &[Value]| xs.iter().map(|&x| v(x)).collect::<Vec<_>>().join(", ");
    let target = |b: BlockId, args: &[Value]| {
        let name = &f.blocks[b.0 as usize].name;
        if args.is_empty() {
            name.clone()
        } else {
            format!("{name}({})", vs(args))
        }
    };

    let mut out = String::new();
    if let Some(c) = &f.comment {
        for line in c.lines() {
            writeln!(out, "; {line}").unwrap();
        }
    }
    let entry_params = &f.blocks[0].params;
    let ps: Vec<String> = entry_params
        .iter()
        .map(|&p| format!("{}: {}", v(p), f.value_ty(p)))
        .collect();
    write!(out, "func @{}({})", f.name, ps.join(", ")).unwrap();
    if let Some(r) = f.ret {
        write!(out, " -> {r}").unwrap();
    }
    out.push_str(" {\n");
    for (bi, block) in f.blocks.iter().enumerate() {
        if bi == 0 || block.params.is_empty() {
            writeln!(out, "{}:", block.name).unwrap();
        } else {
            let ps: Vec<String> = block
                .params
                .iter()
                .map(|&p| format!("{}: {}", v(p), f.value_ty(p)))
                .collect();
            writeln!(out, "{}({}):", block.name, ps.join(", ")).unwrap();
        }
        for inst in &block.insts {
            out.push_str("  ");
            if let Some(r) = inst.result {
                write!(out, "{} = ", v(r)).unwrap();
            }
            match &inst.op {
                Op::Iconst(c) => write!(out, "iconst {c}").unwrap(),
                Op::Fconst(c) => write!(out, "fconst {c:?}").unwrap(),
                Op::IBin(op, a, b) => {
                    write!(out, "{} {}, {}", ibin_name(*op), v(*a), v(*b)).unwrap()
                }
                Op::FBin(op, a, b) => {
                    write!(out, "{} {}, {}", fbin_name(*op), v(*a), v(*b)).unwrap()
                }
                Op::Icmp(c, a, b) => {
                    write!(out, "icmp {} {}, {}", icond_name(*c), v(*a), v(*b)).unwrap()
                }
                Op::Fcmp(c, a, b) => {
                    write!(out, "fcmp {} {}, {}", fcond_name(*c), v(*a), v(*b)).unwrap()
                }
                Op::Sitofp(a) => write!(out, "sitofp {}", v(*a)).unwrap(),
                Op::Fptosi(a) => write!(out, "fptosi {}", v(*a)).unwrap(),
                Op::Bitcast(a) => {
                    let to = f.value_ty(inst.result.unwrap());
                    write!(out, "bitcast {} to {to}", v(*a)).unwrap()
                }
                Op::Load(t, p, off) => {
                    write!(out, "load {} {}, {off}", memty_name(*t), v(*p)).unwrap()
                }
                Op::Store(t, p, off, x) => {
                    write!(out, "store {} {}, {off}, {}", memty_name(*t), v(*p), v(*x)).unwrap()
                }
                Op::Alloca(n) => write!(out, "alloca {n}").unwrap(),
                Op::Addr(s) => write!(out, "addr @{s}").unwrap(),
                Op::Call(callee, args) => {
                    let ty = inst
                        .result
                        .map(|r| f.value_ty(r).to_string())
                        .unwrap_or_else(|| "void".into());
                    write!(out, "call {ty} @{callee}({})", vs(args)).unwrap()
                }
            }
            out.push('\n');
        }
        out.push_str("  ");
        match &block.term {
            Some(Terminator::Jmp(b, args)) => write!(out, "jmp {}", target(*b, args)).unwrap(),
            Some(Terminator::Br(c, t, ta, e, ea)) => {
                write!(out, "br {}, {}, {}", v(*c), target(*t, ta), target(*e, ea)).unwrap()
            }
            Some(Terminator::Ret(Some(x))) => write!(out, "ret {}", v(*x)).unwrap(),
            Some(Terminator::Ret(None)) => out.push_str("ret"),
            Some(Terminator::Unreachable) => out.push_str("unreachable"),
            None => out.push_str("<missing terminator>"),
        }
        out.push('\n');
    }
    out.push_str("}\n");
    out
}
