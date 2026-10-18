//! Luna's type lattice.
//!
//! Every runtime value has one of seven concrete *kinds*: int, float,
//! string, boolean, void, array and record (a value of a trail the program
//! defines, such as `Fruit`). A static type is a set of kinds: `Numeric` =
//! {int, float}, `Any` = all kinds, and the empty set (`Never`) is the type
//! of expressions that never produce a value. An array type also says what
//! its elements are: `[Int]` is an array of ints. All elements of an array
//! have that one type. A record type says which trail it is: `Fruit`.
//!
//! The readme calls all of these *trails*; this module is where they are
//! sets of kinds.
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
    /// A value of a trail the program defines (`Fruit {name = "kiwi"}`).
    Record,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Int,
        Kind::Float,
        Kind::Str,
        Kind::Bool,
        Kind::Void,
        Kind::Array,
        Kind::Record,
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
            Kind::Record => 7,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Int => "Int",
            Kind::Float => "Float",
            Kind::Str => "String",
            Kind::Bool => "Boolean",
            Kind::Void => "Void",
            Kind::Array => "Array",
            Kind::Record => "Record",
        }
    }

    fn bit(self) -> u8 {
        1 << (self.tag() - 1)
    }
}

/// A set of kinds stored as a bit set (bit `tag - 1` is set when the kind is
/// in the set), plus the element type when the set contains arrays, and the
/// trail when it contains records.
///
/// `elem` points to an element type stored once for the whole compilation
/// (see `intern`), so a `Ty` stays a small value that can be copied freely,
/// even though array types nest (`[[int]]`). `None` with the array kind
/// means "an array of anything", as in the declared types `array` and
/// `iterable`.
///
/// `trail` is the global name of the trail (`Fruit`, or `food.Fruit` for one
/// from a linked file), stored once like `elem`. `None` with the record kind
/// means "a value of any trail", as in `Any`. A type holds values of at most
/// one trail: `typeck` reports a mix as an error (see `Ty::clashes`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ty {
    kinds: u8,
    elem: Option<&'static Ty>,
    trail: Option<&'static str>,
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

/// The one shared copy of each trail name.
fn intern_name(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock().unwrap();
    if let Some(&known) = names.get(name) {
        return known;
    }
    let stored: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(stored);
    stored
}

const ARRAY_BIT: u8 = 32;
const RECORD_BIT: u8 = 64;

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
    pub const ANY: Ty = Ty::bits(127);
    /// A value of any trail the program defines.
    pub const ANY_RECORD: Ty = Ty::bits(RECORD_BIT);

    const fn bits(kinds: u8) -> Ty {
        Ty {
            kinds,
            elem: None,
            trail: None,
        }
    }

    /// An array whose elements are of type `elem`.
    pub fn array(elem: Ty) -> Ty {
        Ty {
            kinds: ARRAY_BIT,
            elem: Some(intern(elem)),
            trail: None,
        }
    }

    /// A value of the trail with this global name.
    pub fn record(trail: &str) -> Ty {
        Ty {
            kinds: RECORD_BIT,
            elem: None,
            trail: Some(intern_name(trail)),
        }
    }

    /// The trail of the records in this type, if it has records of one.
    pub fn trail(self) -> Option<&'static str> {
        if self.contains(Kind::Record) {
            self.trail
        } else {
            None
        }
    }

    /// Joining `self` and `other` would mix values of two trails, which a
    /// type cannot hold (an array of fruits and warehouses, a `check` giving
    /// one or the other).
    pub fn clashes(self, other: Ty) -> bool {
        match (self.trail(), other.trail()) {
            (Some(a), Some(b)) if a != b => true,
            _ => match (self.elem, other.elem) {
                (Some(a), Some(b)) => a.clashes(*b),
                _ => false,
            },
        }
    }

    /// The trails named anywhere in this type: its own, and its elements'.
    pub fn trails(self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = self.trail().into_iter().collect();
        if let Some(e) = self.elem {
            out.extend(e.trails());
        }
        out
    }

    /// The element type of the arrays in this type: `None` if it has no
    /// arrays, `Some(ANY)` for "any array".
    pub fn elem(self) -> Option<Ty> {
        self.contains(Kind::Array)
            .then(|| self.elem.copied().unwrap_or(Ty::ANY))
    }

    /// A type with the given kinds, array element type and trail.
    fn make(kinds: u8, elem: Option<&'static Ty>, trail: Option<&'static str>) -> Ty {
        Ty {
            kinds,
            // Only an array type has an element type, and only a record
            // type a trail.
            elem: if kinds & ARRAY_BIT != 0 { elem } else { None },
            trail: if kinds & RECORD_BIT != 0 { trail } else { None },
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
        let both_records = self.contains(Kind::Record) && other.contains(Kind::Record);
        let trail = match (self.trail(), other.trail()) {
            // "any trail" absorbs every trail (a clash is `typeck`'s to report)
            (Some(a), Some(_)) => Some(a),
            _ if both_records => None,
            (a, b) => a.or(b),
        };
        Ty::make(self.kinds | other.kinds, elem, trail)
    }

    pub fn intersect(self, other: Ty) -> Ty {
        let elem = match (self.elem, other.elem) {
            (Some(a), Some(b)) => Some(intern(a.intersect(*b))),
            (a, b) => a.or(b),
        };
        let mut kinds = self.kinds & other.kinds;
        // Records of two different trails have nothing in common.
        if matches!((self.trail(), other.trail()), (Some(a), Some(b)) if a != b) {
            kinds &= !RECORD_BIT;
        }
        Ty::make(kinds, elem, self.trail().or(other.trail()))
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
        if self.contains(Kind::Record)
            && other.contains(Kind::Record)
            && !self.only(Kind::Record).is_subset(other.only(Kind::Record))
        {
            kinds |= RECORD_BIT;
        }
        Ty::make(kinds, self.elem, self.trail)
    }

    fn array_part(self) -> Ty {
        Ty::make(self.kinds & ARRAY_BIT, self.elem, None)
    }

    /// Every value of `self` is also a value of `other`: no kind of `self`
    /// is outside `other`, and arrays hold elements `other` accepts.
    pub fn is_subset(self, other: Ty) -> bool {
        if self.kinds & !other.kinds != 0 {
            return false;
        }
        // Records fit if `other` takes any trail, or theirs.
        if self.contains(Kind::Record) {
            match (self.trail(), other.trail()) {
                (_, None) => {}
                (Some(a), Some(b)) if a == b => {}
                _ => return false,
            }
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
        Ty::make(kind.bit(), self.elem, self.trail)
    }

    pub fn kinds(self) -> impl Iterator<Item = Kind> {
        Kind::ALL.into_iter().filter(move |k| self.contains(*k))
    }

    /// Values of this type live on the heap and must be freed: strings,
    /// arrays, and boxes of union types. `int`, `float` and `boolean` are
    /// plain numbers in a register or stack slot, and `void` is no value at
    /// all.
    pub fn needs_drop(self) -> bool {
        self == Ty::STR
            || matches!(self.as_kind(), Some(Kind::Array | Kind::Record))
            || self.is_union()
    }

    /// Values of this type follow the `Copy` trail: they are copied where
    /// other values move, and a borrowed one can be given away (as a copy).
    /// These are the types whose values are only numbers: `int`, `float`,
    /// `boolean`, `void`, and unions of them such as `numeric` or `int?`.
    /// Copying one costs no more than moving it (a register, or a 16-byte
    /// box), unlike a string or an array, whose copy grows with its size and
    /// must be asked for with `copy`.
    pub fn is_copy(self) -> bool {
        !self.contains(Kind::Str) && !self.contains(Kind::Array) && !self.contains(Kind::Record)
    }

    /// Bit mask of the kinds, as used by runtime type checks.
    pub fn mask(self) -> i64 {
        self.kinds as i64
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Ty::ANY {
            return f.write_str("Any");
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
            Ty::NEVER => f.write_str("Never"),
            Ty::NUMERIC => f.write_str("Numeric"),
            Ty::ITERABLE => f.write_str("Iterable"),
            t => {
                let names: Vec<String> = t
                    .kinds()
                    .map(|k| match (k, t.elem, t.trail) {
                        (Kind::Array, Some(e), _) => format!("[{e}]"),
                        (Kind::Record, _, Some(name)) => name.to_string(),
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
        assert_eq!(Ty::INT.union(Ty::STR).to_string(), "Int|String");
        assert_eq!(Ty::ANY.to_string(), "Any");
    }

    #[test]
    fn maybe_types() {
        let maybe_int = Ty::INT.union(Ty::VOID);
        assert!(maybe_int.is_maybe());
        assert!(!Ty::VOID.is_maybe());
        assert_eq!(maybe_int.to_string(), "Int?");
        assert_eq!(
            Ty::INT.union(Ty::STR).union(Ty::VOID).to_string(),
            "(Int|String)?"
        );
        assert_eq!(maybe_int.without(Ty::VOID), Ty::INT);
    }

    #[test]
    fn arrays() {
        let ints = Ty::array(Ty::INT);
        let strs = Ty::array(Ty::STR);
        let nested = Ty::array(ints);
        assert_eq!(ints.to_string(), "[Int]");
        assert_eq!(nested.to_string(), "[[Int]]");
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
        assert_eq!(ints.union(Ty::VOID).to_string(), "[Int]?");
        assert_eq!(ints.union(Ty::INT).without(Ty::ARRAY), Ty::INT);
        assert_eq!(ints.union(Ty::INT).only(Kind::Array), ints);
    }

    #[test]
    fn records() {
        let fruit = Ty::record("Fruit");
        let shop = Ty::record("Shop");
        assert_eq!(fruit, Ty::record("Fruit"));
        assert_eq!(fruit.to_string(), "Fruit");
        assert_eq!(fruit.union(Ty::VOID).to_string(), "Fruit?");
        assert!(fruit.is_subset(Ty::ANY));
        assert!(!fruit.is_subset(shop));
        assert!(!Ty::ANY.is_subset(fruit));
        assert_eq!(Ty::ANY.intersect(fruit), fruit);
        assert!(fruit.intersect(shop).is_never());
        assert!(fruit.clashes(shop) && !fruit.clashes(fruit));
        assert!(Ty::array(fruit).clashes(Ty::array(shop)));
        assert_eq!(fruit.union(Ty::VOID).without(Ty::VOID), fruit);
        assert_eq!(fruit.union(Ty::INT).without(fruit), Ty::INT);
        assert_eq!(fruit.union(Ty::INT).without(shop), fruit.union(Ty::INT));
        assert!(fruit.needs_drop() && !fruit.is_copy());
        assert_eq!(fruit.union(Ty::ANY), Ty::ANY);
        assert_eq!(Ty::array(fruit).trails(), vec!["Fruit"]);
    }
}
