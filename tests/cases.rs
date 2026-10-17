//! End-to-end tests: compile every `tests/cases/*.luna` for the host, run
//! the executable and compare its stdout with `<name>.out` (and stderr with
//! `<name>.err`, in which case the program must exit with code 1).
//!
//! Each program is also compiled a second time from its own emitted `.lir`,
//! which checks that the IR printer and parser round-trip.
//!
//! A case can also be a hand-written `tests/cases/*.lir` program, for
//! testing the runtime directly; it is compiled once, as written.

use std::path::{Path, PathBuf};
use std::process::Command;

use rluna::diag::SourceFile;
use rluna::driver::{self, Input, Options};
use rluna::target::Target;

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cases");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_exe(exe: &[u8], path: &Path) -> (String, String, i32) {
    driver::write_executable(path, exe).unwrap();
    let out = Command::new(path).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

fn check_case(luna: &Path) -> Result<(), String> {
    let name = luna.file_stem().unwrap().to_string_lossy().to_string();
    let is_lir = luna.extension().is_some_and(|e| e == "lir");
    let read = |ext: &str| {
        std::fs::read_to_string(luna.with_extension(ext))
            .ok()
            .map(|s| s.replace("\r\n", "\n"))
    };
    let expected_out = read("out").ok_or_else(|| format!("{name}: missing .out file"))?;
    let expected_err = read("err");

    let src = SourceFile::new(
        luna.display().to_string(),
        std::fs::read_to_string(luna).unwrap(),
    );
    let opts = Options {
        want_asm: true,
        name: &name,
        ..Options::default()
    };
    let input = if is_lir {
        Input::Lir(&src.text)
    } else {
        Input::Luna(&src)
    };
    let artifacts = driver::compile(input, &opts)
        .map_err(|e| format!("{name}: compile error:\n{}", e.message))?;

    let check = |label: &str, (out, err, code): (String, String, i32)| -> Result<(), String> {
        if out != expected_out {
            return Err(format!(
                "{name} ({label}): stdout differs\n--- expected\n{expected_out}--- actual\n{out}"
            ));
        }
        match &expected_err {
            Some(e) if *e != err || code != 1 => Err(format!(
                "{name} ({label}): expected exit 1 with stderr\n{e}\ngot exit {code} with stderr\n{err}"
            )),
            None if code != 0 || !err.is_empty() => Err(format!(
                "{name} ({label}): expected success, got exit {code}\n{err}"
            )),
            _ => Ok(()),
        }
    };

    let dir = out_dir();
    let exe_path = |stem: String| dir.join(stem).with_extension(opts.target.exe_extension());
    check(
        "luna",
        run_exe(artifacts.exe.as_ref().unwrap(), &exe_path(name.clone())),
    )?;
    if is_lir {
        return Ok(());
    }

    let ir = artifacts.ir.unwrap();
    let again = driver::compile(Input::Lir(&ir), &Options::default())
        .map_err(|e| format!("{name}: emitted IR does not compile:\n{}", e.message))?;
    check(
        "lir",
        run_exe(
            again.exe.as_ref().unwrap(),
            &exe_path(format!("{name}-lir")),
        ),
    )?;
    Ok(())
}

#[test]
fn cases() {
    if Target::host().is_none() {
        eprintln!("skipped: no target runs on this system");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "luna" || e == "lir"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let failures: Vec<String> = files.iter().filter_map(|f| check_case(f).err()).collect();
    if !failures.is_empty() {
        panic!(
            "{} of {} case(s) failed:\n\n{}",
            failures.len(),
            files.len(),
            failures.join("\n\n")
        );
    }
}

#[test]
fn compile_errors() {
    let cases = [
        (
            "sum a b :: numeric, numeric -> numeric = a + b\nmain = sum \"hello\" 5",
            "argument 1 of `sum` must be numeric",
        ),
        ("main = echo missing", "SymbolNotFoundError"),
        ("foo = 1", "no `main` function"),
        (
            "main = check 3:\n    1 -> echo \"a\"",
            "needs an `else` branch",
        ),
        (
            "main = echo (\"a\" - 1)",
            "operator `-` cannot be applied to string and int",
        ),
        ("f a = a\nmain = echo f 1", "`echo` takes one argument"),
        (
            "f n = check n:\n1 -> 1;\nelse -> 2\nmain = echo (f 1)",
            "the arms of a `check` go on the following lines",
        ),
        (
            "f n = check n: 1 -> 1; else -> 2\nmain = echo (f 1)",
            "the arms of a `check` go on the following lines",
        ),
        (
            "f n = check n:\n    1 -> 1; else -> 2\nmain = echo (f 1)",
            "expected another arm of the `check` after `;`",
        ),
        (
            "f n = check n:\n    else -> 1;\n    1 -> 2\nmain = echo (f 1)",
            "the `else` branch must be the last branch",
        ),
        (
            "main =\n    s = \"x\"\n    loop i in [0, 3]:\n        y = s\n    echo 1",
            "`s` is moved in this loop",
        ),
        (
            "main =\n    s = \"x\"\n    y = s\n    loop i in [0, 3]:\n        s = \"new\"\n    echo 1",
            "`s` gets a value in this loop, but it was moved before the loop",
        ),
        (
            "main =\n    loop i in [0, 2]:\n        inner = i\n    echo inner",
            "cannot find symbol `inner`",
        ),
        (
            "main =\n    loop i in [0, 2]:\n    echo 1",
            "this loop has no body",
        ),
        (
            "main =\n    loop i in [0, 2.5]:\n        echo i",
            "the end of a loop range must be an int",
        ),
        (
            "main =\n    loop x in [0, 5]:\n        x = \"s\"",
            "`x` is the variable of a loop and must stay an int, but this is string",
        ),
        (
            "main =\n    loop x in [0, 2]:\n        loop y in [0, 2]:\n            x = 2.5",
            "`x` is the variable of a loop and must stay an int, but this is float",
        ),
        (
            "f n = check n:\n    1 -> echo \"a\";\n    else -> 5\nmain = echo (f 1)",
            "void cannot be mixed with other types",
        ),
        (
            "nothing _ :: any -> void = 5\nmain = echo ((nothing 1) == (nothing 2))",
            "operator `==` cannot be applied to void and void",
        ),
        (
            "main =\n    x = \"s\"\n    y = x\n    echo x",
            "`x` is used after its value was moved",
        ),
        (
            "show s = echo s\nmain =\n    x = \"s\"\n    show x\n    show x",
            "the value of `x` (string) was moved here",
        ),
        (
            "both a b = a + b\ndup s = both s s\nmain = echo (dup \"x\")",
            "`s` is used after its value was moved",
        ),
        (
            "main =\n    s = \"s\"\n    t = check 1:\n        1 -> s;\n        else -> \"o\"\n    echo s",
            "`s` is used after its value was moved",
        ),
        ("f a = a\nmain = f 1 2", "called with 2 argument(s)"),
        ("main = echo (1 + 2", "SyntaxError"),
        (
            "echo 5\nmain = 1",
            "top-level code must live inside a function",
        ),
        (
            "f x :: int -> string = x\nmain = f 1",
            "declared to return string",
        ),
    ];
    for (src, expected) in cases {
        let file = SourceFile::new("test.luna", src);
        match driver::compile(Input::Luna(&file), &Options::default()) {
            Ok(_) => panic!("expected an error for:\n{src}"),
            Err(e) => assert!(
                e.message.contains(expected),
                "for:\n{src}\nexpected `{expected}` in:\n{}",
                e.message
            ),
        }
    }
}

/// Every target compiles on every host, even where its programs cannot run.
#[test]
fn all_targets_link() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases/readme.luna"),
    )
    .unwrap();
    let file = SourceFile::new("readme.luna", src);
    for target in Target::ALL {
        let opts = Options {
            target,
            want_asm: true,
            ..Options::default()
        };
        let artifacts = driver::compile(Input::Luna(&file), &opts)
            .unwrap_or_else(|e| panic!("{target}: {}", e.message));
        let exe = artifacts.exe.unwrap();
        let magic: &[u8] = match target {
            Target::WindowsX64 => b"MZ",
            Target::MacosArm64 => &[0xCF, 0xFA, 0xED, 0xFE],
        };
        assert!(exe.starts_with(magic), "{target}: wrong file format");
        assert!(artifacts.asm.unwrap().contains("rt.start:"), "{target}");
    }
}

/// Runtime functions a program never reaches are left out of the executable.
#[test]
fn unused_runtime_is_removed() {
    let file = SourceFile::new("hello.luna", "main = echo \"hi\"");
    let opts = Options {
        want_asm: true,
        ..Options::default()
    };
    let asm = driver::compile(Input::Luna(&file), &opts)
        .unwrap_or_else(|e| panic!("{}", e.message))
        .asm
        .unwrap();
    assert!(asm.contains("rt.print_str:"), "{asm}");
    assert!(!asm.contains("rt.print_float:"), "{asm}");
    assert!(!asm.contains("rt.alloc:"), "{asm}");
}

/// Every value is freed exactly once: run each case with its `main` wrapped
/// in a function that prints the runtime's count of live heap objects when
/// the program is done. A leak leaves it above 0, a double free below.
#[test]
fn no_leaks() {
    if Target::host().is_none() {
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "luna"))
        // A program stopped by a runtime error never reaches the count.
        .filter(|p| !p.with_extension("err").exists())
        .collect();
    files.sort();
    let mut failures = Vec::new();
    for luna in &files {
        let name = luna.file_stem().unwrap().to_string_lossy().to_string();
        let src = SourceFile::new(name.clone(), std::fs::read_to_string(luna).unwrap());
        let mut artifacts = driver::Artifacts::default();
        let mut module = driver::frontend(&src, &mut artifacts)
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        for f in &mut module.funcs {
            if f.name == "luna.main" {
                f.name = "leak.program".into();
            }
        }
        module.extend(
            rluna::lir::parse::parse_module(
                "func @luna.main() {
entry:
  call void @leak.program()
  %slot = addr @rt.live
  %live = load i64 %slot, 0
  call void @rt.print_int(%live)
  call void @rt.print_newline()
  ret
}",
            )
            .unwrap(),
        );
        module.extend(driver::runtime_module(Target::default()));
        driver::backend(module, &Options::default(), &mut artifacts)
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        let (out, err, code) = run_exe(
            artifacts.exe.as_ref().unwrap(),
            &out_dir()
                .join(format!("{name}-leaks"))
                .with_extension(Target::default().exe_extension()),
        );
        let live = out.lines().last().unwrap_or_default();
        if code != 0 || live != "0" {
            failures.push(format!(
                "{name}: {live} live object(s) at exit (exit {code}) {err}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Constant globals are computed by the compiler: their functions are gone
/// from the executable. A global with a side effect stays a function.
#[test]
fn constant_globals_are_folded() {
    let src = "count n = check n:\n    0 -> 0;\n    else -> 1 + count (n - 1)
foo = \"he\" + \"llo\"
counted = count 50
banner = echo \"hi\"
main =
    banner
    echo foo
    echo counted";
    let file = SourceFile::new("globals.luna", src);
    let opts = Options {
        want_asm: true,
        ..Options::default()
    };
    let asm = driver::compile(Input::Luna(&file), &opts)
        .unwrap_or_else(|e| panic!("{}", e.message))
        .asm
        .unwrap();
    let functions: Vec<&str> = asm
        .lines()
        .filter(|l| l.ends_with(':') && !l.starts_with('.'))
        .collect();
    for folded in ["foo:", "count:", "counted:"] {
        assert!(
            !functions.contains(&folded),
            "{folded} was not folded: {functions:?}"
        );
    }
    assert!(functions.contains(&"banner:"), "{functions:?}");
}

/// A definition that an earlier one always matches first is reported.
#[test]
fn unreachable_definition_warning() {
    let src = "head _ = echo \"nothing left\"\nhead a :: numeric -> void = echo \"winning\"\nmain = head 1";
    let file = SourceFile::new("head.luna", src);
    let artifacts = driver::compile(Input::Luna(&file), &Options::default())
        .unwrap_or_else(|e| panic!("{}", e.message));
    assert!(
        artifacts
            .warnings
            .iter()
            .any(|w| w.contains("this definition of `head` is never used")),
        "{:?}",
        artifacts.warnings
    );
}
