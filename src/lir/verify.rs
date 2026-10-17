//! Structural and type checks for LIR modules.
//!
//! The verifier is a safety net between the stages. A bug in lowering, or a
//! typo in a hand-written `.lir` file such as the runtime, would otherwise
//! become wrong machine code that crashes mysteriously at run time. The
//! verifier turns it into a clear compile-time message naming the
//! function, the value and the rule broken. LLVM and Cranelift do the same.
//!
//! It also lets the backends trust their input: code generation assumes
//! every operand has the right type and every block ends in a terminator,
//! without checking again.

use std::collections::{HashMap, HashSet};

use super::print::print_function;
use super::*;

pub fn verify_module(m: &Module) -> Result<(), String> {
    let mut sigs: HashMap<&str, (&[Type], Option<Type>)> = HashMap::new();
    let mut symbols: HashSet<&str> = HashSet::new();
    for f in &m.funcs {
        if !symbols.insert(&f.name) {
            return Err(format!("symbol @{} is defined more than once", f.name));
        }
        sigs.insert(&f.name, (&f.params, f.ret));
    }
    for e in &m.externs {
        if !symbols.insert(&e.name) {
            return Err(format!("symbol @{} is defined more than once", e.name));
        }
        sigs.insert(&e.name, (&e.params, e.ret));
    }
    for d in &m.data {
        if !symbols.insert(&d.name) {
            return Err(format!("symbol @{} is defined more than once", d.name));
        }
    }
    for f in &m.funcs {
        verify_function(f, &sigs, &symbols)
            .map_err(|e| format!("in function @{}: {e}\n{}", f.name, print_function(f)))?;
    }
    Ok(())
}

fn verify_function(
    f: &Function,
    sigs: &HashMap<&str, (&[Type], Option<Type>)>,
    symbols: &HashSet<&str>,
) -> Result<(), String> {
    let ty = |v: Value| -> Result<Type, String> {
        f.values
            .get(v.0 as usize)
            .map(|i| i.ty)
            .ok_or_else(|| format!("unknown value %{}", v.0))
    };
    let expect = |v: Value, t: Type, what: &str| -> Result<(), String> {
        let actual = ty(v)?;
        if actual != t {
            Err(format!("{what}: expected {t}, found {actual} (%{})", v.0))
        } else {
            Ok(())
        }
    };
    if f.blocks.is_empty() {
        return Err("function has no blocks".into());
    }
    let entry_tys: Vec<Type> = f.blocks[0].params.iter().map(|&p| f.value_ty(p)).collect();
    if entry_tys != f.params {
        return Err("entry block parameters do not match the signature".into());
    }

    // The "single assignment" in SSA: each value may be defined only once,
    // by a block parameter or an instruction result.
    let mut defined = vec![false; f.values.len()];
    for b in &f.blocks {
        for &p in &b.params {
            if std::mem::replace(&mut defined[p.0 as usize], true) {
                return Err(format!("value %{} defined twice", p.0));
            }
        }
        for i in &b.insts {
            if let Some(r) = i.result
                && std::mem::replace(&mut defined[r.0 as usize], true)
            {
                return Err(format!("value %{} defined twice", r.0));
            }
        }
    }

    let check_args = |target: BlockId, args: &[Value]| -> Result<(), String> {
        let tb = f
            .blocks
            .get(target.0 as usize)
            .ok_or("branch to unknown block")?;
        // The entry block's parameters are the function's arguments, which
        // arrive in calling-convention registers. Jumping back to it would
        // need a second way to set them; loops use a separate block.
        if target.0 == 0 {
            return Err("cannot branch to the entry block".into());
        }
        if tb.params.len() != args.len() {
            return Err(format!(
                "block {} takes {} argument(s), got {}",
                tb.name,
                tb.params.len(),
                args.len()
            ));
        }
        for (&p, &a) in tb.params.iter().zip(args) {
            expect(a, f.value_ty(p), &format!("argument to block {}", tb.name))?;
        }
        Ok(())
    };

    for b in &f.blocks {
        for inst in &b.insts {
            let res_ty = inst.result.map(|r| f.value_ty(r));
            let want = |t: Type| -> Result<(), String> {
                if res_ty == Some(t) {
                    Ok(())
                } else {
                    Err(format!("result of {:?} must be {t}", inst.op))
                }
            };
            match &inst.op {
                Op::Iconst(_) | Op::Alloca(_) => want(Type::I64)?,
                Op::Fconst(_) => want(Type::F64)?,
                Op::IBin(_, a, c) | Op::Icmp(_, a, c) => {
                    expect(*a, Type::I64, "integer operand")?;
                    expect(*c, Type::I64, "integer operand")?;
                    want(Type::I64)?;
                }
                Op::FBin(_, a, c) => {
                    expect(*a, Type::F64, "float operand")?;
                    expect(*c, Type::F64, "float operand")?;
                    want(Type::F64)?;
                }
                Op::Fcmp(_, a, c) => {
                    expect(*a, Type::F64, "float operand")?;
                    expect(*c, Type::F64, "float operand")?;
                    want(Type::I64)?;
                }
                Op::Sitofp(a) => {
                    expect(*a, Type::I64, "sitofp operand")?;
                    want(Type::F64)?;
                }
                Op::Fptosi(a) => {
                    expect(*a, Type::F64, "fptosi operand")?;
                    want(Type::I64)?;
                }
                Op::Bitcast(a) => {
                    let from = ty(*a)?;
                    if res_ty.is_none() || res_ty == Some(from) {
                        return Err("bitcast must change the type".into());
                    }
                }
                Op::Load(t, p, _) => {
                    expect(*p, Type::I64, "pointer")?;
                    want(t.value_type())?;
                }
                Op::Store(t, p, _, v) => {
                    expect(*p, Type::I64, "pointer")?;
                    expect(*v, t.value_type(), "stored value")?;
                }
                Op::Addr(s) => {
                    if !symbols.contains(s.as_str()) {
                        return Err(format!("unknown symbol @{s}"));
                    }
                    want(Type::I64)?;
                }
                Op::Call(callee, args) => {
                    let (params, ret) = sigs
                        .get(callee.as_str())
                        .ok_or_else(|| format!("call to unknown function @{callee}"))?;
                    if params.len() != args.len() {
                        return Err(format!(
                            "@{callee} takes {} argument(s), got {}",
                            params.len(),
                            args.len()
                        ));
                    }
                    for (a, p) in args.iter().zip(params.iter()) {
                        expect(*a, *p, &format!("argument to @{callee}"))?;
                    }
                    if res_ty != *ret {
                        return Err(format!(
                            "result type of call to @{callee} does not match its signature"
                        ));
                    }
                }
            }
        }
        match &b.term {
            None => return Err(format!("block {} has no terminator", b.name)),
            Some(Terminator::Jmp(t, args)) => check_args(*t, args)?,
            Some(Terminator::Br(c, t, ta, e, ea)) => {
                expect(*c, Type::I64, "branch condition")?;
                check_args(*t, ta)?;
                check_args(*e, ea)?;
            }
            Some(Terminator::Ret(v)) => match (v, f.ret) {
                (Some(v), Some(t)) => expect(*v, t, "return value")?,
                (None, None) => {}
                _ => {
                    return Err(format!(
                        "return in block {} does not match the function's return type",
                        b.name
                    ));
                }
            },
            Some(Terminator::Unreachable) => {}
        }
    }
    Ok(())
}
