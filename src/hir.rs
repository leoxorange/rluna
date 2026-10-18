//! Name resolution: AST -> HIR.
//!
//! Every name is resolved here, so later passes never deal with scopes.
//! Lookup order is variables (the function's parameters and locals, the
//! innermost scope first), then global definitions; failing both is a
//! `SymbolNotFoundError`. Overloads are grouped by name; picking a specific
//! overload by argument types is left to type checking.
//!
//! Variables: a function's parameters are its first variables (ids 0, 1,
//! ...), followed by its locals. `x = value` gives a value to the variable
//! `x` if one is visible, and otherwise creates it. Each `loop` body is a
//! scope of its own: its loop variable and the variables it creates exist
//! only inside it, while `total = total + i` in the body updates the
//! `total` from outside.
//!
//! What is HIR? A "high-level IR": the same tree shape as the AST, but every
//! name has been replaced by what it refers to. `x` becomes `Local(3)`, and a
//! call names a function group. Strings are replaced by numbers because
//! comparing numbers is cheaper and unambiguous: two different variables can
//! both be called `x` (one in a loop body, one outside it) yet get different
//! ids, so later stages cannot confuse them.
//!
//! Why resolve names before type checking? Scoping rules (shadowing, which
//! `b` is meant) are a separate concern from types. Handling them in one
//! place keeps the type checker simpler, and means "unknown name" errors
//! are reported before any confusing type errors they would cause.

use std::collections::HashMap;

use crate::ast::{self, BinOp, ExprKind, Stmt, TypeName};
use crate::diag::{Diagnostic, ErrorKind, Span};
use crate::types::{Kind, Ty};

/// The functions built into the language (chapter 2 of the readme). A
/// program that defines a function with the same name uses its own instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// The number of elements (or characters).
    Len,
    /// The first element, or void if there is none.
    Head,
    /// Everything after the first element, or void if there is none.
    Tail,
    /// Everything before the last element, or void if there is none.
    Init,
    /// The last element, or void if there is none.
    Last,
    /// The elements (or characters) in the opposite order.
    Reverse,
    /// A copy of any value, owned by whoever receives it: the way to keep
    /// something borrowed (`copy a` inside `f @a = ...`, `copy (head a)`).
    Copy,
}

impl Builtin {
    pub fn from_name(name: &str) -> Option<Builtin> {
        Some(match name {
            "len" => Builtin::Len,
            "head" => Builtin::Head,
            "tail" => Builtin::Tail,
            "init" => Builtin::Init,
            "last" => Builtin::Last,
            "reverse" => Builtin::Reverse,
            "copy" => Builtin::Copy,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Len => "len",
            Builtin::Head => "head",
            Builtin::Tail => "tail",
            Builtin::Init => "init",
            Builtin::Last => "last",
            Builtin::Reverse => "reverse",
            Builtin::Copy => "copy",
        }
    }
}

/// The built-in function a global name stands for: its own name, or
/// `builtin.len` from a file where `len` means something else (see
/// `FileNames::hidden`).
fn builtin_of(global: &str) -> Option<Builtin> {
    Builtin::from_name(global.strip_prefix("builtin.").unwrap_or(global))
}

/// How the names written in one file reach the global definitions, in a
/// program made of several files (see `driver`, which builds one table per
/// file). Each file sees its own definitions, the ones it links, and the
/// standard library; the definitions of every file but the main one have
/// global names of their own (`entity/try.pred`), so files never clash.
#[derive(Debug, Default, Clone)]
pub struct FileNames {
    /// The file, for messages (`entity/try.luna`).
    pub file: String,
    /// Names written without a `.` that mean a global of another name: the
    /// file's own definitions, and the definitions it links by name.
    pub names: HashMap<String, String>,
    /// Names of the main file's definitions, which a linked file does not
    /// see: what they mean there instead (the standard library's or a
    /// built-in function of that name), if anything.
    pub hidden: HashMap<String, Option<String>>,
    /// The files linked whole, by the name they are linked as: the file
    /// (for messages) and its definitions (name -> global name).
    pub files: HashMap<String, (String, HashMap<String, String>)>,
}

impl FileNames {
    /// The global name `written` refers to in this file.
    fn global(&self, written: &str) -> Result<String, String> {
        if let Some((alias, member)) = written.split_once('.') {
            let Some((file, defs)) = self.files.get(alias) else {
                return Err(format!(
                    "cannot find symbol `{written}`: no file is linked as `{alias}` in `{}`",
                    self.file
                ));
            };
            return defs.get(member).cloned().ok_or_else(|| {
                format!("cannot find symbol `{written}`: `{file}` has no definition `{member}`")
            });
        }
        if let Some(global) = self.names.get(written) {
            return Ok(global.clone());
        }
        match self.hidden.get(written) {
            Some(Some(global)) => Ok(global.clone()),
            Some(None) => Err(format!(
                "cannot find symbol `{written}` (the main file defines it, but `{}` does not link it)",
                self.file
            )),
            None => Ok(written.to_string()),
        }
    }
}

/// Built-in constants: names with a fixed float value.
fn constant(name: &str) -> Option<f64> {
    match name {
        "PI" => Some(std::f64::consts::PI),
        "infinite" => Some(f64::INFINITY),
        _ => None,
    }
}

pub type FuncId = usize;
pub type LocalId = usize;

#[derive(Debug)]
pub struct Hir {
    pub funcs: Vec<Func>,
    /// Overloads by name, in definition order.
    pub groups: HashMap<String, Vec<FuncId>>,
    pub main: FuncId,
    /// The trails the program defines, by global name.
    pub trails: HashMap<String, HTrail>,
    /// The trails each embedded value follows, as `std.luna` declares
    /// (through the trails they follow too): ints follow Numeric, Show,
    /// Compare, Copy. An array follows them only when its elements do.
    pub embedded: HashMap<Kind, Vec<String>>,
}

/// A trail the program defines.
#[derive(Debug)]
pub struct HTrail {
    /// Its global name (`Fruit`, `food.Fruit`).
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<HField>,
    /// The trails it follows, directly or through others, and itself:
    /// `Fruit`, `Show`, `Copy`, ...
    pub follows: Vec<String>,
    /// It follows `Copy`: `copy` accepts its values.
    pub follows_copy: bool,
    /// It follows `Show`: `echo` prints its `textualize`.
    pub shows: bool,
    /// Its functions, inherited ones too.
    pub methods: Vec<HMethod>,
    /// One of the embedded trails `std.luna` declares (`Show`, `Compare`,
    /// ...): others follow it, but it has no values of its own.
    pub library: bool,
}

/// A function of a trail, called on a value: `t.describe 1`.
#[derive(Debug, Clone)]
pub struct HMethod {
    pub name: String,
    pub span: Span,
    /// The function group that gives it (its first parameter is the value
    /// itself), or `None`: a trail that follows this one must give it.
    pub func: Option<String>,
    /// The trail that asks for it (or gives it).
    pub from: String,
    /// Its definitions' parameters after the value: for each number of
    /// parameters it takes, which are borrowed (all its definitions with as
    /// many parameters borrow the same ones, as for any function).
    pub shapes: Vec<Vec<bool>>,
    /// The inherited function this one's definitions come before: a call
    /// tries the trail's own definitions first, then those it inherits.
    pub then: Option<String>,
}

/// The embedded value a trail name of `std.luna` declares: `trail Int
/// follow Numeric, Copy:` says which trails ints follow.
pub fn embedded_kind(name: &str) -> Option<Kind> {
    Some(match name {
        "Int" => Kind::Int,
        "Float" => Kind::Float,
        "String" => Kind::Str,
        "Boolean" => Kind::Bool,
        "Void" => Kind::Void,
        "Array" => Kind::Array,
        _ => return None,
    })
}

#[derive(Debug)]
pub struct HField {
    pub name: String,
    pub span: Span,
    /// The declared trail, if written. A field whose trail leads back to
    /// its own (`next :: Node` in `Node`) may be void: `Node?`.
    pub ty: Option<Ty>,
    pub default: FieldDefault,
}

/// What a field holds when a value does not give it.
#[derive(Debug, Clone)]
pub enum FieldDefault {
    /// Nothing: every value must give it.
    Required,
    /// Computed by the function `name`, whose parameters are the fields
    /// it uses (borrowed): `textualize = copy name` uses `name`.
    Func { name: String, uses: Vec<String> },
    /// Void: a field that leads back to its own trail, without a default.
    Void,
}

#[derive(Debug)]
pub struct Func {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<String>,
    /// Declared parameter types (`any` when unannotated). A parameter
    /// declared with a trail that others can follow accepts them all: its
    /// type here takes any record (or any value, for `Show`), and `bounds`
    /// says which.
    pub param_tys: Vec<Ty>,
    /// For each parameter, the trail its value must follow (`:: Fruit`
    /// takes a Fruit or a trail that follows Fruit; `:: Show` anything that
    /// can be printed).
    pub bounds: Vec<Option<String>>,
    /// The trail the result must follow, if declared as one.
    pub ret_bound: Option<String>,
    /// Declared return type, if annotated.
    pub ret: Option<Ty>,
    pub body: Vec<HStmt>,
    /// Name of every variable, by id: the parameters first, then the locals.
    pub local_names: Vec<String>,
    /// For each parameter, the pattern that takes its array apart, if any.
    pub patterns: Vec<Option<HPattern>>,
    /// For each parameter, whether it is borrowed (`@a`). All definitions
    /// with the same name and number of parameters agree on this.
    pub borrowed: Vec<bool>,
}

/// A clause of a list comprehension.
#[derive(Debug)]
pub enum HClause {
    Gen {
        pos: Option<LocalId>,
        var: LocalId,
        source: HExpr,
        /// `in @source`: the source is borrowed, not moved.
        borrowed: bool,
    },
    Guard(HExpr),
}

/// A pattern parameter, with the variables it binds.
#[derive(Debug, Clone)]
pub enum HPattern {
    /// `(x:xs)`: `heads` get the first elements, `rest` the remaining array.
    Cons { heads: Vec<LocalId>, rest: LocalId },
    /// `[]`.
    Empty,
}

#[derive(Debug)]
pub enum HStmt {
    /// Give the variable `local` a value (its first one, or a new one).
    Let {
        local: LocalId,
        value: HExpr,
    },
    Expr(HExpr),
    Loop {
        /// `loop i, x in ...`: the position variable, and where it is named.
        pos: Option<(LocalId, Span)>,
        var: LocalId,
        /// Where the loop variable is named, for error messages.
        var_span: Span,
        /// A range `[a..b]`, an array or a string.
        source: HExpr,
        /// `in @source`: the source is borrowed, not moved.
        borrowed: bool,
        body: Vec<HStmt>,
        /// Variables from outside the loop that the body gives new values:
        /// their values carry over from one iteration to the next.
        carried: Vec<LocalId>,
        /// Variables that exist only inside the body (including `var`).
        inner: Vec<LocalId>,
    },
}

#[derive(Debug)]
pub struct HExpr {
    pub kind: HExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum HExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Void,
    Local(LocalId),
    /// Call of a global function group; the overload is chosen by type checking.
    Call {
        name: String,
        args: Vec<HExpr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<HExpr>,
        rhs: Box<HExpr>,
    },
    Neg(Box<HExpr>),
    /// `ret value` / `ret`.
    Ret(Option<Box<HExpr>>),
    /// A call of a built-in function, which takes one argument.
    Builtin(Builtin, Box<HExpr>),
    Array(Vec<HExpr>),
    Range(Box<HExpr>, Box<HExpr>),
    /// `[body | clauses]`; the generators' variables exist only inside.
    Comprehension {
        body: Box<HExpr>,
        clauses: Vec<HClause>,
    },
    Echo(Box<HExpr>),
    Check {
        scrutinee: Box<HExpr>,
        arms: Vec<(HExpr, HExpr)>,
        else_arm: Option<Box<HExpr>>,
    },
    /// A value of a trail. `steps` evaluate the fields into variables of
    /// their own (the fields given, in the order written, then the defaults
    /// of the others, which read the fields before them); `slots` names the
    /// variable of each field, in the trail's order, and the value takes
    /// them all.
    Construct {
        trail: String,
        steps: Vec<(LocalId, HExpr)>,
        slots: Vec<LocalId>,
    },
    /// A field of a value: `t.name`.
    Field(Box<HExpr>, String, Span),
    /// A function of a value's trail, called on it: `t.describe 1`.
    MethodCall {
        receiver: Box<HExpr>,
        name: String,
        name_span: Span,
        args: Vec<HExpr>,
    },
}

/// The embedded trails a program's trail cannot be named after.
fn embedded_trail(name: &str) -> bool {
    TypeName::parse(name).is_some() || matches!(name, "Copy" | "Show" | "Compare" | "Self")
}

/// The trail a written trail name means: an embedded one, or one the
/// program defines (looked up like a function name, see `FileNames`).
fn resolve_ty(
    ann: &ast::TypeAnn,
    names: Option<&FileNames>,
    trails: &HashMap<String, &ast::Trail>,
) -> Result<Ty, Diagnostic> {
    match &ann.kind {
        ast::TypeAnnKind::Array(inner) => Ok(Ty::array(resolve_ty(inner, names, trails)?)),
        ast::TypeAnnKind::Name(n) => {
            if let Some(t) = TypeName::parse(n) {
                return Ok(Ty::from_name(t));
            }

            let global = match names {
                None => n.clone(),
                Some(f) => f
                    .global(n)
                    .map_err(|m| Diagnostic::error(ErrorKind::SymbolNotFoundError, ann.span, m))?,
            };
            match trails.get(&global) {
                // An embedded trail such as Show, as the trail of a field:
                // any value (one that follows it, in a signature, see
                // `bound_of`).
                Some(t) if t.library => Ok(Ty::ANY),
                Some(_) => Ok(Ty::record(&global)),
                None => Err(Diagnostic::error(
                    ErrorKind::SymbolNotFoundError,
                    ann.span,
                    format!("cannot find the trail `{n}`"),
                )),
            }
        }
    }
}

/// The trails of the program, with their fields: inherited from the
/// trails they follow, then their own. A field a trail declares again
/// (`textualize = copy name` in a trail that follows Show) replaces the
/// inherited one where the trail declares it, keeping its trail.
fn build_trails(
    defs: &HashMap<String, &ast::Trail>,
    ann_ty: &dyn Fn(&ast::TypeAnn, usize) -> Result<Ty, Diagnostic>,
    trail_name: &dyn Fn(&str, usize) -> Option<String>,
    errors: &mut Vec<Diagnostic>,
) -> HashMap<String, HTrail> {
    let mut built: HashMap<String, HTrail> = HashMap::new();
    let mut names: Vec<&String> = defs.keys().collect();
    names.sort();
    for name in names {
        build_trail(
            name,
            defs,
            ann_ty,
            trail_name,
            &mut built,
            &mut Vec::new(),
            errors,
        );
    }
    // A field whose trail leads back to its own (directly, or through
    // other fields) could only hold an endless chain of values; it may be
    // void instead, and is void when not given.
    let reaches = |built: &HashMap<String, HTrail>, start: &str, goal: &str| -> bool {
        let mut seen = vec![start.to_string()];
        let mut stack = vec![start.to_string()];
        while let Some(t) = stack.pop() {
            if t == goal {
                return true;
            }
            for f in built.get(&t).map(|t| &t.fields[..]).unwrap_or(&[]) {
                if let Some(next) = f.ty.and_then(|t| t.trail())
                    && !seen.iter().any(|s| s == next)
                {
                    seen.push(next.to_string());
                    stack.push(next.to_string());
                }
            }
        }
        false
    };
    let names: Vec<String> = built.keys().cloned().collect();
    for name in &names {
        for k in 0..built[name].fields.len() {
            let target = built[name].fields[k].ty.and_then(|t| t.trail());
            if let Some(target) = target
                && reaches(&built, target, name)
            {
                let field = &mut built.get_mut(name).unwrap().fields[k];
                field.ty = field.ty.map(|t| t.union(Ty::VOID));
                if matches!(field.default, FieldDefault::Required) {
                    field.default = FieldDefault::Void;
                }
            }
        }
    }
    built
}

/// Build `name` (and the trails it follows, first) into `built`.
fn build_trail(
    name: &str,
    defs: &HashMap<String, &ast::Trail>,
    ann_ty: &dyn Fn(&ast::TypeAnn, usize) -> Result<Ty, Diagnostic>,
    trail_name: &dyn Fn(&str, usize) -> Option<String>,
    built: &mut HashMap<String, HTrail>,
    stack: &mut Vec<String>,
    errors: &mut Vec<Diagnostic>,
) {
    if built.contains_key(name) || stack.iter().any(|s| s == name) {
        return;
    }
    let t = defs[name];
    stack.push(name.to_string());
    let mut follows = vec![name.to_string()];
    let mut fields: Vec<HField> = Vec::new();
    let mut methods: Vec<HMethod> = Vec::new();
    let inherit = |methods: &mut Vec<HMethod>, ms: Vec<HMethod>| {
        for m in ms {
            match methods.iter().position(|o| o.name == m.name) {
                // given by one trail followed, asked for by another
                Some(k) if methods[k].func.is_none() => methods[k] = m,
                Some(_) => {}
                None => methods.push(m),
            }
        }
    };
    for (parent, span) in &t.follows {
        // Every trail follows Any; only the embedded values follow
        // Iterable and Array (and are Int, String, ...).
        if parent == "Any" {
            continue;
        }
        if parent == "Iterable" || embedded_kind(parent).is_some() {
            errors.push(Diagnostic::error(
                ErrorKind::DefinitionError,
                *span,
                format!("only the embedded values follow `{parent}`, for now"),
            ));
            continue;
        }
        let inherited: Vec<HField> = match trail_name(parent, t.file) {
            Some(g) if stack.contains(&g) => {
                errors.push(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    *span,
                    format!("`{}` follows itself (through `{parent}`)", t.name),
                ));
                continue;
            }
            Some(g) => {
                build_trail(&g, defs, ann_ty, trail_name, built, stack, errors);
                let Some(p) = built.get(&g) else {
                    continue;
                };
                for f in &p.follows {
                    if !follows.contains(f) {
                        follows.push(f.clone());
                    }
                }
                inherit(&mut methods, p.methods.clone());
                p.fields
                    .iter()
                    .map(|f| HField {
                        name: f.name.clone(),
                        span: f.span,
                        ty: f.ty,
                        default: f.default.clone(),
                    })
                    .collect()
            }
            None => {
                errors.push(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    *span,
                    format!("cannot find the trail `{parent}`"),
                ));
                continue;
            }
        };
        for f in inherited {
            match fields.iter().find(|g| g.name == f.name) {
                // the same field from two trails followed: one field
                Some(g) if g.ty == f.ty || f.ty.is_none() => {}
                Some(g) => errors.push(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    *span,
                    format!(
                        "`{}` gets the field `{}` twice, as {} and as {}",
                        t.name,
                        f.name,
                        g.ty.unwrap_or(Ty::ANY),
                        f.ty.unwrap_or(Ty::ANY)
                    ),
                )),
                None => fields.push(f),
            }
        }
    }
    for f in &t.fields {
        let declared = match &f.ty {
            Some(a) => match ann_ty(a, t.file) {
                Ok(ty) => Some(ty),
                Err(e) => {
                    errors.push(e);
                    None
                }
            },
            None => None,
        };
        let inherited = fields.iter().position(|g| g.name == f.name);
        let mut ty = declared;
        let mut default = match f.default {
            Some(_) => FieldDefault::Func {
                name: format!("{name}${}", f.name),
                uses: Vec::new(),
            },
            None => FieldDefault::Required,
        };
        if let Some(k) = inherited {
            let old = fields.remove(k);
            match (declared, old.ty) {
                (Some(new), Some(was)) if new != was => errors.push(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    f.span,
                    format!(
                        "`{}` is {was} in the trail `{}` follows, so it cannot be {new} here",
                        f.name, t.name
                    ),
                )),
                (None, was) => ty = was,
                _ => {}
            }
            if f.default.is_none() {
                default = old.default;
            }
        }
        fields.push(HField {
            name: f.name.clone(),
            span: f.span,
            ty,
            default,
        });
    }
    // Its own functions, each with all its definitions.
    let mut own: Vec<HMethod> = Vec::new();
    for m in &t.methods {
        if fields.iter().any(|f| f.name == m.name) {
            errors.push(Diagnostic::error(
                ErrorKind::DefinitionError,
                m.span,
                format!(
                    "`{}` is a field of `{}`, so it cannot also be a function",
                    m.name, t.name
                ),
            ));
            continue;
        }
        let k = match own.iter().position(|o| o.name == m.name) {
            Some(k) => k,
            None => {
                own.push(HMethod {
                    name: m.name.clone(),
                    span: m.span,
                    func: None,
                    from: name.to_string(),
                    shapes: Vec::new(),
                    then: None,
                });
                own.len() - 1
            }
        };
        let shape: Vec<bool> = m.params.iter().map(|p| p.borrowed).collect();
        match own[k].shapes.iter().find(|s| s.len() == shape.len()) {
            Some(s) if *s != shape => errors.push(Diagnostic::error(
                ErrorKind::DefinitionError,
                m.span,
                format!(
                    "every definition of `{}` with {} parameter(s) must borrow (`@`) the same parameters",
                    m.name,
                    shape.len()
                ),
            )),
            Some(_) => {}
            None => own[k].shapes.push(shape),
        }
        if m.body.is_some() {
            own[k].func = Some(format!("{name}${}", m.name));
        }
    }
    // A trail's own definitions of a function come before the inherited
    // ones (a call tries them first). A function the followed trail only
    // asks for must be given with what it asks for.
    for mut o in own {
        match methods.iter().position(|m| m.name == o.name) {
            Some(k) => {
                let old = &methods[k];
                for shape in &old.shapes {
                    if let Some(s) = o.shapes.iter().find(|s| s.len() == shape.len()) {
                        if s != shape {
                            errors.push(Diagnostic::error(
                                ErrorKind::DefinitionError,
                                o.span,
                                format!(
                                    "every definition of `{}` with {} parameter(s) must borrow (`@`) the same parameters as in `{}`",
                                    o.name,
                                    shape.len(),
                                    old.from.rsplit('.').next().unwrap_or(&old.from)
                                ),
                            ));
                        }
                    } else if old.func.is_some() {
                        o.shapes.push(shape.clone());
                    }
                }
                if let Some(missing) = old.shapes.iter().find(|s| !o.shapes.contains(s)) {
                    let params: Vec<&str> = missing
                        .iter()
                        .map(|b| if *b { "@x" } else { "x" })
                        .collect();
                    errors.push(Diagnostic::error(
                        ErrorKind::DefinitionError,
                        o.span,
                        format!(
                            "`{}` must take the parameters `{}` asks for: {} {}",
                            o.name,
                            old.from.rsplit('.').next().unwrap_or(&old.from),
                            o.name,
                            params.join(" ")
                        ),
                    ));
                }
                let keep = old.func.clone();
                let then = if o.func.is_some() { keep.clone() } else { None };
                methods[k] = HMethod {
                    func: o.func.clone().or(keep),
                    then,
                    ..o
                };
            }
            None => methods.push(o),
        }
    }
    stack.pop();
    let follows_copy = follows.iter().any(|f| f == "Copy");
    let shows = follows.iter().any(|f| f == "Show");
    built.insert(
        name.to_string(),
        HTrail {
            name: name.to_string(),
            name_span: t.name_span,
            fields,
            follows,
            follows_copy,
            shows,
            methods,
            library: t.library,
        },
    );
}

/// The ids of the variables `e` reads.
fn used_locals(e: &HExpr, out: &mut Vec<LocalId>) {
    let mut go = |e: &HExpr| used_locals(e, out);
    match &e.kind {
        HExprKind::Local(l) => {
            if !out.contains(l) {
                out.push(*l);
            }
        }
        HExprKind::Int(_)
        | HExprKind::Float(_)
        | HExprKind::Str(_)
        | HExprKind::Bool(_)
        | HExprKind::Void => {}
        HExprKind::Call { args, .. } | HExprKind::Array(args) => args.iter().for_each(go),
        HExprKind::Binary { lhs, rhs, .. } | HExprKind::Range(lhs, rhs) => {
            go(lhs);
            go(rhs);
        }
        HExprKind::Neg(x)
        | HExprKind::Echo(x)
        | HExprKind::Builtin(_, x)
        | HExprKind::Field(x, _, _) => go(x),
        HExprKind::Ret(x) => x.iter().for_each(|x| go(x)),
        HExprKind::Comprehension { body, clauses } => {
            for c in clauses {
                match c {
                    HClause::Gen { source, .. } => go(source),
                    HClause::Guard(g) => go(g),
                }
            }
            go(body);
        }
        HExprKind::Check {
            scrutinee,
            arms,
            else_arm,
        } => {
            go(scrutinee);
            for (p, b) in arms {
                go(p);
                go(b);
            }
            if let Some(e) = else_arm {
                go(e);
            }
        }
        HExprKind::Construct { steps, .. } => steps.iter().for_each(|(_, v)| go(v)),
        HExprKind::MethodCall { receiver, args, .. } => {
            go(receiver);
            args.iter().for_each(go);
        }
    }
}

/// A trail in the signature of a function of `trail`: `Self` is the
/// trail itself, as a bound (a trail that follows it, for a follower).
fn method_ty(
    ann: &ast::TypeAnn,
    trail: &str,
    ann_ty: &dyn Fn(&ast::TypeAnn, usize) -> Result<Ty, Diagnostic>,
    trail_name: &dyn Fn(&str, usize) -> Option<String>,
    file: usize,
) -> Result<(Ty, Option<String>), Diagnostic> {
    match &ann.kind {
        ast::TypeAnnKind::Name(n) if n == "Self" => {
            Ok((Ty::record(trail), Some(trail.to_string())))
        }
        ast::TypeAnnKind::Array(inner) => {
            let (t, _) = method_ty(inner, trail, ann_ty, trail_name, file)?;
            Ok((Ty::array(t), None))
        }
        _ => {
            let t = ann_ty(ann, file)?;
            Ok((t, bound_of(ann, trail_name, file)))
        }
    }
}

/// The declared parameter and result trails of every definition.
struct Sigs {
    map: HashMap<*const ast::Def, Sig>,
}

struct Sig {
    /// The trails as written (`Fruit` is `Ty::record("Fruit")`).
    params: Vec<Ty>,
    bounds: Vec<Option<String>>,
    ret: Option<Ty>,
    ret_bound: Option<String>,
}

impl Sigs {
    fn params(&self, def: &ast::Def) -> Vec<Ty> {
        self.map[&(def as *const ast::Def)].params.clone()
    }

    fn ret(&self, def: &ast::Def) -> Option<Ty> {
        self.map[&(def as *const ast::Def)].ret
    }

    fn sig(&self, def: &ast::Def) -> &Sig {
        &self.map[&(def as *const ast::Def)]
    }
}

/// The trail a declared type names, if values of other trails can stand
/// for it: a trail of the program (its followers), or `Show` and `Copy`.
fn bound_of(
    ann: &ast::TypeAnn,
    trail_name: &dyn Fn(&str, usize) -> Option<String>,
    file: usize,
) -> Option<String> {
    match &ann.kind {
        // `Any` takes every value anyway, and `Iterable` only the embedded
        // values that follow it.
        ast::TypeAnnKind::Name(n) if n == "Any" || n == "Iterable" => None,
        ast::TypeAnnKind::Name(n) => trail_name(n, file),
        ast::TypeAnnKind::Array(_) => None,
    }
}

/// What a parameter declared `ty` with `bound` accepts before the bound is
/// checked: the records (whose trail must then follow it), and the
/// embedded values that follow it, as `std.luna` declares.
pub fn accepts(ty: Ty, bound: &Option<String>, embedded: &HashMap<Kind, Vec<String>>) -> Ty {
    let Some(b) = bound else {
        return ty;
    };
    let mut accepted = Ty::ANY_RECORD;
    for (kind, follows) in embedded {
        if follows.contains(b) {
            accepted = accepted.union(match kind {
                Kind::Array => Ty::ARRAY,
                k => Ty::of(*k),
            });
        }
    }
    accepted
}

/// Resolution runs in two passes. Pass 1 collects every definition, so that
/// pass 2 can resolve calls to functions defined *later* in the file (and
/// recursive calls). A single top-to-bottom pass could not resolve
/// `summon` calling `sum` if `sum` came after it.
///
/// Errors are collected rather than returned at the first one, so the user
/// sees every unknown name at once instead of fixing them one per compile.
pub fn resolve(program: &ast::Program) -> Result<(Hir, Vec<Diagnostic>), Vec<Diagnostic>> {
    resolve_files(program, &[])
}

/// `resolve` for a program of several files: `files[def.file]` says how
/// the names in each definition reach the globals.
pub fn resolve_files(
    program: &ast::Program,
    files: &[FileNames],
) -> Result<(Hir, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    // Trails first: signatures and fields name them.
    let def_names: std::collections::HashSet<&str> =
        program.defs.iter().map(|d| d.name.as_str()).collect();
    let mut trail_defs: HashMap<String, &ast::Trail> = HashMap::new();
    // `trail Int follow Numeric, Copy:` in `std.luna`: what the embedded
    // values follow.
    let mut embedded_decls: Vec<&ast::Trail> = Vec::new();
    let library_names: std::collections::HashSet<&str> = program
        .trails
        .iter()
        .filter(|t| t.library)
        .map(|t| t.name.as_str())
        .collect();
    for t in &program.trails {
        if t.library && embedded_kind(&t.name).is_some() {
            embedded_decls.push(t);
            continue;
        }
        let written = t.name.rsplit('.').next().unwrap_or(&t.name);
        let problem = if !t.library && (embedded_trail(written) || library_names.contains(written))
        {
            Some(format!(
                "`{written}` is an embedded trail and cannot be defined again"
            ))
        } else if trail_defs.contains_key(&t.name) {
            Some(format!("the trail `{written}` is defined more than once"))
        } else if def_names.contains(t.name.as_str()) || Builtin::from_name(&t.name).is_some() {
            Some(format!(
                "`{written}` names both a trail and a function; a trail's name must be unique"
            ))
        } else {
            None
        };
        match problem {
            Some(m) => errors.push(Diagnostic::error(
                ErrorKind::DefinitionError,
                t.name_span,
                m,
            )),
            None => {
                trail_defs.insert(t.name.clone(), t);
            }
        }
    }
    let ann_ty = |ann: &ast::TypeAnn, file: usize| resolve_ty(ann, files.get(file), &trail_defs);
    // The trail a name in `file` means, if it names one.
    let trail_name = |n: &str, file: usize| -> Option<String> {
        let global = match files.get(file) {
            None => n.to_string(),
            Some(f) => f.global(n).ok()?,
        };
        trail_defs.contains_key(&global).then_some(global)
    };
    let mut sigs = Sigs {
        map: HashMap::new(),
    };
    for def in &program.defs {
        let mut ann = |t: &ast::TypeAnn| match ann_ty(t, def.file) {
            Ok(ty) => ty,
            Err(e) => {
                errors.push(e);
                Ty::ANY
            }
        };
        let (declared, bounds): (Vec<Ty>, Vec<Option<String>>) = match &def.sig {
            Some(sig) if !sig.params.is_empty() => sig
                .params
                .iter()
                .map(|a| {
                    let t = ann(a);
                    (t, bound_of(a, &trail_name, def.file))
                })
                .unzip(),
            _ => (
                vec![Ty::ANY; def.params.len()],
                vec![None; def.params.len()],
            ),
        };
        let params = declared
            .into_iter()
            .zip(&def.params)
            .map(|(t, p)| match p.pattern {
                Some(_) => t.intersect(Ty::ARRAY),
                None => t,
            })
            .collect();
        let ret_ann = def.sig.as_ref().and_then(|s| s.ret.as_ref());
        let ret = ret_ann.map(&mut ann);
        let ret_bound = ret_ann.and_then(|a| bound_of(a, &trail_name, def.file));
        sigs.map.insert(
            def as *const ast::Def,
            Sig {
                params,
                bounds,
                ret,
                ret_bound,
            },
        );
    }
    let trails = build_trails(&trail_defs, &ann_ty, &trail_name, &mut errors);
    let mut embedded: HashMap<Kind, Vec<String>> = HashMap::new();
    for t in &embedded_decls {
        let mut list: Vec<String> = Vec::new();
        for (p, span) in &t.follows {
            match trails.get(p) {
                Some(followed) => {
                    for f in &followed.follows {
                        if !list.contains(f) {
                            list.push(f.clone());
                        }
                    }
                }
                None => errors.push(Diagnostic::error(
                    ErrorKind::DefinitionError,
                    *span,
                    format!("cannot find the trail `{p}`"),
                )),
            }
        }
        embedded.insert(embedded_kind(&t.name).unwrap(), list);
    }
    // How many arguments the functions of trails take, by name, for call
    // chains such as `t.describe sum xs`.
    let mut method_arities: HashMap<String, Vec<usize>> = HashMap::new();
    for t in trails.values() {
        for m in &t.methods {
            let a = method_arities.entry(m.name.clone()).or_default();
            for shape in &m.shapes {
                if !a.contains(&shape.len()) {
                    a.push(shape.len());
                }
            }
        }
    }
    // Which trails each function's parameters declare, for `{...}` passed
    // as an argument.
    let mut group_params: HashMap<String, Vec<Vec<Ty>>> = HashMap::new();
    for def in &program.defs {
        group_params
            .entry(def.name.clone())
            .or_default()
            .push(sigs.params(def));
    }

    // Pass 1. Collect signatures; a later definition with the same name,
    // arity and parameter types replaces the earlier one (this is how the
    // readme's `foo = ...` then `foo = ...` redefinition works).
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut live: Vec<Option<&ast::Def>> = Vec::new();
    for def in &program.defs {
        if constant(&def.name).is_some() {
            errors.push(Diagnostic::error(
                ErrorKind::DefinitionError,
                def.name_span,
                format!(
                    "`{}` is a built-in constant and cannot be redefined",
                    def.name
                ),
            ));
            continue;
        }
        let tys = sigs.params(def);
        let group = groups.entry(def.name.clone()).or_default();
        if let Some(pos) = group.iter().position(|&i| {
            let other = live[i].unwrap();
            // Definitions with patterns are alternatives, never replacements:
            // `head (x:xs)` and `head []` both take an array.
            other.params.len() == def.params.len()
                && sigs.params(other) == tys
                && sigs.sig(other).bounds == sigs.sig(def).bounds
                && !has_patterns(other)
                && !has_patterns(def)
        }) {
            let old = group[pos];
            warnings.push(Diagnostic::warning(
                def.name_span,
                format!(
                    "`{}` is redefined; this definition replaces the earlier one",
                    def.name
                ),
            ));
            live[old] = None;
            group.remove(pos);
        }
        group.push(live.len());
        live.push(Some(def));
    }

    // Renumber the surviving definitions densely, so a `FuncId` can index
    // straight into `funcs` with no gaps left by replaced definitions.
    let mut ids = vec![usize::MAX; live.len()];
    let mut defs = Vec::new();
    for (i, d) in live.iter().enumerate() {
        if let Some(d) = d {
            ids[i] = defs.len();
            defs.push(*d);
        }
    }
    let groups: HashMap<String, Vec<FuncId>> = groups
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().map(|i| ids[i]).collect()))
        .collect();

    // Calls try definitions from top to bottom (see `typeck`), so a
    // definition is never used when an earlier one with as many parameters
    // accepts everything it does, like `head _` before `head a :: numeric`.
    let mut names: Vec<&String> = groups.keys().collect();
    names.sort_by_key(|n| groups[*n].first().map(|&f| defs[f].name_span.start));
    for group in names.into_iter().map(|n| &groups[n]) {
        for (k, &later) in group.iter().enumerate() {
            let (later_def, later_tys) = (defs[later], sigs.params(defs[later]));
            // A definition with patterns matches only some arrays, so it
            // never hides a later one.
            let covering = group[..k].iter().find(|&&earlier| {
                !has_patterns(defs[earlier])
                    && defs[earlier].params.len() == later_def.params.len()
                    && later_tys
                        .iter()
                        .zip(sigs.params(defs[earlier]))
                        .all(|(l, e)| l.is_subset(e))
            });
            if let Some(&earlier) = covering {
                warnings.push(
                    Diagnostic::warning(
                        later_def.name_span,
                        format!(
                            "this definition of `{}` is never used: an earlier one accepts all of its arguments",
                            later_def.name
                        ),
                    )
                    .with_note(defs[earlier].name_span, "calls always match this one first"),
                );
            }
        }
    }

    // A call does not know in advance which definition it reaches (that
    // can depend on the values), yet it must know whether its arguments
    // move or stay with the caller. So every definition with the same name
    // and number of parameters must borrow the same ones.
    for group in groups.values() {
        for (k, &later) in group.iter().enumerate() {
            let flags = |d: &ast::Def| d.params.iter().map(|p| p.borrowed).collect::<Vec<_>>();
            let later_def = defs[later];
            if let Some(&earlier) = group[..k].iter().find(|&&e| {
                defs[e].params.len() == later_def.params.len() && flags(defs[e]) != flags(later_def)
            }) {
                errors.push(
                    Diagnostic::error(
                        ErrorKind::DefinitionError,
                        later_def.name_span,
                        format!(
                            "every definition of `{}` with {} parameter(s) must borrow (`@`) the same parameters, because a call cannot tell in advance which one it reaches",
                            later_def.name,
                            later_def.params.len()
                        ),
                    )
                    .with_note(defs[earlier].name_span, "this definition borrows differently"),
                );
            }
        }
    }

    // Which argument counts each name accepts. A call with a count no
    // overload takes is an error that can be reported now, before types
    // are known.
    let mut arities: HashMap<&str, Vec<usize>> = HashMap::new();
    for def in &defs {
        arities
            .entry(def.name.as_str())
            .or_default()
            .push(def.params.len());
    }
    for a in arities.values_mut() {
        a.sort();
    }

    // The defaults of trail fields become functions of the fields they use,
    // which they borrow: `textualize = copy name` in Fruit is a function
    // `Fruit$textualize @name`. (`$` keeps these names out of reach.) Which
    // fields a default uses is found first, by resolving it with all the
    // other fields visible, so that every `Fruit {...}` knows how to fill
    // in its defaults.
    let mut trails = trails;
    let mut uses: HashMap<String, Vec<String>> = HashMap::new();
    let mut trail_names: Vec<&String> = trail_defs.keys().collect();
    trail_names.sort();
    for name in &trail_names {
        let t = trail_defs[*name];
        for f in &t.fields {
            let Some(default) = &f.default else {
                continue;
            };
            let fields = &trails[*name].fields;
            let others: Vec<String> = fields
                .iter()
                .map(|g| g.name.clone())
                .filter(|g| *g != f.name)
                .collect();
            let field_ty = fields.iter().find(|g| g.name == f.name).and_then(|g| g.ty);
            let mut r = Resolver {
                arities: &arities,
                names: files.get(t.file),
                trails: &trails,
                method_arities: &method_arities,
                group_params: &group_params,
                member: None,
                default_of: Some(name.to_string()),
                ret: field_ty,
                locals: others.iter().cloned().zip(0..).collect(),
                local_names: others.clone(),
                assigned: Vec::new(),
                fixed: (0..others.len()).collect(),
                errors: Vec::new(),
            };
            let mut used = Vec::new();
            if let Ok(e) = r.expr_in(default, field_ty) {
                used_locals(&e, &mut used);
            }
            let params: Vec<String> = others
                .iter()
                .enumerate()
                .filter(|(k, _)| used.contains(k))
                .map(|(_, n)| n.clone())
                .collect();
            uses.insert(format!("{name}${}", f.name), params);
        }
    }
    for t in trails.values_mut() {
        for f in &mut t.fields {
            if let FieldDefault::Func { name, uses: u } = &mut f.default {
                *u = uses.get(name.as_str()).cloned().unwrap_or_default();
            }
        }
    }

    // Pass 2: resolve every name inside every body.
    let mut funcs = Vec::new();
    for def in &defs {
        // The parameters are the first variables.
        let params: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
        let borrowed: Vec<bool> = def.params.iter().map(|p| p.borrowed).collect();
        let mut r = Resolver {
            arities: &arities,
            names: files.get(def.file),
            trails: &trails,
            method_arities: &method_arities,
            group_params: &group_params,
            member: None,
            default_of: None,
            ret: sigs.ret(def).filter(|t| *t != Ty::VOID),
            locals: params.iter().cloned().zip(0..).collect(),
            local_names: params,
            assigned: Vec::new(),
            fixed: (0..borrowed.len()).filter(|&i| borrowed[i]).collect(),
            errors: Vec::new(),
        };
        // The names a pattern gives the parts of its array are variables too.
        // (The pattern parameter's own name, such as `(x:xs)`, can never be
        // written in the body, so the whole array is only reachable through
        // its parts.)
        let patterns: Vec<Option<HPattern>> = def
            .params
            .iter()
            .map(|p| match &p.pattern {
                None => None,
                Some(ast::Pattern::Empty) => Some(HPattern::Empty),
                Some(ast::Pattern::Cons { heads, rest }) => Some(HPattern::Cons {
                    heads: heads.iter().map(|(n, _)| r.new_local(n)).collect(),
                    rest: r.new_local(&rest.0),
                }),
            })
            .collect();
        let body = r.body(&def.body.stmts);
        errors.append(&mut r.errors);
        funcs.push(Func {
            name: def.name.clone(),
            name_span: def.name_span,
            params: def.params.iter().map(|p| p.name.clone()).collect(),
            param_tys: sigs
                .params(def)
                .into_iter()
                .zip(&sigs.sig(def).bounds)
                .map(|(t, b)| accepts(t, b, &embedded))
                .collect(),
            bounds: sigs.sig(def).bounds.clone(),
            ret_bound: sigs.sig(def).ret_bound.clone(),
            ret: sigs.ret(def),
            body,
            local_names: r.local_names,
            patterns,
            borrowed,
        });
    }
    // Then the default functions themselves.
    let mut groups = groups;
    for name in &trail_names {
        let t = trail_defs[*name];
        for f in &t.fields {
            let Some(default) = &f.default else {
                continue;
            };
            let fields = &trails[*name].fields;
            let field_ty = fields.iter().find(|g| g.name == f.name).and_then(|g| g.ty);
            let fname = format!("{name}${}", f.name);
            let params = uses[&fname].clone();
            let mut r = Resolver {
                arities: &arities,
                names: files.get(t.file),
                trails: &trails,
                method_arities: &method_arities,
                group_params: &group_params,
                member: None,
                default_of: Some(name.to_string()),
                ret: field_ty,
                locals: params.iter().cloned().zip(0..).collect(),
                local_names: params.clone(),
                assigned: Vec::new(),
                fixed: (0..params.len()).collect(),
                errors: Vec::new(),
            };
            let body = match r.expr_in(default, field_ty) {
                Ok(e) => vec![HStmt::Expr(e)],
                Err(e) => {
                    r.errors.push(e);
                    Vec::new()
                }
            };
            errors.append(&mut r.errors);
            let param_tys: Vec<Ty> = params
                .iter()
                .map(|p| {
                    fields
                        .iter()
                        .find(|g| &g.name == p)
                        .and_then(|g| g.ty)
                        .unwrap_or(Ty::ANY)
                })
                .collect();
            groups.entry(fname.clone()).or_default().push(funcs.len());
            let n = params.len();
            funcs.push(Func {
                name: fname,
                name_span: f.span,
                params,
                param_tys,
                bounds: vec![None; n],
                ret_bound: None,
                ret: field_ty,
                body,
                local_names: r.local_names,
                patterns: vec![None; n],
                borrowed: vec![true; n],
            });
        }
    }

    // The functions of trails: each takes the value itself first (borrowed,
    // as the variable `#self`), and sees its trail's fields and functions
    // by name. `Self` in a signature is the trail (or one that follows it).
    for name in &trail_names {
        let t = trail_defs[*name];
        for m in &t.methods {
            let Some(body) = &m.body else {
                continue;
            };
            let mut ann = |a: &ast::TypeAnn| match method_ty(a, name, &ann_ty, &trail_name, t.file)
            {
                Ok(r) => r,
                Err(e) => {
                    errors.push(e);
                    (Ty::ANY, None)
                }
            };
            let (tys, bounds): (Vec<Ty>, Vec<Option<String>>) = match &m.sig {
                Some(sig) if !sig.params.is_empty() => sig.params.iter().map(&mut ann).unzip(),
                _ => (vec![Ty::ANY; m.params.len()], vec![None; m.params.len()]),
            };
            // a pattern parameter takes an array
            let tys: Vec<Ty> = tys
                .into_iter()
                .zip(&m.params)
                .map(|(t, p)| match p.pattern {
                    Some(_) => t.intersect(Ty::ARRAY),
                    None => t,
                })
                .collect();
            let (ret, ret_bound) = match m.sig.as_ref().and_then(|s| s.ret.as_ref()) {
                Some(a) => {
                    let (t, b) = ann(a);
                    (Some(t), b)
                }
                None => (None, None),
            };
            let mut params = vec!["#self".to_string()];
            params.extend(m.params.iter().map(|p| p.name.clone()));
            let mut borrowed = vec![true];
            borrowed.extend(m.params.iter().map(|p| p.borrowed));
            let mut r = Resolver {
                arities: &arities,
                names: files.get(t.file),
                trails: &trails,
                method_arities: &method_arities,
                group_params: &group_params,
                member: Some(name.to_string()),
                default_of: None,
                ret: ret.filter(|t| *t != Ty::VOID),
                locals: params.iter().cloned().zip(0..).collect(),
                local_names: params.clone(),
                assigned: Vec::new(),
                fixed: (0..borrowed.len()).filter(|&i| borrowed[i]).collect(),
                errors: Vec::new(),
            };
            let mut patterns: Vec<Option<HPattern>> = vec![None];
            for p in &m.params {
                patterns.push(match &p.pattern {
                    None => None,
                    Some(ast::Pattern::Empty) => Some(HPattern::Empty),
                    Some(ast::Pattern::Cons { heads, rest }) => Some(HPattern::Cons {
                        heads: heads.iter().map(|(n, _)| r.new_local(n)).collect(),
                        rest: r.new_local(&rest.0),
                    }),
                });
            }
            let body = r.body(&body.stmts);
            errors.append(&mut r.errors);
            let fname = format!("{name}${}", m.name);
            groups.entry(fname.clone()).or_default().push(funcs.len());
            let mut param_tys = vec![Ty::ANY_RECORD];
            param_tys.extend(
                tys.iter()
                    .zip(&bounds)
                    .map(|(t, b)| accepts(*t, b, &embedded)),
            );
            let mut all_bounds = vec![Some(name.to_string())];
            all_bounds.extend(bounds);
            funcs.push(Func {
                name: fname,
                name_span: m.span,
                params,
                param_tys,
                bounds: all_bounds,
                ret_bound,
                ret,
                body,
                local_names: r.local_names,
                patterns,
                borrowed,
            });
        }
    }

    // A trail's own definitions of a function, then the inherited ones.
    fn chain(
        group: &str,
        then: &HashMap<String, String>,
        own: &HashMap<String, Vec<FuncId>>,
        seen: &mut Vec<String>,
    ) -> Vec<FuncId> {
        if seen.iter().any(|s| s == group) {
            return Vec::new();
        }
        seen.push(group.to_string());
        let mut out = own.get(group).cloned().unwrap_or_default();
        if let Some(next) = then.get(group) {
            out.extend(chain(next, then, own, seen));
        }
        out
    }
    let then: HashMap<String, String> = trails
        .values()
        .flat_map(|t| &t.methods)
        .filter_map(|m| Some((m.func.clone()?, m.then.clone()?)))
        .collect();
    let own_groups: HashMap<String, Vec<FuncId>> = then
        .keys()
        .chain(then.values())
        .filter_map(|g| Some((g.clone(), groups.get(g)?.clone())))
        .collect();
    for g in then.keys() {
        let all = chain(g, &then, &own_groups, &mut Vec::new());
        groups.insert(g.clone(), all);
    }

    // A function whose definitions take arrays apart with `(x:xs)` but have
    // nothing for an empty array would stop the program when called with
    // one: warn about it.
    for def in &defs {
        for (j, p) in def.params.iter().enumerate() {
            if !matches!(p.pattern, Some(ast::Pattern::Cons { .. })) {
                continue;
            }
            let empty_handled = defs.iter().any(|other| {
                other.name == def.name
                    && other.params.len() == def.params.len()
                    && match &other.params[j].pattern {
                        Some(ast::Pattern::Empty) => true,
                        Some(_) => false,
                        None => sigs.params(other)[j].contains(crate::types::Kind::Array),
                    }
            });
            if !empty_handled {
                warnings.push(Diagnostic::warning(
                    p.span,
                    format!(
                        "no definition of `{}` accepts an empty array here, so calling it with one stops the program; add one such as `{} [] = ...`",
                        def.name, def.name
                    ),
                ));
                break;
            }
        }
    }

    let main = groups
        .get("main")
        .and_then(|g| g.iter().copied().find(|&f| funcs[f].params.is_empty()));
    let main = match main {
        Some(m) => m,
        None => {
            let span = program
                .defs
                .first()
                .map(|d| d.name_span)
                .unwrap_or_default();
            errors.push(Diagnostic {
                severity: crate::diag::Severity::Error,
                kind: Some(ErrorKind::DefinitionError),
                message:
                    "the program has no `main` function (define `main = ...` with no arguments)"
                        .into(),
                span: if program.defs.is_empty() {
                    None
                } else {
                    Some(span)
                },
                notes: Vec::new(),
            });
            usize::MAX
        }
    };

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok((
        Hir {
            funcs,
            groups,
            main,
            trails,
            embedded,
        },
        warnings,
    ))
}

/// The declared parameter types: from the signature, or `any`; a pattern
/// parameter (`(x:xs)`, `[]`) takes an array.
fn has_patterns(def: &ast::Def) -> bool {
    def.params.iter().any(|p| p.pattern.is_some())
}

/// Resolves the names in one function body.
struct Resolver<'a> {
    arities: &'a HashMap<&'a str, Vec<usize>>,
    /// How this file's names reach the globals, in a program of several
    /// files.
    names: Option<&'a FileNames>,
    trails: &'a HashMap<String, HTrail>,
    /// How many arguments the functions of trails take, by name.
    method_arities: &'a HashMap<String, Vec<usize>>,
    /// The declared parameter trails of each function's definitions.
    group_params: &'a HashMap<String, Vec<Vec<Ty>>>,
    /// In a function of a trail: the trail, whose fields and functions
    /// are visible by name (on the value itself, the variable `#self`, 0).
    member: Option<String>,
    /// In a field's default: the trail, whose functions the default
    /// cannot call (the value is not made yet).
    default_of: Option<String>,
    /// The function's declared result, which a `{...}` it gives (as its last
    /// expression or with `ret`) is a value of.
    ret: Option<Ty>,
    /// Visible variables, innermost last. This stack is the "symbol table"
    /// of the classic textbook: a loop body pushes its variables and pops
    /// them when it ends, so they are not visible after the loop.
    locals: Vec<(String, LocalId)>,
    /// Name of each variable by id.
    local_names: Vec<String>,
    /// Every variable given a value so far, in order; a loop looks at the
    /// part written while resolving its body.
    assigned: Vec<LocalId>,
    /// Variables that hold something borrowed: `@` parameters and the
    /// variables of loops over `@source`. They cannot get a new value.
    fixed: Vec<LocalId>,
    errors: Vec<Diagnostic>,
}

impl<'a> Resolver<'a> {
    /// A function body: its last expression is the result, so a `{...}`
    /// there is a value of the declared result trail.
    fn body(&mut self, stmts: &[Stmt]) -> Vec<HStmt> {
        let Some((last, rest)) = stmts.split_last() else {
            return Vec::new();
        };
        let mut out = self.stmts(rest);
        match last {
            Stmt::Expr(e) => match self.expr_in(e, self.ret) {
                Ok(e) => out.push(HStmt::Expr(e)),
                Err(e) => self.errors.push(e),
            },
            _ => out.extend(self.stmts(std::slice::from_ref(last))),
        }
        out
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> Vec<HStmt> {
        let mut out = Vec::new();
        for stmt in stmts {
            match self.stmt(stmt) {
                Ok(s) => out.push(s),
                Err(e) => self.errors.push(e),
            }
        }
        out
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<HStmt, Diagnostic> {
        match stmt {
            Stmt::Let { name, span, .. } if constant(name).is_some() => Err(Diagnostic::error(
                ErrorKind::DefinitionError,
                *span,
                format!("`{name}` is a built-in constant and cannot be given a new value"),
            )),
            Stmt::Let { name, span, value } => {
                // The value is resolved first, so in a first binding such as
                // `x = x * 2` the right-hand `x` cannot be the new variable.
                let value = self.expr(value)?;
                let id = match self.lookup_local(name) {
                    Some(id) => id,
                    None => self.new_local(name),
                };
                if self.fixed.contains(&id) {
                    return Err(Diagnostic::error(
                        ErrorKind::OwnershipError,
                        *span,
                        format!(
                            "`{name}` is borrowed, so it cannot get a new value: it still belongs to the caller (or to what the loop goes through)"
                        ),
                    ));
                }
                self.assigned.push(id);
                Ok(HStmt::Let { local: id, value })
            }
            Stmt::Expr(e) => Ok(HStmt::Expr(self.expr(e)?)),
            Stmt::Loop {
                pos,
                var,
                var_span,
                source,
                borrowed,
                body,
            } => {
                let source = self.expr(source)?;
                // Variables created from here on belong to the body.
                let first_inner = self.local_names.len();
                let visible = self.locals.len();
                let assigned = self.assigned.len();
                let pos = pos.as_ref().map(|(p, span)| (self.new_local(p), *span));
                let var = self.new_local(var);
                if *borrowed {
                    self.fixed.push(var);
                }
                let body = self.stmts(body);
                self.locals.truncate(visible);
                let mut carried: Vec<LocalId> = self.assigned[assigned..]
                    .iter()
                    .copied()
                    .filter(|&id| id < first_inner)
                    .collect();
                carried.sort();
                carried.dedup();
                Ok(HStmt::Loop {
                    pos,
                    var,
                    var_span: *var_span,
                    source,
                    borrowed: *borrowed,
                    body,
                    carried,
                    inner: (first_inner..self.local_names.len()).collect(),
                })
            }
        }
    }

    fn clauses(&mut self, clauses: &[ast::Clause]) -> Result<Vec<HClause>, Diagnostic> {
        let mut out = Vec::new();
        for c in clauses {
            out.push(match c {
                ast::Clause::Gen {
                    pos,
                    var,
                    source,
                    borrowed,
                    ..
                } => {
                    let source = self.expr(source)?;
                    HClause::Gen {
                        pos: pos.as_ref().map(|(p, _)| self.new_local(p)),
                        var: self.new_local(var),
                        source,
                        borrowed: *borrowed,
                    }
                }
                ast::Clause::Guard(g) => HClause::Guard(self.expr(g)?),
            });
        }
        Ok(out)
    }

    fn new_local(&mut self, name: &str) -> LocalId {
        let id = self.local_names.len();
        self.local_names.push(name.to_string());
        self.locals.push((name.to_string(), id));
        id
    }

    /// Look a variable up from the innermost scope outward. Globals are
    /// checked by the caller, last. That order is the "name shadow" rule
    /// from the readme.
    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    fn lookup_value(&self, name: &str) -> Option<HExprKind> {
        self.lookup_local(name).map(HExprKind::Local)
    }

    /// The global name `written` refers to: itself, except in a program of
    /// several files (see `FileNames`).
    fn global(&self, written: &str, span: Span) -> Result<String, Diagnostic> {
        match self.names {
            None => Ok(written.to_string()),
            Some(names) => names
                .global(written)
                .map_err(|m| Diagnostic::error(ErrorKind::SymbolNotFoundError, span, m)),
        }
    }

    /// How many arguments `name` takes, if it names a function: a
    /// variable is a value, and a constant or unknown name is no function.
    fn function_arities(&self, name: &str) -> Option<Vec<usize>> {
        if self.lookup_local(name).is_some() {
            return None;
        }
        // a function of the trail, inside it
        if self.member_has(name, true) {
            return self.method_arities.get(name).cloned();
        }
        if let Ok(global) = self.global(name, Span::default()) {
            if let Some(a) = self.arities.get(global.as_str()) {
                return Some(a.clone());
            }
            if builtin_of(&global).is_some() {
                return Some(vec![1]);
            }
        }
        // `t.describe`: a function of a value
        let (_, method) = name.rsplit_once('.')?;
        self.method_arities.get(method).cloned()
    }

    /// Arity-directed grouping of `callee arg1 arg2 ...` when the flat
    /// argument count fits no definition. Reading left to right, a
    /// function name among the arguments takes the arguments after it, as
    /// many as it accepts, so `echo sum xs` is `echo (sum xs)` and
    /// `f g x y` with one-argument `f` and two-argument `g` is
    /// `f (g x y)`. Returns the regrouped call, or `None` if no grouping
    /// works (the arity error is reported as before); several groupings
    /// are an error asking for parentheses.
    fn group_call(
        &self,
        callee: &str,
        callee_span: Span,
        args: &[ast::Expr],
        arities: &[usize],
    ) -> Result<Option<ast::Expr>, Diagnostic> {
        let mut found = Vec::new();
        for &n in arities {
            for (end, parts) in self.take_args(args, 0, n) {
                if end == args.len() {
                    found.push(parts);
                }
            }
        }
        match found.len() {
            0 => Ok(None),
            1 => {
                let parts = found.pop().unwrap();
                let span = callee_span.to(args.last().unwrap().span);
                Ok(Some(ast::Expr {
                    kind: ExprKind::Call {
                        callee: callee.to_string(),
                        callee_span,
                        args: parts,
                    },
                    span,
                }))
            }
            _ => Err(Diagnostic::error(
                ErrorKind::TypeError,
                callee_span.to(args.last().unwrap().span),
                format!(
                    "the arguments of `{callee}` can be grouped in more than one way; add parentheses to show which"
                ),
            )),
        }
    }

    /// The ways to read `n` arguments from `args[i..]`: where they end, and
    /// the argument expressions.
    fn take_args(&self, args: &[ast::Expr], i: usize, n: usize) -> Vec<(usize, Vec<ast::Expr>)> {
        if n == 0 {
            return vec![(i, Vec::new())];
        }
        let mut ways = Vec::new();
        for (next, first) in self.take_one(args, i) {
            for (end, mut rest) in self.take_args(args, next, n - 1) {
                rest.insert(0, first.clone());
                ways.push((end, rest));
            }
        }
        ways
    }

    /// The ways to read one argument starting at `args[i]`: the atom
    /// itself, or, for a function name, a call taking the following
    /// arguments.
    fn take_one(&self, args: &[ast::Expr], i: usize) -> Vec<(usize, ast::Expr)> {
        let Some(atom) = args.get(i) else {
            return Vec::new();
        };
        let ExprKind::Name(name) = &atom.kind else {
            return vec![(i + 1, atom.clone())];
        };
        let Some(arities) = self.function_arities(name) else {
            return vec![(i + 1, atom.clone())];
        };
        let mut ways = Vec::new();
        for k in arities {
            if k == 0 {
                // a function without arguments is used as a value
                ways.push((i + 1, atom.clone()));
                continue;
            }
            for (end, parts) in self.take_args(args, i + 1, k) {
                let span = atom.span.to(parts.last().unwrap().span);
                ways.push((
                    end,
                    ast::Expr {
                        kind: ExprKind::Call {
                            callee: name.clone(),
                            callee_span: atom.span,
                            args: parts,
                        },
                        span,
                    },
                ));
            }
        }
        ways
    }

    /// Check that `name` names a function taking `nargs` arguments, and
    /// give its global name.
    fn check_callable(&self, name: &str, nargs: usize, span: Span) -> Result<String, Diagnostic> {
        let global = self.global(name, span)?;
        if !self.arities.contains_key(global.as_str()) && builtin_of(&global).is_some() {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!("`{name}` is called with {nargs} argument(s), but it takes 1"),
            ));
        }
        let Some(arities) = self.arities.get(global.as_str()) else {
            return Err(Diagnostic::error(
                ErrorKind::SymbolNotFoundError,
                span,
                format!("cannot find symbol `{name}`"),
            ));
        };
        if !arities.contains(&nargs) {
            let mut list: Vec<String> = arities.iter().map(|a| a.to_string()).collect();
            list.dedup();
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!(
                    "`{name}` is called with {nargs} argument(s), but it takes {}",
                    list.join(" or ")
                ),
            ));
        }
        Ok(global)
    }

    fn expr(&mut self, e: &ast::Expr) -> Result<HExpr, Diagnostic> {
        self.expr_in(e, None)
    }

    /// Resolve `e`, a value of `expect` if that is known: what lets a bare
    /// `{...}` know its trail. It is the declared result of the function
    /// for its last expression and `ret`, a parameter's declared trail for
    /// an argument, a field's for its value, and it reaches into the arms
    /// of a `check` and the elements of an array.
    fn expr_in(&mut self, e: &ast::Expr, expect: Option<Ty>) -> Result<HExpr, Diagnostic> {
        let kind = match &e.kind {
            ExprKind::Int(v) => HExprKind::Int(*v),
            ExprKind::Float(v) => HExprKind::Float(*v),
            ExprKind::Str(s) => HExprKind::Str(s.clone()),
            ExprKind::Bool(b) => HExprKind::Bool(*b),
            ExprKind::Void => HExprKind::Void,
            // `t.name`: a field of the variable `t`, or a definition of the
            // file linked as `t` (and fields of what that gives).
            ExprKind::Name(name)
                if name.contains('.') && !self.arities.contains_key(name.as_str()) =>
            {
                return self.dotted(name, e.span);
            }
            // A bare name that is not a local or parameter must be a global.
            // Globals are functions, so `foo` is a call with no arguments.
            // This is how the readme's "sum = 1 is a function that always
            // returns 1" works.
            ExprKind::Name(name) => match self.lookup_value(name) {
                Some(k) => k,
                // Inside a function of a trail: a field of the value itself.
                None if self.member_has(name, false) => HExprKind::Field(
                    Box::new(HExpr {
                        kind: HExprKind::Local(0),
                        span: e.span,
                    }),
                    name.clone(),
                    e.span,
                ),
                None if self.member_has(name, true) => {
                    return Err(Diagnostic::error(
                        ErrorKind::TypeError,
                        e.span,
                        format!("`{name}` is a function of this trail: give it its arguments"),
                    ));
                }
                None if !self.arities.contains_key(name.as_str()) && constant(name).is_some() => {
                    HExprKind::Float(constant(name).unwrap())
                }
                None => self.global_value(name, e.span)?,
            },
            ExprKind::Call {
                callee,
                callee_span,
                args,
            } => {
                if self.lookup_value(callee).is_some() {
                    return Err(Diagnostic::error(
                        ErrorKind::TypeError,
                        *callee_span,
                        format!("`{callee}` is a value, not a function, and cannot take arguments"),
                    ));
                }
                // More arguments than any definition takes: `echo sum xs`,
                // `head tail a`. Group them by how many arguments each
                // function takes.
                if let Some(arities) = self.function_arities(callee)
                    && !arities.contains(&args.len())
                    && let Some(grouped) = self.group_call(callee, *callee_span, args, &arities)?
                {
                    return self.expr_in(&grouped, expect);
                }
                if let Some(t) = &self.default_of
                    && self.lookup_local(callee).is_none()
                    && self.trails[t].methods.iter().any(|m| &m.name == callee)
                {
                    return Err(Diagnostic::error(
                        ErrorKind::TypeError,
                        *callee_span,
                        format!(
                            "a field's default cannot call `{callee}`, a function of the trail: the value is not made yet"
                        ),
                    ));
                }
                // Inside a function of a trail: another function of the
                // value itself.
                if self.member_has(callee, true) {
                    return self.method_call(
                        HExpr {
                            kind: HExprKind::Local(0),
                            span: *callee_span,
                        },
                        callee,
                        *callee_span,
                        args,
                        e.span,
                    );
                }
                // `t.describe 1`: a function of the value `t` (unless `t` is
                // a linked file, whose definition `describe` this is).
                // (A global's own name may have a dot: the standard
                // library's `std.sum`.)
                if let Some((before, method)) = callee.rsplit_once('.')
                    && !self.arities.contains_key(callee.as_str())
                {
                    let first = before.split('.').next().unwrap();
                    let is_file = self.names.is_some_and(|n| n.files.contains_key(first))
                        && self.lookup_local(first).is_none()
                        && !before.contains('.');
                    // Neither a variable nor a global: perhaps a file that
                    // is not linked, which the file's names explain.
                    if self.lookup_local(first).is_none()
                        && !self.member_has(first, false)
                        && self.global(first, *callee_span).is_ok_and(|g| {
                            !self.arities.contains_key(g.as_str()) && constant(&g).is_none()
                        })
                    {
                        self.global(callee, *callee_span)?;
                    }
                    if !is_file {
                        let receiver = self.expr(&ast::Expr {
                            kind: ExprKind::Name(before.to_string()),
                            span: *callee_span,
                        })?;
                        return self.method_call(receiver, method, *callee_span, args, e.span);
                    }
                }
                // `Fruit {...}`: a value of the trail `Fruit`.
                if let Ok(global) = self.global(callee, *callee_span)
                    && self.trails.contains_key(&global)
                {
                    return match &args[..] {
                        [
                            ast::Expr {
                                kind: ExprKind::Record(fields),
                                span,
                            },
                        ] => Ok(HExpr {
                            kind: self.construct(&global, fields, *span)?,
                            span: e.span,
                        }),
                        _ => Err(Diagnostic::error(
                            ErrorKind::TypeError,
                            e.span,
                            format!(
                                "`{callee}` is a trail: write its fields in braces, `{callee} {{...}}`"
                            ),
                        )),
                    };
                }
                let global = self.global(callee, *callee_span)?;
                if !self.arities.contains_key(global.as_str())
                    && let Some(b) = builtin_of(&global)
                {
                    let [arg] = &args[..] else {
                        return Err(Diagnostic::error(
                            ErrorKind::TypeError,
                            *callee_span,
                            format!(
                                "`{callee}` is called with {} argument(s), but it takes 1",
                                args.len()
                            ),
                        ));
                    };
                    return Ok(HExpr {
                        kind: HExprKind::Builtin(b, Box::new(self.expr(arg)?)),
                        span: e.span,
                    });
                }
                let name = self.check_callable(callee, args.len(), *callee_span)?;
                // An argument is a value of its parameter's trail when every
                // definition taking this many arguments declares the same.
                let declared: Vec<Option<Ty>> = (0..args.len())
                    .map(|j| {
                        let defs: Vec<&Vec<Ty>> = self
                            .group_params
                            .get(&name)
                            .map(|g| g.iter().filter(|p| p.len() == args.len()).collect())
                            .unwrap_or_default();
                        let first = defs.first().map(|p| p[j])?;
                        defs.iter().all(|p| p[j] == first).then_some(first)
                    })
                    .collect();
                let args = args
                    .iter()
                    .zip(declared)
                    .map(|(a, t)| self.expr_in(a, t))
                    .collect::<Result<_, _>>()?;
                HExprKind::Call { name, args }
            }
            ExprKind::Binary { op, lhs, rhs } => HExprKind::Binary {
                op: *op,
                lhs: Box::new(self.expr(lhs)?),
                rhs: Box::new(self.expr(rhs)?),
            },
            ExprKind::Neg(inner) => HExprKind::Neg(Box::new(self.expr(inner)?)),
            ExprKind::Ret(value) => HExprKind::Ret(match value {
                Some(v) => Some(Box::new(self.expr_in(v, self.ret)?)),
                None => None,
            }),
            ExprKind::Array(items) => {
                let elem = expect.and_then(|t| t.elem());
                HExprKind::Array(
                    items
                        .iter()
                        .map(|i| self.expr_in(i, elem))
                        .collect::<Result<_, _>>()?,
                )
            }
            ExprKind::Range(start, end) => {
                HExprKind::Range(Box::new(self.expr(start)?), Box::new(self.expr(end)?))
            }
            ExprKind::Comprehension { body, clauses } => {
                // Each clause sees the variables of the generators before
                // it; the body sees them all. None exist after the brackets.
                let visible = self.locals.len();
                let elem = expect.and_then(|t| t.elem());
                let resolved = self.clauses(clauses).and_then(|clauses| {
                    let body = self.expr_in(body, elem)?;
                    Ok(HExprKind::Comprehension {
                        body: Box::new(body),
                        clauses,
                    })
                });
                self.locals.truncate(visible);
                resolved?
            }
            ExprKind::Echo(inner) => HExprKind::Echo(Box::new(self.expr(inner)?)),
            ExprKind::Check {
                scrutinee,
                arms,
                else_arm,
            } => HExprKind::Check {
                scrutinee: Box::new(self.expr(scrutinee)?),
                arms: arms
                    .iter()
                    .map(|a| Ok((self.expr(&a.pattern)?, self.expr_in(&a.body, expect)?)))
                    .collect::<Result<_, Diagnostic>>()?,
                else_arm: match else_arm {
                    Some(e) => Some(Box::new(self.expr_in(e, expect)?)),
                    None => None,
                },
            },
            // A bare `{...}` is a value of the trail expected here.
            ExprKind::Record(fields) => match expect.and_then(|t| t.trail()) {
                Some(trail) => self.construct(trail, fields, e.span)?,
                None => {
                    return Err(Diagnostic::error(
                        ErrorKind::TypeError,
                        e.span,
                        "this `{...}` needs the name of its trail, like `Fruit {...}`: nothing here says which trail it is",
                    ));
                }
            },
            ExprKind::Field(base, name, span) => {
                HExprKind::Field(Box::new(self.expr(base)?), name.clone(), *span)
            }
            ExprKind::Method {
                receiver,
                name,
                name_span,
                args,
            } => {
                let receiver = self.expr(receiver)?;
                return self.method_call(receiver, name, *name_span, args, e.span);
            }
        };
        Ok(HExpr { kind, span: e.span })
    }

    /// Inside a function of a trail, whether `name` is one of its fields
    /// (or, with `function`, one of its functions) and not a variable.
    fn member_has(&self, name: &str, function: bool) -> bool {
        let Some(trail) = &self.member else {
            return false;
        };
        if self.lookup_local(name).is_some() {
            return false;
        }
        let t = &self.trails[trail];
        if function {
            t.methods.iter().any(|m| m.name == name)
        } else {
            t.fields.iter().any(|f| f.name == name)
        }
    }

    fn method_call(
        &mut self,
        receiver: HExpr,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let args = args
            .iter()
            .map(|a| self.expr(a))
            .collect::<Result<_, _>>()?;
        Ok(HExpr {
            kind: HExprKind::MethodCall {
                receiver: Box::new(receiver),
                name: name.to_string(),
                name_span,
                args,
            },
            span,
        })
    }

    /// The value of the global `name` (a call without arguments). A trail
    /// is not a value.
    fn global_value(&self, name: &str, span: Span) -> Result<HExprKind, Diagnostic> {
        if let Ok(global) = self.global(name, span)
            && self.trails.contains_key(&global)
        {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!("`{name}` is a trail, not a value: make one with `{name} {{...}}`"),
            ));
        }
        Ok(HExprKind::Call {
            name: self.check_callable(name, 0, span)?,
            args: Vec::new(),
        })
    }

    /// `a.b.c`: fields of the variable `a`; or, if `a` is a linked file,
    /// fields of its definition `b`; or else fields of the global `a`.
    fn dotted(&mut self, name: &str, span: Span) -> Result<HExpr, Diagnostic> {
        let parts: Vec<&str> = name.split('.').collect();
        let is_file = self.names.is_some_and(|n| n.files.contains_key(parts[0]));
        let (mut e, rest) = if let Some(l) = self.lookup_local(parts[0]) {
            (HExprKind::Local(l), &parts[1..])
        } else if is_file {
            let qualified = format!("{}.{}", parts[0], parts[1]);
            (self.global_value(&qualified, span)?, &parts[2..])
        } else {
            (self.global_value(parts[0], span)?, &parts[1..])
        };
        for field in rest {
            e = HExprKind::Field(Box::new(HExpr { kind: e, span }), field.to_string(), span);
        }
        Ok(HExpr { kind: e, span })
    }

    /// A value of `trail` with the fields written in `fields` (see
    /// `HExprKind::Construct`).
    fn construct(
        &mut self,
        trail: &str,
        fields: &[(String, Span, ast::Expr)],
        span: Span,
    ) -> Result<HExprKind, Diagnostic> {
        let t = &self.trails[trail];
        let shown = trail.rsplit('.').next().unwrap_or(trail).to_string();
        if t.library {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!(
                    "`{shown}` is an embedded trail: it has no values of its own, other trails follow it"
                ),
            ));
        }
        if let Some(m) = t.methods.iter().find(|m| m.func.is_none()) {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!(
                    "a `{shown}` cannot be made: it does not give the function `{}`, which `{}` asks for",
                    m.name,
                    m.from.rsplit('.').next().unwrap_or(&m.from)
                ),
            ));
        }
        let mut slots: Vec<Option<LocalId>> = vec![None; t.fields.len()];
        let mut steps = Vec::new();
        for (name, fspan, value) in fields {
            let Some(k) = t.fields.iter().position(|f| &f.name == name) else {
                return Err(Diagnostic::error(
                    ErrorKind::TypeError,
                    *fspan,
                    format!("`{shown}` has no field `{name}`, so a value cannot give one"),
                ));
            };
            let ty = t.fields[k].ty;
            let v = self.expr_in(value, ty)?;
            let l = self.new_local(&format!("#{name}"));
            steps.push((l, v));
            slots[k] = Some(l);
        }
        let t = &self.trails[trail];
        if let Some(f) = t
            .fields
            .iter()
            .zip(&slots)
            .find(|(f, l)| l.is_none() && matches!(f.default, FieldDefault::Required))
        {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!(
                    "every `{shown}` needs a value for `{}`, which has no default",
                    f.0.name
                ),
            ));
        }
        // The defaults, each once the fields it uses have their values.
        let index = |n: &str| t.fields.iter().position(|f| f.name == n).unwrap();
        while let Some(k) = (0..t.fields.len()).find(|&k| {
            slots[k].is_none()
                && match &t.fields[k].default {
                    FieldDefault::Func { uses, .. } => {
                        uses.iter().all(|u| slots[index(u)].is_some())
                    }
                    _ => true,
                }
        }) {
            let field = &t.fields[k];
            let value = match &field.default {
                FieldDefault::Func { name, uses } => HExprKind::Call {
                    name: name.clone(),
                    args: uses
                        .iter()
                        .map(|u| HExpr {
                            kind: HExprKind::Local(slots[index(u)].unwrap()),
                            span,
                        })
                        .collect(),
                },
                _ => HExprKind::Void,
            };
            let l = self.new_local(&format!("#{}", field.name));
            steps.push((l, HExpr { kind: value, span }));
            slots[k] = Some(l);
        }
        let waiting: Vec<String> = t
            .fields
            .iter()
            .zip(&slots)
            .filter(|(_, l)| l.is_none())
            .map(|(f, _)| format!("`{}`", f.name))
            .collect();
        if !waiting.is_empty() {
            return Err(Diagnostic::error(
                ErrorKind::TypeError,
                span,
                format!(
                    "the defaults of {} use each other, so every `{shown}` must give one of them",
                    waiting.join(" and ")
                ),
            ));
        }
        Ok(HExprKind::Construct {
            trail: trail.to_string(),
            steps,
            slots: slots.into_iter().map(|l| l.unwrap()).collect(),
        })
    }
}
