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

use std::io;
use std::path::Path;

use crate::codegen;
use crate::diag::{Diagnostic, SourceFile, Sources};
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
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            target: Target::default(),
            want_asm: false,
            name: "luna",
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

/// Compile Luna source to an LIR module (without the runtime).
pub fn frontend(src: &SourceFile, artifacts: &mut Artifacts) -> Result<Module, CompileError> {
    // The standard library's byte offsets follow the program's, so every
    // span says which file it is in.
    let std = SourceFile::new("std.luna", STD_LUNA);
    let std_base = src.text.len() + 1;
    let sources = Sources::new(vec![(0, src), (std_base, &std)]);
    let fail = |diags: &[Diagnostic]| CompileError {
        message: render_all(&sources, diags),
    };
    let tokens = lexer::lex(&src.text).map_err(|d| fail(&[d]))?;
    let mut program = parser::parse(tokens).map_err(|d| fail(&[d]))?;
    artifacts.ast = Some(ast::dump(&program));
    let std_tokens = lexer::lex_at(&std.text, std_base).map_err(|d| fail(&[d]))?;
    let mut library = parser::parse(std_tokens).map_err(|d| fail(&[d]))?;
    // A program's own definition of a name replaces the library's (all of
    // its definitions of that name) for the program. The library keeps
    // using its own: those definitions get a name no program can write
    // (`std.sum`), and the library's calls are pointed at it.
    let own: std::collections::HashSet<&str> =
        program.defs.iter().map(|d| d.name.as_str()).collect();
    let renames: std::collections::HashMap<String, String> = library
        .defs
        .iter()
        .filter(|d| own.contains(d.name.as_str()))
        .map(|d| (d.name.clone(), format!("std.{}", d.name)))
        .collect();
    for def in &mut library.defs {
        ast::rename_globals(def, &renames);
        if let Some(new) = renames.get(&def.name) {
            def.name = new.clone();
        }
    }
    program.defs.extend(library.defs);
    let (hir, warnings) = hir::resolve(&program).map_err(|ds| fail(&ds))?;
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
    ownership::check(&tir).map_err(|ds| fail(&ds))?;
    consteval::fold_globals(&mut tir);
    let mut module = lower::lower(&tir);
    lir::opt::optimize_module(&mut module);
    Ok(module)
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
            let program = frontend(src, &mut artifacts)?;
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
