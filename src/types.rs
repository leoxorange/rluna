//! Luna's type lattice.
//!
//! Every runtime value has one of six concrete *kinds*: int, float, string,
//! boolean, void and array. A static type is a set of kinds: `numeric` =
//! {int, float}, `any` = all kinds, and the empty set (`never`) is the type
//! of expressions that never produce a value. An array type also says what
//! its elements are: `[int]` is an array of ints. All elements of an array
//! have that one type.
//!
//! `void` is the "nothing" of Luna: there is exactly one void, and it marks
//! code that produces nothing. A type that is "something or nothing", such
//! as `int` plus void, is written `int?`: it arises when one branch of a
//! function or `check` gives void and another gives a value (`head []` is
//! void, `head [1, 2]` is 1, so `head` gives `int?`). Operators let the
//! nothing flow through (`int? + 1` is `int?`); places that need a real
//! value make the program handle it first (see `typeck`).
//!
//! Thinking of types as *sets of possible kinds* makes the type rules simple
//! set operations:
//!
//! - "can this value be passed where `numeric` is expected?" is a subset test;
//! - "what does `check` return when one arm gives int and another string?"
//!   is the union, `int|string`;
//! - "which kinds survive a runtime check against `numeric`?" is the
//!   intersection.
//!
//! The sets form a *lattice*: `never` at the bottom, `any` at the top, and
//! union/intersection to move up and down. Type inference (see `typeck`)
//! starts low and only moves up, which is why it always terminates.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use crate::ast::TypeName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Int,
    Float,
    Str,
    Bool,
    Void,
    Array,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Int,
        Kind::Float,
        Kind::Str,
        Kind::Bool,
        Kind::Void,
        Kind::Array,
    ];

    /// Runtime tag stored in boxed values. When a value's kind is only known
    /// at run time, the program carries this number with it so it can ask
    /// "is this an int?" while running. The numbering must match the comment
    /// at the top of `runtime/core.lir`, which reads the same tags.
    pub fn tag(self) -> i64 {
        match self {
            Kind::Int => 1,
            Kind::Float => 2,
            Kind::Str => 3,
            Kind::Bool => 4,
            Kind::Void => 5,
            Kind::Array => 6,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Int => "int",
            Kind::Float => "float",
            Kind::Str => "string",
            Kind::Bool => "boolean",
            Kind::Void => "void",
            Kind::Array => "array",
        }
    }

    fn bit(self) -> u8 {
        1 << (self.tag() - 1)
    }
}

/// A set of kinds stored as a bit set (bit `tag - 1` is set when the kind is
/// in the set), plus the element type when the set contains arrays.
///
/// `elem` points to an element type stored once for the whole compilation
/// (see `intern`), so a `Ty` stays a small value that can be copied freely,
/// even though array types nest (`[[int]]`). `None` with the array kind
/// means "an array of anything", as in the declared types `array` and
/// `iterable`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ty {
    kinds: u8,
    elem: Option<&'static Ty>,
}

/// The one shared copy of each element type.
fn intern(t: Ty) -> &'static Ty {
    static TYPES: OnceLock<Mutex<HashSet<&'static Ty>>> = OnceLock::new();
    let mut types = TYPES.get_or_init(Default::default).lock().unwrap();
    if let Some(&known) = types.get(&t) {
        return known;
    }
    let stored: &'static Ty = Box::leak(Box::new(t));
    types.insert(stored);
    stored
}

const ARRAY_BIT: u8 = 32;

impl Ty {
    pub const NEVER: Ty = Ty::bits(0);
    pub const INT: Ty = Ty::bits(1);
    pub const FLOAT: Ty = Ty::bits(2);
    pub const STR: Ty = Ty::bits(4);
    pub const BOOL: Ty = Ty::bits(8);
    pub const VOID: Ty = Ty::bits(16);
    pub const NUMERIC: Ty = Ty::bits(1 | 2);
    /// Any array.
    pub const ARRAY: Ty = Ty::bits(ARRAY_BIT);
    /// Strings and arrays: what `len`, `head`, loops ... can go through.
    pub const ITERABLE: Ty = Ty::bits(4 | ARRAY_BIT);
    pub const ANY: Ty = Ty::bits(63);

    const fn bits(kinds: u8) -> Ty {
        Ty { kinds, elem: None }
    }

    /// An array whose elements are of type `elem`.
    pub fn array(elem: Ty) -> Ty {
        Ty {
            kinds: ARRAY_BIT,
            elem: Some(intern(elem)),
        }
    }

    /// The element type of the arrays in this type: `None` if it has no
    /// arrays, `Some(ANY)` for "any array".
    pub fn elem(self) -> Option<Ty> {
        self.contains(Kind::Array)
            .then(|| self.elem.copied().unwrap_or(Ty::ANY))
    }

    /// A type with the given kinds and array element type.
    fn make(kinds: u8, elem: Option<&'static Ty>) -> Ty {
        Ty {
            kinds,
            // Only an array type has an element type.
            elem: if kinds & ARRAY_BIT != 0 { elem } else { None },
        }
    }

    pub fn of(kind: Kind) -> Ty {
        Ty::bits(kind.bit())
    }

    pub fn from_name(name: TypeName) -> Ty {
        match name {
            TypeName::Numeric => Ty::NUMERIC,
            TypeName::Int => Ty::INT,
            TypeName::Float => Ty::FLOAT,
            TypeName::String => Ty::STR,
            TypeName::Boolean => Ty::BOOL,
            TypeName::Void => Ty::VOID,
            TypeName::Array => Ty::ARRAY,
            TypeName::Iterable => Ty::ITERABLE,
            TypeName::Any => Ty::ANY,
        }
    }

    pub fn union(self, other: Ty) -> Ty {
        let both_arrays = self.contains(Kind::Array) && other.contains(Kind::Array);
        let elem = match (self.elem, other.elem) {
            (Some(a), Some(b)) => Some(intern(a.union(*b))),
            // "any array" absorbs every element type
            _ if both_arrays => None,
            (a, b) => a.or(b),
        };
        Ty::make(self.kinds | other.kinds, elem)
    }

    pub fn intersect(self, other: Ty) -> Ty {
        let elem = match (self.elem, other.elem) {
            (Some(a), Some(b)) => Some(intern(a.intersect(*b))),
            (a, b) => a.or(b),
        };
        Ty::make(self.kinds & other.kinds, elem)
    }

    /// The kinds of `self` that are not in `other`. An array part is removed
    /// only if `other` accepts every array `self` may hold.
    pub fn without(self, other: Ty) -> Ty {
        let mut kinds = self.kinds & !other.kinds;
        if self.contains(Kind::Array)
            && other.contains(Kind::Array)
            && !self.array_part().is_subset(other.array_part())
        {
            kinds |= ARRAY_BIT;
        }
        Ty::make(kinds, self.elem)
    }

    fn array_part(self) -> Ty {
        Ty::make(self.kinds & ARRAY_BIT, self.elem)
    }

    /// Every value of `self` is also a value of `other`: no kind of `self`
    /// is outside `other`, and arrays hold elements `other` accepts.
    pub fn is_subset(self, other: Ty) -> bool {
        if self.kinds & !other.kinds != 0 {
            return false;
        }
        match (self.elem, other.elem) {
            _ if !self.contains(Kind::Array) => true,
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => a.is_subset(*b),
        }
    }

    pub fn is_never(self) -> bool {
        self.kinds == 0
    }

    pub fn contains(self, kind: Kind) -> bool {
        self.kinds & kind.bit() != 0
    }

    /// May be void, along with other kinds: a "maybe" type like `int?`.
    pub fn is_maybe(self) -> bool {
        self.contains(Kind::Void) && self.is_union()
    }

    /// The single kind of this type, if it has exactly one.
    pub fn as_kind(self) -> Option<Kind> {
        let mut it = self.kinds();
        match (it.next(), it.next()) {
            (Some(k), None) => Some(k),
            _ => None,
        }
    }

    /// A type with two or more kinds is represented as a boxed value at runtime.
    /// A value of type `int` is known to be an int, so it can be kept as a
    /// raw 64-bit number. A value of type `int|string` could be either, so
    /// it must carry its tag along with it: a "box".
    pub fn is_union(self) -> bool {
        self.kinds.count_ones() > 1
    }

    /// The part of this type of one kind: `Ty::of(kind)`, but keeping the
    /// element type of arrays.
    pub fn only(self, kind: Kind) -> Ty {
        Ty::make(kind.bit(), self.elem)
    }

    pub fn kinds(self) -> impl Iterator<Item = Kind> {
        Kind::ALL.into_iter().filter(move |k| self.contains(*k))
    }

    /// Values of this type live on the heap and must be freed: strings,
    /// arrays, and boxes of union types. `int`, `float` and `boolean` are
    /// plain numbers in a register or stack slot, and `void` is no value at
    /// all.
    pub fn needs_drop(self) -> bool {
        self == Ty::STR || self.as_kind() == Some(Kind::Array) || self.is_union()
    }

    /// Values of this type follow the `Copy` trail: they are copied where
    /// other values move, and a borrowed one can be given away (as a copy).
    /// These are the types whose values are only numbers: `int`, `float`,
    /// `boolean`, `void`, and unions of them such as `numeric` or `int?`.
    /// Copying one costs no more than moving it (a register, or a 16-byte
    /// box), unlike a string or an array, whose copy grows with its size and
    /// must be asked for with `copy`.
    pub fn is_copy(self) -> bool {
        !self.contains(Kind::Str) && !self.contains(Kind::Array)
    }

    /// Bit mask of the kinds, as used by runtime type checks.
    pub fn mask(self) -> i64 {
        self.kinds as i64
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Ty::ANY {
            return f.write_str("any");
        }
        if self.is_maybe() {
            // "something or nothing": `int?`, `(int|string)?`
            let base = self.without(Ty::VOID);
            return if base.is_union() && base != Ty::NUMERIC && base != Ty::ITERABLE {
                write!(f, "({base})?")
            } else {
                write!(f, "{base}?")
            };
        }
        match *self {
            Ty::NEVER => f.write_str("never"),
            Ty::NUMERIC => f.write_str("numeric"),
            Ty::ITERABLE => f.write_str("iterable"),
            t => {
                let names: Vec<String> = t
                    .kinds()
                    .map(|k| match (k, t.elem) {
                        (Kind::Array, Some(e)) => format!("[{e}]"),
                        _ => k.name().to_string(),
                    })
                    .collect();
                f.write_str(&names.join("|"))
            }
        }
    }
}

impl fmt::Debug for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice() {
        assert!(Ty::INT.is_subset(Ty::NUMERIC));
        assert!(!Ty::STR.is_subset(Ty::NUMERIC));
        assert!(Ty::NEVER.is_subset(Ty::INT));
        assert_eq!(Ty::INT.union(Ty::FLOAT), Ty::NUMERIC);
        assert_eq!(Ty::ANY.intersect(Ty::STR), Ty::STR);
        assert_eq!(Ty::NUMERIC.as_kind(), None);
        assert_eq!(Ty::FLOAT.as_kind(), Some(Kind::Float));
        assert_eq!(Ty::INT.union(Ty::STR).to_string(), "int|string");
        assert_eq!(Ty::ANY.to_string(), "any");
    }

    #[test]
    fn maybe_types() {
        let maybe_int = Ty::INT.union(Ty::VOID);
        assert!(maybe_int.is_maybe());
        assert!(!Ty::VOID.is_maybe());
        assert_eq!(maybe_int.to_string(), "int?");
        assert_eq!(
            Ty::INT.union(Ty::STR).union(Ty::VOID).to_string(),
            "(int|string)?"
        );
        assert_eq!(maybe_int.without(Ty::VOID), Ty::INT);
    }

    #[test]
    fn arrays() {
        let ints = Ty::array(Ty::INT);
        let strs = Ty::array(Ty::STR);
        let nested = Ty::array(ints);
        assert_eq!(ints.to_string(), "[int]");
        assert_eq!(nested.to_string(), "[[int]]");
        assert_eq!(ints, Ty::array(Ty::INT));
        assert!(ints.is_subset(Ty::ARRAY));
        assert!(ints.is_subset(Ty::ITERABLE));
        assert!(!ints.is_subset(strs));
        assert!(!Ty::ARRAY.is_subset(ints));
        // an empty array literal fits any array type
        assert!(Ty::array(Ty::NEVER).is_subset(ints));
        assert_eq!(ints.union(strs), Ty::array(Ty::INT.union(Ty::STR)));
        assert_eq!(ints.union(Ty::ARRAY), Ty::ARRAY);
        assert_eq!(Ty::ARRAY.intersect(ints), ints);
        assert_eq!(ints.elem(), Some(Ty::INT));
        assert_eq!(Ty::ARRAY.elem(), Some(Ty::ANY));
        assert_eq!(Ty::INT.elem(), None);
        assert!(ints.needs_drop());
        assert_eq!(ints.union(Ty::VOID).to_string(), "[int]?");
        assert_eq!(ints.union(Ty::INT).without(Ty::ARRAY), Ty::INT);
        assert_eq!(ints.union(Ty::INT).only(Kind::Array), ints);
    }
}
