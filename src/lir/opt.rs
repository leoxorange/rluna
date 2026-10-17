//! LIR optimization passes.

use std::collections::HashSet;

use super::*;

pub fn optimize_module(m: &mut Module) {
    for f in &mut m.funcs {
        dead_code_elimination(f);
    }
}

fn operands(op: &Op) -> Vec<Value> {
    match op {
        Op::Iconst(_) | Op::Fconst(_) | Op::Alloca(_) | Op::Addr(_) => vec![],
        Op::IBin(_, a, b) | Op::FBin(_, a, b) | Op::Icmp(_, a, b) | Op::Fcmp(_, a, b) => {
            vec![*a, *b]
        }
        Op::Sitofp(a) | Op::Fptosi(a) | Op::Bitcast(a) | Op::Load(_, a, _) => vec![*a],
        Op::Store(_, p, _, v) => vec![*p, *v],
        Op::Call(_, args) => args.clone(),
    }
}

fn has_side_effects(op: &Op) -> bool {
    matches!(op, Op::Store(..) | Op::Call(..))
}

/// Remove instructions without side effects whose results are never used.
pub fn dead_code_elimination(f: &mut Function) {
    loop {
        let mut used: HashSet<Value> = HashSet::new();
        for b in &f.blocks {
            for i in &b.insts {
                used.extend(operands(&i.op));
            }
            match &b.term {
                Some(Terminator::Jmp(_, args)) => used.extend(args),
                Some(Terminator::Br(c, _, a, _, e)) => {
                    used.insert(*c);
                    used.extend(a);
                    used.extend(e);
                }
                Some(Terminator::Ret(Some(v))) => {
                    used.insert(*v);
                }
                _ => {}
            }
        }
        let mut removed = false;
        for b in &mut f.blocks {
            b.insts.retain(|i| {
                let dead = !has_side_effects(&i.op) && i.result.is_some_and(|r| !used.contains(&r));
                removed |= dead;
                !dead
            });
        }
        if !removed {
            break;
        }
    }
}
