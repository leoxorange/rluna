//! Ownership checking: every value has exactly one owner.
//!
//! Luna follows Rust's ownership model without references:
//!
//! - A local variable or parameter *owns* its value.
//! - Giving the value somewhere else **moves** it, and the old name becomes
//!   invalid. A value is given away by binding it (`y = x`), passing it as
//!   an argument (`f x`), returning it (the function's last expression), or
//!   making it the result of a `check` arm (always, even when the `check`
//!   itself is only read, as in `echo (check ...)`: like Rust's `if`, a
//!   `check` produces a value of its own).
//! - Reading a value **borrows** it, and the name stays valid: operators
//!   (`x + 1`, `x == y`), `echo x`, the scrutinee and patterns of `check`.
//! - Values of `int`, `float` and `boolean` are plain machine numbers and
//!   are **copied** instead of moved, as with Rust's `Copy` types. `void` is
//!   not a value at all, so there is nothing to own. Strings and values of
//!   mixed types (`numeric`, `any`, ...) live on the heap and move.
//!
//! Using a name after its value has moved is an `OwnershipError`. This is
//! what lets the compiler free memory automatically (see `lower`): every
//! value has exactly one owner, so it is freed exactly once, when that owner
//! is done with it, and nothing can still point at it.
//!
//! Why check after type checking? Whether a value is copied or moved depends
//! on its type, and types are only known per specialized instance: in
//! `pick a = b = a`, `pick 1` copies an int but `pick "s"` moves a string.
//! So the check runs on each instance in the TIR.
//!
//! The check follows the order in which the program runs, tracking which
//! names have been moved so far. `check` is the only branching construct:
//! each arm starts from the state before it, and afterwards a name counts as
//! moved if *any* arm moved it, because the compiler cannot know which arm
//! will run.

use std::collections::HashMap;

use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::typeck::{TExpr, TExprKind, TFunc, TStmt, Tir};

/// Something that can own a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    Param(usize),
    Local(usize),
}

pub fn check(tir: &Tir) -> Result<(), Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();
    for f in &tir.funcs {
        let mut c = Checker {
            f,
            moved: HashMap::new(),
            errors: Vec::new(),
        };
        c.function();
        // Instances of the same function can report the same error.
        for e in c.errors {
            if !errors
                .iter()
                .any(|o| o.span == e.span && o.message == e.message)
            {
                errors.push(e);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// How an expression's value is used by its context.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// The value is given away: it ends up owned by someone else.
    Move,
    /// The value is only read.
    Borrow,
}

struct Checker<'a> {
    f: &'a TFunc,
    /// Owners whose value has been moved, with where it happened.
    moved: HashMap<Owner, Span>,
    errors: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn name(&self, owner: Owner) -> &str {
        match owner {
            Owner::Param(i) => &self.f.param_names[i],
            Owner::Local(l) => &self.f.local_names[l],
        }
    }

    fn function(&mut self) {
        // Every statement's value goes somewhere: into a binding, out of the
        // function (the last one), or it is discarded (consumed). All of
        // these move.
        for stmt in &self.f.body {
            match stmt {
                TStmt::Let { value, .. } => self.expr(value, Use::Move),
                TStmt::Expr(e) => self.expr(e, Use::Move),
            }
        }
    }

    fn expr(&mut self, e: &TExpr, usage: Use) {
        match &e.kind {
            TExprKind::Param(i) => self.owner(Owner::Param(*i), e, usage),
            TExprKind::Local(l) => self.owner(Owner::Local(*l), e, usage),
            TExprKind::Int(_) | TExprKind::Float(_) | TExprKind::Str(_) | TExprKind::Bool(_) => {}
            // Arguments move into the callee, left to right: in `f x x` the
            // second `x` is already gone.
            TExprKind::Call { args, .. } | TExprKind::Diverge(args) => {
                for a in args {
                    self.expr(a, Use::Move);
                }
            }
            TExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, Use::Borrow);
                self.expr(rhs, Use::Borrow);
            }
            TExprKind::Neg(x) | TExprKind::Echo(x) => self.expr(x, Use::Borrow),
            // A conversion (boxing, unboxing) passes its value through, so
            // it is used the same way as the conversion itself.
            TExprKind::Convert(x) => self.expr(x, usage),
            TExprKind::Check {
                scrutinee,
                arms,
                else_arm,
            } => {
                self.expr(scrutinee, Use::Borrow);
                // Patterns are tested one after another, so each starts where
                // the previous one left off. An arm's body runs only if its
                // pattern matched, so it starts from the state after that
                // pattern, and the next pattern does not see its moves.
                let mut after: HashMap<Owner, Span> = HashMap::new();
                for (pattern, body) in arms {
                    self.expr(pattern, Use::Borrow);
                    let before_body = self.moved.clone();
                    self.expr(body, Use::Move);
                    merge(&mut after, &self.moved);
                    self.moved = before_body;
                }
                // Without `else` the arms cover every value, so falling past
                // the last pattern never happens and adds no path.
                if let Some(body) = else_arm {
                    self.expr(body, Use::Move);
                    merge(&mut after, &self.moved);
                }
                self.moved = after;
            }
        }
    }

    fn owner(&mut self, owner: Owner, e: &TExpr, usage: Use) {
        if let Some(&moved_at) = self.moved.get(&owner) {
            let name = self.name(owner).to_string();
            self.errors.push(
                Diagnostic::error(
                    ErrorKind::OwnershipError,
                    e.span,
                    format!("`{name}` is used after its value was moved"),
                )
                .with_note(
                    moved_at,
                    format!("the value of `{name}` ({}) was moved here", e.ty),
                ),
            );
            return;
        }
        // Only heap values have an owner; numbers are simply copied.
        if usage == Use::Move && e.ty.needs_drop() {
            self.moved.insert(owner, e.span);
        }
    }
}

/// Add the moves of one path to the moves after a branch.
fn merge(into: &mut HashMap<Owner, Span>, path: &HashMap<Owner, Span>) {
    for (owner, span) in path {
        into.entry(*owner).or_insert(*span);
    }
}
