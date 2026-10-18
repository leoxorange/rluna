//! `lunac` — command-line driver for the Luna compiler.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rluna::diag::SourceFile;
use rluna::driver::{self, Input};
use rluna::target::Target;

const USAGE: &str = "\
Usage: lunac <file.luna | file.lir> [options] [-- <program arguments>]

Options:
  -o <path>         Write the executable to <path> (default: the input
                    file name, with `.exe` for Windows)
  --target=<name>   Compile for another system (default: this one):
                      macos-arm64   Apple silicon Mac (Mach-O)
                      windows-x64   64-bit Windows (PE .exe)
  --emit=<kinds>    Also write intermediate files next to the output,
                    comma-separated:
                      ast   parsed syntax tree      (<name>.ast)
                      ir    Luna IR (LIR)           (<name>.lir)
                      asm   machine code listing    (<name>.asm)
  -r, --run         Run the program after compiling it, with the
                    arguments after `--`
  -h, --help        Show this help
  -V, --version     Show the compiler version
";

struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
    target: Target,
    emit_ast: bool,
    emit_ir: bool,
    emit_asm: bool,
    run: bool,
    /// What `--run` gives the program: everything after `--`.
    program_args: Vec<String>,
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let mut opts = Options {
        input: PathBuf::new(),
        output: None,
        target: Target::default(),
        emit_ast: false,
        emit_ir: false,
        emit_asm: false,
        run: false,
        program_args: Vec::new(),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("lunac {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-o" => opts.output = Some(args.next().ok_or("`-o` needs a path")?.into()),
            "-r" | "--run" => opts.run = true,
            "--" => {
                opts.program_args = args.by_ref().collect();
                if !opts.run {
                    return Err(
                        "arguments after `--` are for the program, which only `-r` runs".into(),
                    );
                }
            }
            _ if arg.starts_with("--target=") => {
                let name = &arg["--target=".len()..];
                opts.target =
                    Target::from_name(name).ok_or_else(|| format!("unknown target `{name}`"))?;
            }
            _ if arg.starts_with("--emit=") => {
                for kind in arg["--emit=".len()..].split(',') {
                    match kind {
                        "ast" => opts.emit_ast = true,
                        "ir" | "lir" => opts.emit_ir = true,
                        "asm" => opts.emit_asm = true,
                        "exe" | "" => {}
                        _ => return Err(format!("unknown --emit kind `{kind}`")),
                    }
                }
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option `{arg}`")),
            _ => {
                if input.replace(PathBuf::from(&arg)).is_some() {
                    return Err("only one input file is supported".into());
                }
            }
        }
    }
    opts.input = input.ok_or("no input file")?;
    Ok(Some(opts))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    driver::write_executable(path, bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn run() -> Result<ExitCode, String> {
    let Some(opts) = parse_args().map_err(|e| format!("{e}\n\n{USAGE}"))? else {
        return Ok(ExitCode::SUCCESS);
    };
    let text = std::fs::read_to_string(&opts.input)
        .map_err(|e| format!("cannot read {}: {e}", opts.input.display()))?;
    let output = opts
        .output
        .clone()
        .unwrap_or_else(|| opts.input.with_extension(opts.target.exe_extension()));
    if output == opts.input {
        return Err(format!(
            "the executable would overwrite {}; choose another name with -o",
            opts.input.display()
        ));
    }
    let name = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "luna".into());
    let is_lir = opts.input.extension().is_some_and(|e| e == "lir");

    let source = SourceFile::new(opts.input.display().to_string(), text);
    let input = if is_lir {
        Input::Lir(&source.text)
    } else {
        Input::Luna(&source)
    };
    // Linked files are found below the main file's folder.
    let folder = opts.input.parent().unwrap_or(Path::new("")).to_path_buf();
    let load = move |path: &str| std::fs::read_to_string(folder.join(path)).ok();
    let compile_opts = driver::Options {
        target: opts.target,
        want_asm: opts.emit_asm,
        name: &name,
        files: Some(&load),
    };
    let artifacts = match driver::compile(input, &compile_opts) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e.message.trim_end());
            return Ok(ExitCode::FAILURE);
        }
    };
    for w in &artifacts.warnings {
        eprintln!("{}", w.trim_end());
    }

    if opts.emit_ast
        && let Some(ast) = &artifacts.ast
    {
        write(&output.with_extension("ast"), ast.as_bytes())?;
    }
    if opts.emit_ir
        && !is_lir
        && let Some(ir) = &artifacts.ir
    {
        write(&output.with_extension("lir"), ir.as_bytes())?;
    }
    if opts.emit_asm
        && let Some(asm) = &artifacts.asm
    {
        write(&output.with_extension("asm"), asm.as_bytes())?;
    }
    write_executable(&output, artifacts.exe.as_deref().unwrap_or_default())?;

    if opts.run {
        if Target::host() != Some(opts.target) {
            return Err(format!(
                "cannot run a {} program on this system",
                opts.target
            ));
        }
        let path = std::path::absolute(&output).map_err(|e| e.to_string())?;
        let status = std::process::Command::new(&path)
            .args(&opts.program_args)
            .status()
            .map_err(|e| format!("cannot run {}: {e}", path.display()))?;
        return Ok(ExitCode::from(status.code().unwrap_or(1) as u8));
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lunac: {e}");
            ExitCode::FAILURE
        }
    }
}
