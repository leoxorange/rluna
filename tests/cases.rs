//! End-to-end tests: compile every `tests/cases/*.luna`, run the executable
//! and compare its stdout with `<name>.out` (and stderr with `<name>.err`,
//! in which case the program must exit with code 1).
//!
//! Each program is also compiled a second time from its own emitted `.lir`,
//! which checks that the IR printer and parser round-trip.

use std::path::{Path, PathBuf};
use std::process::Command;

use rluna::diag::SourceFile;
use rluna::driver::{self, Input};

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cases");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_exe(exe: &[u8], path: &Path) -> (String, String, i32) {
    std::fs::write(path, exe).unwrap();
    let out = Command::new(path).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

fn check_case(luna: &Path) -> Result<(), String> {
    let name = luna.file_stem().unwrap().to_string_lossy().to_string();
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
    let artifacts = driver::compile(Input::Luna(&src), true)
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
    check(
        "luna",
        run_exe(
            artifacts.exe.as_ref().unwrap(),
            &dir.join(format!("{name}.exe")),
        ),
    )?;

    let ir = artifacts.ir.unwrap();
    let again = driver::compile(Input::Lir(&ir), false)
        .map_err(|e| format!("{name}: emitted IR does not compile:\n{}", e.message))?;
    check(
        "lir",
        run_exe(
            again.exe.as_ref().unwrap(),
            &dir.join(format!("{name}.lir.exe")),
        ),
    )?;
    Ok(())
}

#[test]
fn cases() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "luna"))
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
        ("main = check 3:\n1 -> echo \"a\"", "needs an `else` branch"),
        (
            "main = echo \"a\" - 1",
            "operator `-` cannot be applied to string and int",
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
        match driver::compile(Input::Luna(&file), false) {
            Ok(_) => panic!("expected an error for:\n{src}"),
            Err(e) => assert!(
                e.message.contains(expected),
                "for:\n{src}\nexpected `{expected}` in:\n{}",
                e.message
            ),
        }
    }
}
