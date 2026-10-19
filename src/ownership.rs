//! Ownership checking: every value has exactly one owner.
//!
//! Luna follows Rust's ownership model:
//!
//! - A variable (a parameter or a local) *owns* its value.
//! - Giving the value somewhere else **moves** it, and the old name becomes
//!   invalid. A value is given away by binding it (`y = x`), passing it to a
//!   parameter (`f x`), returning it (the function's last expression, or
//!   `ret`), putting it in an array (`[x]`), making it the result of a
//!   `check` arm, or handing it to an operation that builds something new
//!   out of it: `a ++ b`, `a ** b`, `tail a`, `init a`, `reverse a`, and a
//!   `loop` or list comprehension going through `a`. Moving copies nothing:
//!   the new owner gets the same object, and the old name is forgotten.
//! - **Borrowing** lends a value without giving it away, and the owner keeps
//!   it: operators that compute something new (`x + 1`, `x == y`), `echo`,
//!   `len`, `copy`, the value checked by `check`, a parameter marked `@`
//!   (`f @a = ...`), and a loop or comprehension over `@a`.
//! - Something borrowed can be read, and lent on to another `@` parameter,
//!   but not given away: it still belongs to someone else. That includes
//!   a `@` parameter inside its function, the variable of a loop over `@a`,
//!   and the elements `head a`, `last a` and `a !! i`, which are the
//!   elements themselves, still inside `a`. To keep such a value, the
//!   program asks for a copy: `copy a`.
//! - Values of `Copy` types (`int`, `float`, `boolean`, `void`, and unions
//!   of them, such as `numeric`) are numbers: they are **copied** instead of
//!   moved, so the old name stays valid, and a borrowed one can be given
//!   away as a copy. Copying one costs no more than moving it.
//!
//! So Luna never copies a string or an array unless the program says
//! `copy`. Using a name after its value has moved is an `OwnershipError`.
//! This is what lets the compiler free memory automatically (see `lower`):
//! every value has exactly one owner, so it is freed exactly once, when that
//! owner is done with it, and nothing can still point at it.
//!
//! While something is borrowed, its owner must keep it: moving the owner,
//! or giving it a new value (which frees the old one), while the borrowed
//! value is still in use is an error. In `f (head a) a`, the second argument
//! would move `a` while `f` still reads its first element; in
//! `loop x in @a:`, the body cannot move `a` or give it a new value.
//!
//! Why check after type checking? Whether a value is copied or moved depends
//! on its type, and types are only known per specialized instance: in
//! `pick a = b = a`, `pick 1` copies an int but `pick "s"` moves a string.
//! So the check runs on each instance in the TIR.
//!
//! - Giving a variable a new value (`x = ...`) makes it usable again after
//!   a move. If it still owned a value, that old value is freed (see `lower`).
//!
//! The check follows the order in which the program runs, tracking which
//! names have been moved so far. In a `check`, each arm starts from the
//! state before it, and afterwards a name counts as moved if *any* arm moved
//! it, because the compiler cannot know which arm will run.
//!
//! A `loop` body must leave every outside variable as it found it. If the
//! body moved `s` and did not give it a new value, the next iteration would
//! use a moved value; if `s` was moved before the loop and the body gives it
//! one, whether `s` holds a value after the loop would depend on how many
//! times the loop ran. Both are errors, so the state after a loop is always
//! the state before it, however often the body runs (even zero times).
//! (What the loop goes through is evaluated once, before the loop, so a
//! loop over `a` moves `a` for good.)

use std::collections::HashMap;

use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::typeck::{
    LoopSource, TClause, TExpr, TExprKind, TFunc, TStmt, Tir, gives_element_itself,
};
use crate::types::Ty;

/// Something that can own a value: a variable, by id.
type Owner = usize;

pub fn check(tir: &Tir) -> Result<(), Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();
    for f in &tir.funcs {
        let mut c = Checker {
            tir,
            f,
            moved: HashMap::new(),
            borrows: Vec::new(),
            lent: f
                .borrowed
                .iter()
                .enumerate()
                .filter(|(_, b)| **b)
                .map(|(i, _)| (i, Lent::Param))
                .collect(),
            diverged: false,
            loops: Vec::new(),
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

/// Why a variable holds something borrowed.
#[derive(Clone, Copy)]
enum Lent {
    /// A `@` parameter: its value belongs to the caller.
    Param,
    /// The variable of a loop or comprehension over `@a`: an element of `a`.
    Element,
}

struct Checker<'a> {
    tir: &'a Tir,
    f: &'a TFunc,
    /// Owners whose value has been moved, with where it happened.
    moved: HashMap<Owner, Span>,
    /// Owners whose value is borrowed by something still in use, with where
    /// it was borrowed: the operands of the operation being evaluated, and
    /// what an enclosing loop over `@a` goes through. Each operation removes
    /// its operands' borrows once it is done (it truncates the list back).
    borrows: Vec<(Owner, Span)>,
    /// Variables holding something borrowed, which they cannot give away.
    lent: HashMap<Owner, Lent>,
    /// The current path has left the function (`ret`): it continues nowhere,
    /// so its moves do not matter for what follows.
    diverged: bool,
    /// The loops being checked, innermost last: what was moved before the
    /// loop, its body's own variables, and where its source is.
    loops: Vec<(HashMap<Owner, Span>, Vec<Owner>, Span)>,
    errors: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn name(&self, owner: Owner) -> &str {
        &self.f.local_names[owner]
    }

    fn function(&mut self) {
        let f = self.f;
        // The last expression's value is the result, moved to the caller,
        // unless the function gives nothing.
        let returns = !f.discard && f.ret != Ty::VOID && !f.ret.is_never();
        let n = f.body.len();
        for (i, stmt) in f.body.iter().enumerate() {
            match stmt {
                TStmt::Expr(e) if returns && i + 1 == n => self.expr(e, Use::Move),
                _ => self.stmt(stmt),
            }
        }
    }

    fn stmts(&mut self, stmts: &[TStmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &TStmt) {
        let mark = self.borrows.len();
        match stmt {
            TStmt::Let { local, value } => {
                self.expr(value, Use::Move);
                if let Some(&(_, at)) = self.borrows.iter().find(|(o, _)| o == local) {
                    let name = self.name(*local).to_string();
                    self.errors.push(
                        Diagnostic::error(
                            ErrorKind::OwnershipError,
                            value.span,
                            format!(
                                "`{name}` gets a new value here while it is borrowed, which would free the value still in use"
                            ),
                        )
                        .with_note(at, format!("`{name}` is borrowed here")),
                    );
                }
                // The variable holds a value again.
                self.moved.remove(local);
            }
            // A value that is thrown away is given away (and dropped), unless
            // it is borrowed: then there is nothing to give.
            TStmt::Expr(e) => {
                let usage = if e.is_borrowed() {
                    Use::Borrow
                } else {
                    Use::Move
                };
                self.expr(e, usage);
            }
            TStmt::Loop {
                var,
                source,
                borrowed,
                body,
                inner,
                ..
            } => {
                self.loop_source(source, *borrowed, *var);
                let exprs = source.exprs();
                let range = exprs[0].span.to(exprs[exprs.len() - 1].span);
                let before = self.moved.clone();
                // The body's own variables start fresh every iteration.
                for l in inner {
                    self.moved.remove(l);
                }
                self.loops.push((before, inner.clone(), range));
                self.stmts(body);
                // A body that always leaves the function (or the round, with
                // `over` or `jump`, checked there) does not reach its end.
                if !std::mem::take(&mut self.diverged) {
                    self.round_ends(
                        "so the next iteration would find it without a value; give `{name}` a new value before the end of the loop body",
                        None,
                    );
                }
                let (before, _, _) = self.loops.pop().unwrap();
                self.moved = before;
            }
        }
        self.borrows.truncate(mark);
    }

    /// A round of the innermost loop ends here: at the end of its body, at a
    /// `jump` or at an `over`. Then the variables from outside the loop must
    /// be as they were before it, for the next round and after the loop.
    /// `then` says what would go wrong with a variable moved in the loop;
    /// `early` is the `over` or `jump` that ends the round.
    fn round_ends(&mut self, then: &str, early: Option<(Span, &str)>) {
        let (before, inner, range) = self.loops.last().unwrap();
        let outside = |owner: &Owner| !inner.contains(owner);
        let mut moved: Vec<(Owner, Span)> = self
            .moved
            .iter()
            .filter(|(o, _)| outside(o) && !before.contains_key(o))
            .map(|(o, s)| (*o, *s))
            .collect();
        moved.sort_by_key(|(o, _)| *o);
        let mut revived: Vec<(Owner, Span)> = before
            .iter()
            .filter(|(o, _)| outside(o) && !self.moved.contains_key(o))
            .map(|(o, s)| (*o, *s))
            .collect();
        revived.sort_by_key(|(o, _)| *o);
        let range = *range;
        for (owner, span) in moved {
            let name = self.name(owner).to_string();
            let mut d = Diagnostic::error(
                ErrorKind::OwnershipError,
                span,
                format!(
                    "`{name}` is moved in this loop, {}",
                    then.replace("{name}", &name)
                ),
            );
            if let Some((at, what)) = early {
                d = d.with_note(at, what);
            }
            self.errors.push(d);
        }
        for (owner, moved_at) in revived {
            let name = self.name(owner).to_string();
            self.errors.push(
                Diagnostic::error(
                    ErrorKind::OwnershipError,
                    range,
                    format!(
                        "`{name}` gets a value in this loop, but it was moved before the loop, so after the loop it would hold a value only if the loop ran"
                    ),
                )
                .with_note(moved_at, format!("the value of `{name}` was moved here")),
            );
        }
    }

    /// What a loop or a generator goes through. It moves into the loop,
    /// unless it is borrowed (`in @a`); then it stays borrowed (it is not
    /// taken off `borrows`) for as long as the loop runs, and the variable
    /// holds borrowed elements.
    fn loop_source(&mut self, source: &LoopSource, borrowed: bool, var: Owner) {
        match source {
            LoopSource::Range(a, b) => {
                let mark = self.borrows.len();
                self.expr(a, Use::Borrow);
                self.expr(b, Use::Borrow);
                self.borrows.truncate(mark);
            }
            LoopSource::Each(e) if borrowed => {
                self.expr(e, Use::Borrow);
                // The characters of a string are new strings, owned by the
                // variable; the elements of an array are still the array's.
                if e.ty.elem().is_some() {
                    self.lent.insert(var, Lent::Element);
                }
            }
            LoopSource::Each(e) => self.expr(e, Use::Move),
        }
    }

    /// Evaluate the operands of an operation that only reads them; their
    /// borrows end with it.
    fn read(&mut self, operands: &[&TExpr]) {
        let mark = self.borrows.len();
        for e in operands {
            self.expr(e, Use::Borrow);
        }
        self.borrows.truncate(mark);
    }

    /// Evaluate the operands of an operation that takes them.
    fn take(&mut self, operands: &[&TExpr]) {
        let mark = self.borrows.len();
        for e in operands {
            self.expr(e, Use::Move);
        }
        self.borrows.truncate(mark);
    }

    /// Call arguments, left to right: each moves into the callee, or is
    /// borrowed for the duration of the call if its parameter has `@`. In
    /// `f x x` the second `x` is already gone; in `g x x` with `g @a b`, the
    /// second would move `x` while `g` borrows it.
    fn args(&mut self, args: &[TExpr], borrowed: &[bool]) {
        let mark = self.borrows.len();
        for (a, &b) in args.iter().zip(borrowed) {
            self.expr(a, if b { Use::Borrow } else { Use::Move });
        }
        self.borrows.truncate(mark);
    }

    fn expr(&mut self, e: &TExpr, usage: Use) {
        // An element given by `!!` is still inside its array: it cannot be
        // given away.
        if usage == Use::Move
            && !e.ty.is_copy()
            && matches!(e.kind, TExprKind::Index(..) | TExprKind::Field(..))
            && e.is_borrowed()
        {
            let is_field = matches!(e.kind, TExprKind::Field(..));
            let whose = match e.borrowed_from() {
                Some(l) => format!("`{}`", self.name(l)),
                None if is_field => "its value".to_string(),
                None => "its array".to_string(),
            };
            let msg = if is_field {
                format!(
                    "this is a field still inside {whose}, so it cannot be given away: a field cannot be moved out of its value; to keep it, give a copy with `copy`"
                )
            } else {
                format!(
                    "this is an element still inside {whose} (`!!` gives the element itself, not a copy), so it cannot be given away; to keep it, give a copy with `copy`"
                )
            };
            self.errors
                .push(Diagnostic::error(ErrorKind::OwnershipError, e.span, msg));
        }
        match &e.kind {
            TExprKind::Local(l) => self.owner(*l, e, usage),
            TExprKind::Int(_)
            | TExprKind::Float(_)
            | TExprKind::Str(_)
            | TExprKind::Bool(_)
            | TExprKind::Void => {}
            TExprKind::Call { inst, args } => {
                self.args(args, &self.tir.funcs[*inst].borrowed);
            }
            // Every definition a dispatch may reach borrows the same
            // parameters (see `hir`).
            TExprKind::Dispatch { args, cases, .. } => {
                self.args(args, &self.tir.funcs[cases[0].inst].borrowed);
            }
            // The call is never reached: its arguments are only evaluated.
            TExprKind::Diverge(args) => self.read(&args.iter().collect::<Vec<_>>()),
            TExprKind::Binary { lhs, rhs, .. } => self.read(&[lhs, rhs]),
            // The value leaves with the function.
            TExprKind::Ret(value) => {
                if let Some(v) = value {
                    self.take(&[v]);
                }
                self.diverged = true;
            }
            // The round ends here, and what follows on this path never runs.
            TExprKind::Jump => {
                self.round_ends(
                    "so the next iteration would find it without a value; give `{name}` a new value before this `jump`",
                    Some((e.span, "the next iteration starts here")),
                );
                self.diverged = true;
            }
            TExprKind::Over => {
                self.round_ends(
                    "so after the loop it would have no value; give `{name}` a new value before this `over`",
                    Some((e.span, "the loop ends here")),
                );
                self.diverged = true;
            }
            TExprKind::Neg(x) | TExprKind::Is(x, _) => self.read(&[x]),
            // A field is like an element: the field itself keeps its value
            // borrowed while it is used.
            TExprKind::Field(base, _) if e.is_borrowed() => self.expr(base, Use::Borrow),
            TExprKind::Field(base, _) => self.read(&[base]),
            TExprKind::Textualize(x) => self.read(&[x]),
            // The values move into the new value, through variables of its
            // own (which the defaults of later fields borrow).
            TExprKind::Construct { steps, .. } => {
                let mark = self.borrows.len();
                for (l, v) in steps {
                    self.expr(v, Use::Move);
                    self.moved.remove(l);
                }
                self.borrows.truncate(mark);
            }
            TExprKind::Index(a, i) => {
                if gives_element_itself(a.ty) {
                    self.expr(a, Use::Borrow);
                    self.read(&[i]);
                } else {
                    self.read(&[a, i]);
                }
            }
            // `a ++ b` and `a ** b` build their result from the operands'
            // elements, so they take them.
            TExprKind::Concat(a, b) | TExprKind::ElemMul(a, b) => self.take(&[a, b]),
            // Elements are moved into the new array.
            TExprKind::Array(items) => self.take(&items.iter().collect::<Vec<_>>()),
            TExprKind::Range(a, b) => self.read(&[a, b]),
            TExprKind::Comprehension { body, clauses } => self.comprehension(body, clauses),
            // A conversion (boxing, unboxing) passes its value through, so
            // it is used the same way as the conversion itself.
            TExprKind::Convert(x) => self.expr(x, usage),
            TExprKind::Check {
                scrutinee,
                arms,
                else_arm,
                ..
            } => {
                // The checked value is only compared with the patterns, so
                // an arm may move it: no pattern is tested after the arm.
                self.read(&[scrutinee]);
                // Patterns are tested one after another, so each starts where
                // the previous one left off. An arm's body runs only if its
                // pattern matched, so it starts from the state after that
                // pattern, and the next pattern does not see its moves.
                // An arm that leaves the function (`ret`) does not reach the
                // code after the `check`, so its moves are not merged.
                let mut after: HashMap<Owner, Span> = HashMap::new();
                let mut all_diverge = true;
                for (pattern, body) in arms {
                    self.read(&[pattern]);
                    let before_body = self.moved.clone();
                    self.take(&[body]);
                    if !std::mem::take(&mut self.diverged) {
                        merge(&mut after, &self.moved);
                        all_diverge = false;
                    }
                    self.moved = before_body;
                }
                // Without `else` the arms cover every value, so falling past
                // the last pattern never happens and adds no path.
                if let Some(body) = else_arm {
                    self.take(&[body]);
                    if !std::mem::take(&mut self.diverged) {
                        merge(&mut after, &self.moved);
                        all_diverge = false;
                    }
                }
                self.moved = after;
                self.diverged = all_diverge;
            }
            // The block of an `if`: its statements in order. Its value is a
            // new value, so the last statement gives its value up, as an arm
            // of a `check` does. Its own variables are new each time it
            // runs, and gone after it.
            TExprKind::Block { stmts, inner } => {
                for l in inner {
                    self.moved.remove(l);
                }
                let n = stmts.len();
                for (i, stmt) in stmts.iter().enumerate() {
                    match stmt {
                        TStmt::Expr(v) if i + 1 == n => self.expr(v, Use::Move),
                        _ => self.stmt(stmt),
                    }
                }
                for l in inner {
                    self.moved.remove(l);
                }
            }
        }
    }

    /// The generators of a list comprehension are nested loops, and the
    /// body runs once for every combination of their elements. What the
    /// first generator goes through is evaluated once, so it may move a
    /// variable from outside for good. Everything after it runs repeatedly:
    /// what a later generator goes through runs once per element of the
    /// generator before it, and the guards and the body once per element of
    /// the latest generator. Each of them may move only the variables of
    /// that generator, which are new for every element; moving anything
    /// else would leave the next element without it.
    fn comprehension(&mut self, body: &TExpr, clauses: &[TClause]) {
        let mark = self.borrows.len();
        let mut after_first: Option<HashMap<Owner, Span>> = None;
        let mut fresh: Vec<Owner> = Vec::new();
        for c in clauses {
            let start = self.moved.clone();
            match c {
                TClause::Gen {
                    pos,
                    var,
                    source,
                    borrowed,
                } => {
                    self.loop_source(source, *borrowed, *var);
                    if after_first.is_none() {
                        after_first = Some(self.moved.clone());
                    } else {
                        self.only_fresh_moved(&start, &fresh);
                    }
                    fresh = std::iter::once(*var).chain(*pos).collect();
                    for v in &fresh {
                        self.moved.remove(v);
                    }
                }
                TClause::Guard(g) => {
                    self.read(&[g]);
                    self.only_fresh_moved(&start, &fresh);
                }
            }
        }
        let start = self.moved.clone();
        self.take(&[body]);
        self.only_fresh_moved(&start, &fresh);
        self.moved = after_first.expect("a comprehension starts with a generator");
        self.borrows.truncate(mark);
    }

    /// Report the moves since `start` of variables other than `fresh`.
    fn only_fresh_moved(&mut self, start: &HashMap<Owner, Span>, fresh: &[Owner]) {
        let mut moved: Vec<(Owner, Span)> = self
            .moved
            .iter()
            .filter(|(o, _)| !start.contains_key(o) && !fresh.contains(o))
            .map(|(o, s)| (*o, *s))
            .collect();
        moved.sort_by_key(|(o, _)| *o);
        for (owner, span) in moved {
            let name = self.name(owner).to_string();
            self.errors.push(Diagnostic::error(
                ErrorKind::OwnershipError,
                span,
                format!(
                    "`{name}` is moved in a list comprehension, so the next element would find it without a value"
                ),
            ));
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
        match usage {
            Use::Borrow => self.borrows.push((owner, e.span)),
            // Numbers are simply copied.
            Use::Move if e.ty.is_copy() => {}
            Use::Move => {
                let name = self.name(owner).to_string();
                if let Some(lent) = self.lent.get(&owner) {
                    // A field's default is a function of the fields before
                    // it (see `hir`), named `Trail$field`.
                    let in_default = self.f.name.contains('$');
                    let whose = match lent {
                        Lent::Param if in_default => {
                            format!("`{name}` is a field of the value being made, which keeps it")
                        }
                        Lent::Param => format!(
                            "`{name}` is borrowed (`@{name}`): its value still belongs to the caller"
                        ),
                        Lent::Element => format!(
                            "`{name}` is an element borrowed from what the loop goes through (`in @...`)"
                        ),
                    };
                    let msg = if in_default {
                        format!(
                            "{whose}, so a default cannot give it away; give a copy with `copy {name}`"
                        )
                    } else {
                        format!(
                            "{whose}, so it cannot be given away; lend it on (to a `@` parameter, or a loop over `@{name}`), or give a copy with `copy {name}`"
                        )
                    };
                    self.errors
                        .push(Diagnostic::error(ErrorKind::OwnershipError, e.span, msg));
                } else if let Some(&(_, at)) = self.borrows.iter().find(|(o, _)| *o == owner) {
                    self.errors.push(
                        Diagnostic::error(
                            ErrorKind::OwnershipError,
                            e.span,
                            format!(
                                "`{name}` is moved here while it is borrowed, which would free the value still in use"
                            ),
                        )
                        .with_note(at, format!("`{name}` is borrowed here")),
                    );
                } else {
                    self.moved.insert(owner, e.span);
                }
            }
        }
    }
}

/// Add the moves of one path to the moves after a branch.
fn merge(into: &mut HashMap<Owner, Span>, path: &HashMap<Owner, Span>) {
    for (owner, span) in path {
        into.entry(*owner).or_insert(*span);
    }
}
