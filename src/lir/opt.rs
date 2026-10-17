//! LIR optimization passes.
//!
//! An optimization pass rewrites the IR into an equivalent but cheaper form.
//! Passes run on LIR rather than on machine code because the IR is simple
//! and target-independent: one pass here speeds up every backend.
//!
//! Lowering is written to be simple, not efficient, and leaves leftovers
//! (constants it ended up not using, loads whose result is ignored).
//! Cleaning up afterwards is the usual compiler strategy: it is much easier
//! than making every lowering rule produce perfect code.

use std::collections::{HashMap, HashSet};

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

/// Remove functions, data and imports that cannot be reached from `root`.
///
/// Every program is compiled together with the whole runtime, but `hello`
/// never prints a float, so `rt.print_float` and everything only it uses
/// would be dead weight in the executable. This pass walks the "uses" graph
/// from the entry point: a function uses what it calls (`call @f`) and what
/// it takes the address of (`addr @s`). Whatever the walk never reaches is
/// dropped. Linkers do the same at the end of a build (`ld -dead_strip`,
/// `--gc-sections`); bundlers for JavaScript call it "tree shaking".
///
/// It must run on the complete module (program + runtime): only then is
/// every possible caller known.
pub fn remove_unreachable(m: &mut Module, root: &str) {
    let funcs: HashMap<&str, &Function> = m.funcs.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut reached: HashSet<String> = HashSet::from([root.to_string()]);
    let mut work = vec![root];
    while let Some(name) = work.pop() {
        let Some(f) = funcs.get(name) else {
            continue; // data or an import: nothing further to follow
        };
        for inst in f.blocks.iter().flat_map(|b| &b.insts) {
            let used = match &inst.op {
                Op::Call(callee, _) => callee,
                Op::Addr(symbol) => symbol,
                _ => continue,
            };
            if reached.insert(used.clone()) {
                work.push(used);
            }
        }
    }
    m.funcs.retain(|f| reached.contains(&f.name));
    m.data.retain(|d| reached.contains(&d.name));
    m.externs.retain(|e| reached.contains(&e.name));
}

/// Does the instruction do something besides computing its result? Such an
/// instruction must stay even if its result is unused: deleting a
/// `call @rt.print_int` because it returns nothing would delete the output.
/// Calls are conservatively assumed to have effects, since the pass does
/// not look inside the callee.
fn has_side_effects(op: &Op) -> bool {
    matches!(op, Op::Store(..) | Op::Call(..))
}

/// Remove instructions without side effects whose results are never used.
///
/// SSA makes this easy: a value has one definition, so "never used" just
/// means "never appears as an operand". Removing an instruction can make
/// *its* operands unused (in `%a = iconst 1; %b = iadd %a, %a` with `%b`
/// unused, `%a` becomes dead only after `%b` is gone), so the pass repeats
/// until nothing changes: another fixpoint.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::parse::parse_module;

    #[test]
    fn removes_what_the_root_cannot_reach() {
        let mut m = parse_module(
            r#"extern @lib.used(i64)
extern @lib.unused(i64)
data @kept = { i64 1 }
data @dropped = { i64 2 }

func @root() {
entry:
  call void @a()
  ret
}

func @a() {
entry:
  %p = addr @kept
  call void @lib.used(%p)
  ret
}

func @b() {
entry:
  %p = addr @dropped
  call void @lib.unused(%p)
  call void @a()
  ret
}
"#,
        )
        .unwrap();
        remove_unreachable(&mut m, "root");
        let names = |v: Vec<&String>| v.into_iter().cloned().collect::<Vec<_>>();
        assert_eq!(
            names(m.funcs.iter().map(|f| &f.name).collect()),
            ["root", "a"]
        );
        assert_eq!(names(m.data.iter().map(|d| &d.name).collect()), ["kept"]);
        assert_eq!(
            names(m.externs.iter().map(|e| &e.name).collect()),
            ["lib.used"]
        );
    }
}
