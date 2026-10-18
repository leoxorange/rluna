//! Functions generated per type: freeing, copying, printing and comparing
//! values whose layout depends on their type.
//!
//! Freeing a string is one call to `rt.free`. Freeing an array of strings
//! must also free every string in it, and freeing an array of arrays of
//! strings must do that for every inner array. The runtime cannot do this on
//! its own, because an array does not record what its elements are: only the
//! compiler knows the types. So for every type that needs it, the compiler
//! writes a small LIR function (`drop.[string]`, `print.[[int]]`, ...) once,
//! and the program calls it. This is what C++ templates and Rust's
//! monomorphized `Drop` and `Clone` implementations do too.
//!
//! Layouts (see `runtime/core.lir`):
//!
//! ```text
//!   string  { len, bytes... }
//!   array   { len, elements... }   8 bytes per element, as `lower::repr`
//!   box     { tag, payload }        a value of a union type
//! ```
//!
//! Every object starts after an allocator header, so freeing a box of the
//! shared void, or a string literal, is a harmless no-op.

use std::collections::HashMap;

use crate::lir::{BlockId, FunctionBuilder, IBinOp, ICond, MemTy, Module, Type, Value};
use crate::lower::repr;
use crate::types::{Kind, Ty};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Helper {
    Drop,
    Clone,
    Print { quoted: bool },
    Eq,
}

#[derive(Default)]
pub struct TypeFns {
    funcs: Vec<crate::lir::Function>,
    names: HashMap<(Helper, Ty), String>,
}

/// Load element `i` of an array of `elem`.
pub fn load_elem(b: &mut FunctionBuilder, array: Value, i: Value, elem: Ty) -> Value {
    let p = elem_addr(b, array, i);
    b.load(mem_ty(elem), p, 8)
}

/// Store `v` as element `i` of an array of `elem`.
pub fn store_elem(b: &mut FunctionBuilder, array: Value, i: Value, elem: Ty, v: Value) {
    let p = elem_addr(b, array, i);
    b.store(mem_ty(elem), p, 8, v);
}

/// Address of element `i`, minus the 8 bytes of the length field.
fn elem_addr(b: &mut FunctionBuilder, array: Value, i: Value) -> Value {
    let three = b.iconst(3);
    let offset = b.ibin(IBinOp::Shl, i, three);
    b.ibin(IBinOp::Add, array, offset)
}

fn mem_ty(t: Ty) -> MemTy {
    match repr(t) {
        Type::F64 => MemTy::F64,
        Type::I64 => MemTy::I64,
    }
}

/// `for i in 0..len(array)`: calls `body` in the loop body with `i`, then
/// continues after the loop.
fn for_each(
    b: &mut FunctionBuilder,
    array: Value,
    mut body: impl FnMut(&mut FunctionBuilder, Value),
) {
    let len = b.load(MemTy::I64, array, 0);
    let zero = b.iconst(0);
    let header = b.new_block("each");
    let i = b.block_param(header, Type::I64);
    let inner = b.new_block("each_body");
    let done = b.new_block("each_end");
    b.jmp(header, vec![zero]);
    b.switch_to(header);
    let more = b.icmp(ICond::Slt, i, len);
    b.br(more, inner, vec![], done, vec![]);
    b.switch_to(inner);
    body(b, i);
    let one = b.iconst(1);
    let next = b.ibin(IBinOp::Add, i, one);
    b.jmp(header, vec![next]);
    b.switch_to(done);
}

/// For a value of a union type: a block per kind, entered when the box's
/// tag is that kind. Returns (kind, block) pairs and the block after them;
/// each case block must jump to it (or return).
fn switch_tag(b: &mut FunctionBuilder, boxed: Value, ty: Ty) -> (Vec<(Kind, BlockId)>, BlockId) {
    let tag = b.load(MemTy::I64, boxed, 0);
    let end = b.new_block("tag_end");
    let mut cases = Vec::new();
    for kind in ty.kinds() {
        let case = b.new_block(&format!("tag_{}", kind.name()));
        let next = b.new_block("tag_next");
        let k = b.iconst(kind.tag());
        let is = b.icmp(ICond::Eq, tag, k);
        b.br(is, case, vec![], next, vec![]);
        b.switch_to(next);
        cases.push((kind, case));
    }
    b.jmp(end, vec![]);
    (cases, end)
}

fn payload(b: &mut FunctionBuilder, boxed: Value, piece: Ty) -> Value {
    b.load(mem_ty(piece), boxed, 8)
}

impl TypeFns {
    /// Add the generated functions to the module.
    pub fn finish(self, module: &mut Module) {
        module.funcs.extend(self.funcs);
    }

    /// The name of the helper for `ty`, generating it on first use.
    fn helper(
        &mut self,
        helper: Helper,
        ty: Ty,
        params: &[Type],
        ret: Option<Type>,
        body: impl FnOnce(&mut Self, &mut FunctionBuilder),
    ) -> String {
        if let Some(name) = self.names.get(&(helper, ty)) {
            return name.clone();
        }
        let prefix = match helper {
            Helper::Drop => "drop",
            Helper::Clone => "clone",
            Helper::Print { quoted: false } => "print",
            Helper::Print { quoted: true } => "print_quoted",
            Helper::Eq => "eq",
        };
        let name = format!("{prefix}.{}", self.names.len());
        // Registered before the body is generated, so a type may refer to
        // itself while its helper is being written.
        self.names.insert((helper, ty), name.clone());
        let mut b = FunctionBuilder::new(name.clone(), params, ret);
        b.func.comment = Some(format!("{prefix} a value of type {ty}"));
        body(self, &mut b);
        self.funcs.push(b.finish());
        name
    }

    /// Free `v` of type `ty`, and everything it owns.
    pub fn drop(&mut self, b: &mut FunctionBuilder, v: Value, ty: Ty) {
        if !ty.needs_drop() {
            return;
        }
        if ty == Ty::STR {
            b.call("rt.free", vec![v], None);
            return;
        }
        let name = self.helper(Helper::Drop, ty, &[Type::I64], None, |fns, f| {
            let v = f.param(0);
            if ty.is_union() {
                let (cases, end) = switch_tag(f, v, ty);
                for (kind, case) in cases {
                    f.switch_to(case);
                    let piece = ty.only(kind);
                    if piece.needs_drop() {
                        let p = payload(f, v, piece);
                        fns.drop(f, p, piece);
                    }
                    f.jmp(end, vec![]);
                }
                f.switch_to(end);
            } else if let Some(elem) = ty.elem().filter(|e| e.needs_drop()) {
                for_each(f, v, |f, i| {
                    let e = load_elem(f, v, i, elem);
                    fns.drop(f, e, elem);
                });
            }
            f.call("rt.free", vec![v], None);
            f.ret(None);
        });
        b.call(&name, vec![v], None);
    }

    /// A copy of `v` of type `ty` that the caller owns. Numbers are values
    /// already; strings, arrays and boxes are copied, with their contents.
    pub fn clone(&mut self, b: &mut FunctionBuilder, v: Value, ty: Ty) -> Value {
        if !ty.needs_drop() {
            return v;
        }
        if ty == Ty::STR {
            return b.call("rt.str_copy", vec![v], Some(Type::I64)).unwrap();
        }
        let name = self.helper(
            Helper::Clone,
            ty,
            &[Type::I64],
            Some(Type::I64),
            |fns, f| {
                let v = f.param(0);
                if ty.is_union() {
                    let (cases, end) = switch_tag(f, v, ty);
                    for (kind, case) in cases {
                        f.switch_to(case);
                        let piece = ty.only(kind);
                        if kind == Kind::Void {
                            f.ret(Some(v)); // the one shared void
                            continue;
                        }
                        let p = f.load(MemTy::I64, v, 8);
                        let p = if piece.needs_drop() {
                            fns.clone(f, p, piece)
                        } else {
                            p
                        };
                        let tag = f.iconst(kind.tag());
                        let copy = f.call("rt.box", vec![tag, p], Some(Type::I64)).unwrap();
                        f.ret(Some(copy));
                    }
                    f.switch_to(end);
                    f.unreachable();
                } else {
                    let elem = ty.elem().unwrap_or(Ty::NEVER);
                    let len = f.load(MemTy::I64, v, 0);
                    let copy = f.call("rt.array_new", vec![len], Some(Type::I64)).unwrap();
                    for_each(f, v, |f, i| {
                        let e = load_elem(f, v, i, elem);
                        let e = fns.clone(f, e, elem);
                        store_elem(f, copy, i, elem, e);
                    });
                    f.ret(Some(copy));
                }
            },
        );
        b.call(&name, vec![v], Some(Type::I64)).unwrap()
    }

    /// Print `v` of type `ty`. Inside arrays (`quoted`), strings are shown
    /// in quotes so that `["a", "b"]` reads as an array of two strings.
    pub fn print(&mut self, b: &mut FunctionBuilder, v: Value, ty: Ty, quoted: bool) {
        match ty {
            Ty::INT => {
                b.call("rt.print_int", vec![v], None);
            }
            Ty::FLOAT => {
                b.call("rt.print_float", vec![v], None);
            }
            Ty::BOOL => {
                b.call("rt.print_bool", vec![v], None);
            }
            Ty::VOID => {
                b.call("rt.print_void", vec![], None);
            }
            Ty::STR if quoted => {
                b.call("rt.print_quoted", vec![v], None);
            }
            Ty::STR => {
                b.call("rt.print_str", vec![v], None);
            }
            _ if ty.is_never() => {}
            _ => {
                let params = [repr(ty)];
                let name = self.helper(Helper::Print { quoted }, ty, &params, None, |fns, f| {
                    let v = f.param(0);
                    if ty.is_union() {
                        let (cases, end) = switch_tag(f, v, ty);
                        for (kind, case) in cases {
                            f.switch_to(case);
                            let piece = ty.only(kind);
                            let p = payload(f, v, piece);
                            fns.print(f, p, piece, quoted);
                            f.jmp(end, vec![]);
                        }
                        f.switch_to(end);
                    } else {
                        let elem = ty.elem().unwrap_or(Ty::NEVER);
                        let open = f.iconst('[' as i64);
                        f.call("rt.out_byte", vec![open], None);
                        for_each(f, v, |f, i| {
                            // ", " before every element but the first
                            let zero = f.iconst(0);
                            let later = f.icmp(ICond::Sgt, i, zero);
                            let sep = f.new_block("sep");
                            let item = f.new_block("item");
                            f.br(later, sep, vec![], item, vec![]);
                            f.switch_to(sep);
                            let comma = f.iconst(',' as i64);
                            f.call("rt.out_byte", vec![comma], None);
                            let space = f.iconst(' ' as i64);
                            f.call("rt.out_byte", vec![space], None);
                            f.jmp(item, vec![]);
                            f.switch_to(item);
                            let e = load_elem(f, v, i, elem);
                            fns.print(f, e, elem, true);
                        });
                        let close = f.iconst(']' as i64);
                        f.call("rt.out_byte", vec![close], None);
                    }
                    f.ret(None);
                });
                b.call(&name, vec![v], None);
            }
        }
    }

    /// 1 if `x` and `y` (both of type `ty`) are equal, else 0. Arrays are
    /// equal when they have the same length and equal elements; values of
    /// a union type when they have the same kind and equal payloads.
    pub fn eq(&mut self, b: &mut FunctionBuilder, x: Value, y: Value, ty: Ty) -> Value {
        match ty {
            Ty::INT | Ty::BOOL => return b.icmp(ICond::Eq, x, y),
            Ty::FLOAT => return b.fcmp(crate::lir::FCond::Eq, x, y),
            Ty::VOID => return b.iconst(1),
            Ty::STR => {
                return b.call("rt.str_eq", vec![x, y], Some(Type::I64)).unwrap();
            }
            _ if ty.is_never() => return b.iconst(1),
            _ => {}
        }
        let params = [repr(ty), repr(ty)];
        let name = self.helper(Helper::Eq, ty, &params, Some(Type::I64), |fns, f| {
            let (x, y) = (f.param(0), f.param(1));
            let zero = f.iconst(0);
            let differ = f.new_block("differ");
            if ty.is_union() {
                let tx = f.load(MemTy::I64, x, 0);
                let ty_tag = f.load(MemTy::I64, y, 0);
                let same = f.icmp(ICond::Eq, tx, ty_tag);
                let compare = f.new_block("compare");
                f.br(same, compare, vec![], differ, vec![]);
                f.switch_to(compare);
                let (cases, end) = switch_tag(f, x, ty);
                for (kind, case) in cases {
                    f.switch_to(case);
                    let piece = ty.only(kind);
                    let px = payload(f, x, piece);
                    let py = payload(f, y, piece);
                    let r = fns.eq(f, px, py, piece);
                    f.ret(Some(r));
                }
                f.switch_to(end);
                f.unreachable();
            } else {
                let elem = ty.elem().unwrap_or(Ty::NEVER);
                let lx = f.load(MemTy::I64, x, 0);
                let ly = f.load(MemTy::I64, y, 0);
                let same_len = f.icmp(ICond::Eq, lx, ly);
                let elements = f.new_block("elements");
                f.br(same_len, elements, vec![], differ, vec![]);
                f.switch_to(elements);
                for_each(f, x, |f, i| {
                    let ex = load_elem(f, x, i, elem);
                    let ey = load_elem(f, y, i, elem);
                    let same = fns.eq(f, ex, ey, elem);
                    let next = f.new_block("next_elem");
                    f.br(same, next, vec![], differ, vec![]);
                    f.switch_to(next);
                });
                let one = f.iconst(1);
                f.ret(Some(one));
            }
            f.switch_to(differ);
            f.ret(Some(zero));
        });
        b.call(&name, vec![x, y], Some(Type::I64)).unwrap()
    }
}
