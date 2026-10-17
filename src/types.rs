//! Luna's type lattice.
//!
//! Every runtime value has one of five concrete *kinds*. A static type is a
//! set of kinds: `numeric` = {int, float}, `any` = all kinds, and the empty
//! set (`never`) is the type of expressions that never produce a value.

use std::fmt;

use crate::ast::TypeName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Int,
    Float,
    Str,
    Bool,
    Void,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Int, Kind::Float, Kind::Str, Kind::Bool, Kind::Void];

    /// Runtime tag stored in boxed values.
    pub fn tag(self) -> i64 {
        match self {
            Kind::Int => 1,
            Kind::Float => 2,
            Kind::Str => 3,
            Kind::Bool => 4,
            Kind::Void => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Int => "int",
            Kind::Float => "float",
            Kind::Str => "string",
            Kind::Bool => "boolean",
            Kind::Void => "void",
        }
    }

    fn bit(self) -> u8 {
        1 << (self.tag() - 1)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ty(u8);

impl Ty {
    pub const NEVER: Ty = Ty(0);
    pub const INT: Ty = Ty(1);
    pub const FLOAT: Ty = Ty(2);
    pub const STR: Ty = Ty(4);
    pub const BOOL: Ty = Ty(8);
    pub const VOID: Ty = Ty(16);
    pub const NUMERIC: Ty = Ty(1 | 2);
    pub const ANY: Ty = Ty(31);

    pub fn of(kind: Kind) -> Ty {
        Ty(kind.bit())
    }

    pub fn from_name(name: TypeName) -> Ty {
        match name {
            TypeName::Numeric => Ty::NUMERIC,
            TypeName::Int => Ty::INT,
            TypeName::Float => Ty::FLOAT,
            TypeName::String => Ty::STR,
            TypeName::Boolean => Ty::BOOL,
            TypeName::Void => Ty::VOID,
            TypeName::Any => Ty::ANY,
        }
    }

    pub fn union(self, other: Ty) -> Ty {
        Ty(self.0 | other.0)
    }

    pub fn intersect(self, other: Ty) -> Ty {
        Ty(self.0 & other.0)
    }

    pub fn is_subset(self, other: Ty) -> bool {
        self.0 & !other.0 == 0
    }

    pub fn is_never(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, kind: Kind) -> bool {
        self.0 & kind.bit() != 0
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
    pub fn is_union(self) -> bool {
        self.0.count_ones() > 1
    }

    pub fn kinds(self) -> impl Iterator<Item = Kind> {
        Kind::ALL.into_iter().filter(move |k| self.contains(*k))
    }

    /// Bit mask of the kinds, as used by runtime type checks.
    pub fn mask(self) -> i64 {
        self.0 as i64
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Ty::NEVER => f.write_str("never"),
            Ty::ANY => f.write_str("any"),
            Ty::NUMERIC => f.write_str("numeric"),
            t => {
                let names: Vec<&str> = t.kinds().map(Kind::name).collect();
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
}
