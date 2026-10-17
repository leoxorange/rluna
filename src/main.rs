//! `lunac` — command-line driver for the Luna compiler.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rluna::diag::SourceFile;
use rluna::driver::{self, Input};

const USAGE: &str = "\
Usage: lunac <file.luna | file.lir> [options]

Options:
  -o <path>         Write the executable to <path> (default: <input>.exe)
  --emit=<kinds>    Also write intermediate files next to the output,
                    comma-separated:
                      ast   parsed syntax tree      (<name>.ast)
                      ir    Luna IR (LIR)           (<name>.lir)
                      asm   x86-64 listing          (<name>.asm)
  -r, --run         Run the program after compiling it
  -h, --help        Show this help
  -V, --version     Show the compiler version
";

struct Options {
    input: PathBuf,
    output: Option<PathBuf>,
    emit_ast: bool,
    emit_ir: bool,
    emit_asm: bool,
    run: bool,
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let mut opts = Options {
        input: PathBuf::new(),
        output: None,
        emit_ast: false,
        emit_ir: false,
        emit_asm: false,
        run: false,
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

fn run() -> Result<ExitCode, String> {
    let Some(opts) = parse_args().map_err(|e| format!("{e}\n\n{USAGE}"))? else {
        return Ok(ExitCode::SUCCESS);
    };
    let text = std::fs::read_to_string(&opts.input)
        .map_err(|e| format!("cannot read {}: {e}", opts.input.display()))?;
    let output = opts
        .output
        .clone()
        .unwrap_or_else(|| opts.input.with_extension("exe"));
    let is_lir = opts.input.extension().is_some_and(|e| e == "lir");

    let source = SourceFile::new(opts.input.display().to_string(), text);
    let input = if is_lir {
        Input::Lir(&source.text)
    } else {
        Input::Luna(&source)
    };
    let artifacts = match driver::compile(input, opts.emit_asm) {
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
    write(&output, artifacts.exe.as_deref().unwrap_or_default())?;

    if opts.run {
        let path = std::path::absolute(&output).map_err(|e| e.to_string())?;
        let status = std::process::Command::new(&path)
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
