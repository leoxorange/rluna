//! The compilation pipeline:
//!
//! ```text
//! .luna --lex--> tokens --parse--> AST --resolve--> HIR --typeck--> TIR
//!       --ownership check--> TIR --fold constant globals--> TIR
//!       --lower--> LIR (+ runtime LIR) --codegen--> machine code --link--> executable
//! ```
//!
//! The machine code and executable format depend on the `Target`:
//! x86-64 + PE for Windows, AArch64 + Mach-O for macOS.
//!
//! The pipeline splits into a *frontend* (source → LIR, knows Luna but no
//! machine) and a *backend* (LIR → executable, knows the machine but not
//! Luna). LIR is the contract between them, which is why a `.lir` file can
//! skip the frontend entirely and go straight to the backend.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use crate::codegen;
use crate::diag::{Diagnostic, ErrorKind, SourceFile, Sources, Span};
use crate::link::{Image, macho, pe};
use crate::lir::{self, Module};
use crate::target::Target;
use crate::{ast, consteval, hir, lexer, lower, ownership, parser, typeck};

/// The OS-independent part of the runtime library, compiled into every
/// program together with the target's OS layer.
pub const RUNTIME_CORE_LIR: &str = include_str!("runtime/core.lir");
pub const RUNTIME_ENTRY: &str = "rt.start";

pub struct Options<'a> {
    pub target: Target,
    /// Produce an assembly listing.
    pub want_asm: bool,
    /// Name of the program, recorded in the executable where the format
    /// has a place for it (the macOS code signature).
    pub name: &'a str,
    /// Reads the files the program links (see `frontend_files`); without
    /// it, a program cannot link files.
    pub files: Option<Loader<'a>>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            target: Target::default(),
            want_asm: false,
            name: "luna",
            files: None,
        }
    }
}

#[derive(Default)]
pub struct Artifacts {
    pub ast: Option<String>,
    pub ir: Option<String>,
    pub asm: Option<String>,
    pub exe: Option<Vec<u8>>,
    pub warnings: Vec<String>,
}

pub struct CompileError {
    pub message: String,
}

fn render_all(sources: &Sources, diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| sources.render(d))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The standard library, written in Luna. It is compiled together with
/// every program, like the runtime; only the functions a program uses are
/// compiled to machine code.
pub const STD_LUNA: &str = include_str!("std.luna");

/// Reads the files a program links: given a path below the main file's
/// folder (`entity/try.luna`), its text, or `None` if there is no such file.
pub type Loader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Compile Luna source to an LIR module (without the runtime). The program
/// cannot link other files.
pub fn frontend(src: &SourceFile, artifacts: &mut Artifacts) -> Result<Module, CompileError> {
    frontend_files(src, &|_| None, artifacts)
}

/// One file of a program.
struct File {
    /// Its path below the main file's folder: `main.luna`, `entity/try.luna`.
    path: String,
    source: SourceFile,
    base: usize,
    program: ast::Program,
    /// For each link, the file it links (an index into the files).
    targets: Vec<usize>,
}

impl File {
    /// What its definitions are called globally: the main file's by their
    /// own names, the others' after their file (`entity/try.pred`).
    fn global(&self, index: usize, name: &str) -> String {
        if index == 0 {
            name.to_string()
        } else {
            format!("{}.{name}", self.path.trim_end_matches(".luna"))
        }
    }
}

/// Compile a Luna program to an LIR module (without the runtime). The main
/// file `src` can link other files (`| entity`), which `load` reads.
///
/// All files are read first, following the links from the main file (each
/// file once, so files may link each other in any way). Their definitions
/// then become one program: those of linked files get global names after
/// their file (`entity/try.pred`), so two files can define the same name,
/// and each file gets a table of what its names mean (`hir::FileNames`).
/// From there on, the compiler sees one program.
pub fn frontend_files(
    src: &SourceFile,
    load: Loader,
    artifacts: &mut Artifacts,
) -> Result<Module, CompileError> {
    let main_path = std::path::Path::new(&src.name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "main.luna".into());
    let folder = std::path::Path::new(&src.name)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let mut files: Vec<File> = Vec::new();
    // Every file gets its own range of byte offsets (see `diag::Sources`).
    let mut next_base = 0;
    let mut add = |files: &mut Vec<File>, path: String, source: SourceFile| {
        let base = next_base;
        next_base += source.text.len() + 1;
        let fail = |files: &[File], source: &SourceFile, d: Diagnostic| {
            let mut all: Vec<(usize, &SourceFile)> =
                files.iter().map(|f| (f.base, &f.source)).collect();
            all.push((base, source));
            CompileError {
                message: Sources::new(all).render(&d),
            }
        };
        let tokens = lexer::lex_at(&source.text, base).map_err(|d| fail(files, &source, d))?;
        let program = parser::parse(tokens).map_err(|d| fail(files, &source, d))?;
        files.push(File {
            path,
            source,
            base,
            program,
            targets: Vec::new(),
        });
        Ok::<usize, CompileError>(files.len() - 1)
    };
    add(
        &mut files,
        main_path,
        SourceFile::new(src.name.clone(), src.text.clone()),
    )?;
    artifacts.ast = Some(ast::dump(&files[0].program));

    // Follow the links. A link `| entity` means `entity.luna` or, for a
    // folder of the same name, `entity/entity.luna`.
    let mut i = 0;
    while i < files.len() {
        for k in 0..files[i].program.links.len() {
            let link = &files[i].program.links[k];
            let span = link.path_span;
            let joined = link.path.join("/");
            let candidates = [
                format!("{joined}.luna"),
                format!("{joined}/{}.luna", link.path.last().unwrap()),
            ];
            let mut found: Vec<usize> = Vec::new();
            for path in &candidates {
                if let Some(j) = files.iter().position(|f| &f.path == path) {
                    found.push(j);
                } else if let Some(text) = load(path) {
                    let shown = folder.join(path).display().to_string();
                    found.push(add(&mut files, path.clone(), SourceFile::new(shown, text))?);
                }
            }
            let error = |files: &[File], msg: String| {
                let all = files.iter().map(|f| (f.base, &f.source)).collect();
                CompileError {
                    message: Sources::new(all).render(&Diagnostic::error(
                        ErrorKind::SymbolNotFoundError,
                        span,
                        msg,
                    )),
                }
            };
            match found[..] {
                [j] => files[i].targets.push(j),
                [] => {
                    return Err(error(
                        &files,
                        format!(
                            "cannot find the file `{}` or `{}` to link (paths start at the folder of the main file)",
                            candidates[0], candidates[1]
                        ),
                    ));
                }
                _ => {
                    return Err(error(
                        &files,
                        format!(
                            "both `{}` and `{}` exist; move or rename one of them so that `{joined}` names one file",
                            candidates[0], candidates[1]
                        ),
                    ));
                }
            }
        }
        i += 1;
    }

    let std = SourceFile::new("std.luna", STD_LUNA);
    let std_base = next_base;
    let mut all: Vec<(usize, &SourceFile)> = files.iter().map(|f| (f.base, &f.source)).collect();
    all.push((std_base, &std));
    let sources = Sources::new(all);
    let fail = |diags: &[Diagnostic]| CompileError {
        message: render_all(&sources, diags),
    };
    let std_tokens = lexer::lex_at(&std.text, std_base).map_err(|d| fail(&[d]))?;
    let mut library = parser::parse(std_tokens).map_err(|d| fail(&[d]))?;
    // A main file's own definition of a name replaces the library's (all
    // of its definitions of that name) for the main file. The library
    // keeps using its own: those definitions get a name no program can
    // write (`std.sum`), and the library's calls are pointed at it.
    let own: HashSet<&str> = files[0]
        .program
        .defs
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    let library_names: HashSet<String> = library.defs.iter().map(|d| d.name.clone()).collect();
    let renames: HashMap<String, String> = library_names
        .iter()
        .filter(|n| own.contains(n.as_str()))
        .map(|n| (n.clone(), format!("std.{n}")))
        .collect();
    for def in &mut library.defs {
        ast::rename_globals(def, &renames);
        if let Some(new) = renames.get(&def.name) {
            def.name = new.clone();
        }
        def.file = files.len();
    }

    let names = file_names(&files, &library_names, &renames).map_err(|d| fail(&[d]))?;
    let mut program = ast::Program {
        links: Vec::new(),
        trails: Vec::new(),
        defs: Vec::new(),
    };
    for (index, file) in files.iter().enumerate() {
        for def in &file.program.defs {
            let mut def = def.clone();
            def.name = file.global(index, &def.name);
            def.file = index;
            program.defs.push(def);
        }
        for trail in &file.program.trails {
            let mut trail = trail.clone();
            trail.name = file.global(index, &trail.name);
            trail.file = index;
            program.trails.push(trail);
        }
    }
    program.defs.extend(library.defs);
    for mut trail in library.trails {
        trail.file = files.len();
        trail.library = true;
        program.trails.push(trail);
    }
    let (hir, warnings) = hir::resolve_files(&program, &names).map_err(|ds| fail(&ds))?;
    // Warnings about the library are not the program's business.
    artifacts.warnings.extend(
        warnings
            .iter()
            .filter(|w| w.span.is_none_or(|s| !sources.in_file(s, std_base)))
            .map(|w| sources.render(w)),
    );
    // Each stage stops the pipeline on error: there is no point type
    // checking a program whose names do not resolve, and the later errors
    // would mostly be noise caused by the first one.
    let mut tir = typeck::check(&hir).map_err(|ds| fail(&ds))?;
    artifacts.warnings.extend(
        tir.warnings
            .iter()
            .filter(|w| w.span.is_none_or(|s| !sources.in_file(s, std_base)))
            .map(|w| sources.render(w)),
    );
    ownership::check(&tir).map_err(|ds| fail(&ds))?;
    consteval::fold_globals(&mut tir);
    let mut module = lower::lower(&tir);
    lir::opt::optimize_module(&mut module);
    Ok(module)
}

/// What the names in each file mean (see `hir::FileNames`): the file's own
/// definitions, the files it links whole (`et.pred`), and the definitions
/// it links by name. A linked file does not see the main file's
/// definitions; where it uses one of their names, it means the standard
/// library's or a built-in function, if there is one.
fn file_names(
    files: &[File],
    library: &HashSet<String>,
    renames: &HashMap<String, String>,
) -> Result<Vec<hir::FileNames>, Diagnostic> {
    // A file's definitions and trails share one set of names.
    let defined = |j: usize| -> HashMap<String, String> {
        let program = &files[j].program;
        program
            .defs
            .iter()
            .map(|d| &d.name)
            .chain(program.trails.iter().map(|t| &t.name))
            .map(|n| (n.clone(), files[j].global(j, n)))
            .collect()
    };
    let mut tables = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let mut table = hir::FileNames {
            file: file.path.clone(),
            names: defined(i),
            ..Default::default()
        };
        // Where each name came from, for the error when two links clash.
        let mut linked_at: HashMap<String, Span> = HashMap::new();
        for (link, &j) in file.program.links.iter().zip(&file.targets) {
            let theirs = defined(j);
            match &link.names {
                None => {
                    let (alias, span) = link
                        .aliases
                        .first()
                        .cloned()
                        .unwrap_or_else(|| (link.path.last().unwrap().clone(), link.path_span));
                    if table.files.contains_key(&alias) {
                        return Err(Diagnostic::error(
                            ErrorKind::DefinitionError,
                            span,
                            format!(
                                "two files are linked as `{alias}`; give one of them another name with `>`"
                            ),
                        ));
                    }
                    table.files.insert(alias, (files[j].path.clone(), theirs));
                }
                Some(picked) => {
                    for (k, (name, span)) in picked.iter().enumerate() {
                        let Some(global) = theirs.get(name) else {
                            return Err(Diagnostic::error(
                                ErrorKind::SymbolNotFoundError,
                                *span,
                                format!("`{}` has no definition `{name}`", files[j].path),
                            ));
                        };
                        let (local, local_span) = link.aliases.get(k).unwrap_or(&picked[k]);
                        if let Some(&before) = linked_at.get(local) {
                            if table.names[local] != *global {
                                return Err(Diagnostic::error(
                                    ErrorKind::DefinitionError,
                                    *local_span,
                                    format!("`{local}` is linked twice, from different files"),
                                )
                                .with_note(before, "linked here first"));
                            }
                        } else if table.names.contains_key(local) {
                            return Err(Diagnostic::error(
                                ErrorKind::DefinitionError,
                                *local_span,
                                format!(
                                    "`{local}` is defined in this file and linked from `{}`; give the linked one another name with `>`",
                                    files[j].path
                                ),
                            ));
                        }
                        table.names.insert(local.clone(), global.clone());
                        linked_at.insert(local.clone(), *local_span);
                    }
                }
            }
        }
        if i > 0 {
            let main = &files[0].program;
            let main_names = main
                .defs
                .iter()
                .map(|d| &d.name)
                .chain(main.trails.iter().map(|t| &t.name));
            for n in main_names {
                if table.names.contains_key(n) {
                    continue;
                }
                let instead = if library.contains(n) {
                    Some(renames.get(n).cloned().unwrap_or_else(|| n.clone()))
                } else if hir::Builtin::from_name(n).is_some() {
                    Some(format!("builtin.{n}"))
                } else {
                    None
                };
                table.hidden.insert(n.clone(), instead);
            }
        }
        tables.push(table);
    }
    Ok(tables)
}

/// The runtime library for a target: the core plus the target's OS layer.
///
/// Every program needs support code it does not spell out: turning a float
/// into digits, allocating memory, writing to the terminal. Like C's libc,
/// that code is the "runtime library". Here it is LIR text compiled into the
/// executable alongside the program, so programs need no extra library
/// installed.
pub fn runtime_module(target: Target) -> Module {
    let parse = |file: &str, text: &str| {
        lir::parse::parse_module(text).unwrap_or_else(|e| panic!("runtime {file} is invalid: {e}"))
    };
    let mut module = parse("core.lir", RUNTIME_CORE_LIR);
    module.extend(parse(target.name(), target.runtime_os_lir()));
    module
}

/// Compile a complete LIR module (program + runtime) to an executable.
pub fn backend(
    mut module: Module,
    opts: &Options,
    artifacts: &mut Artifacts,
) -> Result<(), CompileError> {
    // Verify everything, including runtime functions this program does not
    // use, so a broken runtime is caught by every compile, not just by the
    // programs that happen to need the broken part.
    lir::verify::verify_module(&module).map_err(|e| CompileError {
        message: format!("internal error: invalid IR: {e}"),
    })?;
    // Then keep only what the program can actually reach.
    lir::opt::remove_unreachable(&mut module, RUNTIME_ENTRY);
    let module = &module;
    let (code, image) = match opts.target {
        Target::WindowsX64 => {
            let code = codegen::x86_64::compile_module(module);
            let image = pe::link(module, &code, RUNTIME_ENTRY);
            (code, image)
        }
        Target::MacosArm64 => {
            let code = codegen::aarch64::compile_module(module);
            let image = macho::link(module, &code, RUNTIME_ENTRY, opts.name);
            (code, image)
        }
    };
    let image: Image = image.map_err(|e| CompileError {
        message: format!("link error: {e}"),
    })?;
    if opts.want_asm {
        let mut listing = String::new();
        for f in &code {
            listing.push_str(&format!("{}:\n", f.name));
            listing.push_str(&f.render_listing(image.func_addrs[&f.name]));
            listing.push('\n');
        }
        artifacts.asm = Some(listing);
    }
    artifacts.exe = Some(image.bytes);
    Ok(())
}

pub enum Input<'a> {
    Luna(&'a SourceFile),
    Lir(&'a str),
}

pub fn compile(input: Input, opts: &Options) -> Result<Artifacts, CompileError> {
    let mut artifacts = Artifacts::default();
    let module = match input {
        Input::Luna(src) => {
            let program = match opts.files {
                Some(load) => frontend_files(src, load, &mut artifacts)?,
                None => frontend(src, &mut artifacts)?,
            };
            let runtime = runtime_module(opts.target);
            artifacts.ir = Some(format!(
                "; Luna IR generated by lunac {}\n; ---- program ----\n\n{}\n; ---- runtime ----\n\n{}",
                env!("CARGO_PKG_VERSION"),
                lir::print::print_module(&program),
                lir::print::print_module(&runtime)
            ));
            let mut module = program;
            module.extend(runtime);
            module
        }
        Input::Lir(text) => {
            let mut module = lir::parse::parse_module(text).map_err(|e| CompileError {
                message: format!("error: {e}"),
            })?;
            if !module.funcs.iter().any(|f| f.name == RUNTIME_ENTRY) {
                module.extend(runtime_module(opts.target));
            }
            // A hand-written program gets the plain `rt.out_hook`.
            if !module.funcs.iter().any(|f| f.name == "rt.out_hook") {
                module.funcs.push(lower::out_hook(false));
            }
            artifacts.ir = Some(lir::print::print_module(&module));
            module
        }
    };
    backend(module, opts, &mut artifacts)?;
    Ok(artifacts)
}

/// Write an executable file and make it executable.
///
/// An existing file is removed first rather than overwritten: macOS caches
/// the code signature of a file, and a program rewritten in place is killed
/// when it starts.
pub fn write_executable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
