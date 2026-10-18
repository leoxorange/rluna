//! End-to-end tests: compile every `tests/cases/*.luna` for the host, run
//! the executable and compare its stdout with `<name>.out` (and stderr with
//! `<name>.err`, in which case the program must exit with code 1).
//! A case may give the program its input in `<name>.in` and its arguments,
//! one per line, in `<name>.args`, where `$OUT` is a folder for the files
//! it writes.
//!
//! Each program is also compiled a second time from its own emitted `.lir`,
//! which checks that the IR printer and parser round-trip.
//!
//! A case can also be a hand-written `tests/cases/*.lir` program, for
//! testing the runtime directly; it is compiled once, as written.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rluna::diag::SourceFile;
use rluna::driver::{self, Input, Options};
use rluna::target::Target;

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cases");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Reads linked files below `tests/cases` (`| links/geometry`).
fn case_file(path: &str) -> Option<String> {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/cases")
            .join(path),
    )
    .ok()
}

/// What a case gives the program it runs: `<name>.in` and `<name>.args`.
fn case_inputs(luna: &Path) -> (Vec<u8>, Vec<String>) {
    let input = std::fs::read(luna.with_extension("in")).unwrap_or_default();
    let out = out_dir().display().to_string();
    let args = std::fs::read_to_string(luna.with_extension("args"))
        .unwrap_or_default()
        .lines()
        .map(|a| a.replace("$OUT", &out))
        .collect();
    (input, args)
}

fn run_exe(
    exe: &[u8],
    path: &Path,
    (input, args): &(Vec<u8>, Vec<String>),
) -> (String, String, i32) {
    driver::write_executable(path, exe).unwrap();
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A program may end without reading all of its input.
    let _ = child.stdin.take().unwrap().write_all(input);
    let out = child.wait_with_output().unwrap();
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
        files: Some(&case_file),
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
    let inputs = case_inputs(luna);
    check(
        "luna",
        run_exe(
            artifacts.exe.as_ref().unwrap(),
            &exe_path(name.clone()),
            &inputs,
        ),
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
            &inputs,
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
            "argument 1 of `sum` must be Numeric",
        ),
        ("main = echo missing", "SymbolNotFoundError"),
        (
            "main =\n    over",
            "`over` leaves a loop, but this is not inside a `loop`",
        ),
        (
            "main =\n    loop i in [0..3]:\n        echo [jump | x in [0..3]]",
            "`jump` goes on with a loop's next round, but this is not inside a `loop`",
        ),
        (
            "main =\n    xs = [0..infinite]",
            "a range that never ends (`[0..infinite]`) can only be what a `loop` goes through",
        ),
        (
            "main =\n    echo [x | x in [0..infinite]]",
            "a range that never ends",
        ),
        (
            "take s = s\nmain =\n    s = \"a\"\n    loop i in [0..3]:\n        take s\n        if i == 1:\n            over\n        s = \"b\"",
            "so after the loop it would have no value; give `s` a new value before this `over`",
        ),
        (
            "take s = s\nmain =\n    s = \"a\"\n    loop i in [0..3]:\n        take s\n        if i == 1:\n            jump\n        s = \"b\"",
            "give `s` a new value before this `jump`",
        ),
        ("foo = 1", "no `main` function"),
        (
            "main = check 3:\n    1 -> echo \"a\"",
            "needs an `else` branch",
        ),
        (
            "main = echo (\"a\" - 1)",
            "operator `-` cannot be applied to String and Int",
        ),
        ("main = echo 1 2", "`echo` takes one argument"),
        (
            "main =\n    loop i, x in [5, 6]:\n        i = \"s\"",
            "`i` is the variable of a loop and must stay an int",
        ),
        (
            "main =\n    echo [s | s in [\"a\"], n in [0..2]]",
            "`s` is moved in a list comprehension",
        ),
        // an error inside the standard library points into std.luna
        ("main = echo sum [\"a\", \"b\"]", "--> std.luna:"),
        (
            "main = echo [x | x in [1, 2], x]",
            "a condition in a list comprehension must be a boolean",
        ),
        (
            "f x = x\nf x y = x\ng x = x\ng x y = y\nmain = echo f g 1 2",
            "the arguments of `f` can be grouped in more than one way",
        ),
        (
            "main = echo ([\"a\"] ** [1])",
            "`**` multiplies two arrays of numbers",
        ),
        (
            "main = echo [ret 1 | x in [0..3]]",
            "`ret` cannot leave the function from inside a list comprehension",
        ),
        (
            "f x :: int -> int =\n    ret \"s\"\nmain = echo (f 1)",
            "the function is declared to return Int",
        ),
        ("PI = 3\nmain = echo PI", "`PI` is a built-in constant"),
        (
            "native now :: Int\nmain = echo now",
            "only the standard library can declare native functions",
        ),
        (
            "main = print read_line",
            "argument 1 of `print` is String?, which may be void",
        ),
        (
            "trail F:\n    x :: Int = 1\nf a :: F? -> Int = 1\nmain = echo (f void)",
            "`F?` cannot be written yet",
        ),
        (
            "f x :: Int -> Int? = \"s\"\nmain = echo (f 1)",
            "`f` is declared to return Int?",
        ),
        ("main = echo (len 5)", "`len` takes an array or a string"),
        (
            "main = echo (head [1] [2])",
            "`head` is called with 2 argument(s)",
        ),
        (
            "main = echo [1, \"a\"]",
            "the elements of an array must all have the same type",
        ),
        (
            "main = echo ([1] ++ [\"a\"])",
            "`++` joins arrays whose elements have the same type",
        ),
        (
            "main =\n    s = \"x\"\n    echo [s | i in [0..3]]",
            "`s` is moved in a list comprehension",
        ),
        (
            "main =\n    if 5:\n        echo 1",
            "the condition of `if` must be a Boolean, but this is Int",
        ),
        (
            "main =\n    if true:\n        y = 1\n    echo y",
            "cannot find symbol `y`",
        ),
        (
            "f n :: Int -> Int = if n > 0:\n    1\nmain = echo (f 1)",
            "`f` is declared to return Int, but this is Int?",
        ),
        (
            "main =\n    if true: echo 1",
            "the block of `if` goes on the following lines",
        ),
        ("main =\n    if true:\n    echo 1", "`if` has no block"),
        (
            "main =\n    if true:\n        echo 1\n        else:\n        echo 2",
            "this `else` belongs to no `if`",
        ),
        (
            "main =\n    t = 0\n    echo [(if x > 1:\n        t = x\n    else:\n        0) | x in [1, 2]]",
            "a list comprehension cannot give `t` a new value",
        ),
        (
            "main =\n    s = \"a\"\n    if true:\n        t = s\n    echo s",
            "`s` is used after its value was moved",
        ),
        (
            "trail A:\n    x :: Int\ntrail B:\n    y :: Int\nmain = echo (if true:\n    A {x = 1}\nelse:\n    B {y = 2})",
            "the blocks of this `if` give A and B",
        ),
        (
            "main = echo (7.5 % 2)",
            "operator `%` cannot be applied to Float and Int",
        ),
        (
            "trail Box follow Numeric:\n    x :: Int\n    add @other :: Self -> Self = Box {x = x}\n    sub @other :: Self -> Self = Box {x = x}\n    mul @other :: Self -> Self = Box {x = x}\n    div @other :: Self -> Self = Box {x = x}\n    compare @other :: Self -> Int = 0\n    textualize = \"box\"\nmain = echo (Box {x = 1} % Box {x = 2})",
            "`%` on `Box` values calls its `mod @other :: Self -> Self`",
        ),
        (
            "main = echo ([1, 2] !! \"a\")",
            "the position in `!!` must be an int",
        ),
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
            "f a b = check a:\n    true -> check b:\n        true -> 1;\n        false -> 2;\n    false -> 3\nmain = echo (f true true)",
            "the last arm of a `check` has no `;` after it",
        ),
        (
            "f n = check n:\n    1 -> 1\n    else -> 2\nmain = echo (f 1)",
            "expected `;` after this arm",
        ),
        (
            "f n = check n:\n    else -> 1;\n    1 -> 2\nmain = echo (f 1)",
            "the `else` branch must be the last branch",
        ),
        (
            "main =\n    s = \"x\"\n    loop i in [0..3]:\n        y = s\n    echo 1",
            "`s` is moved in this loop",
        ),
        (
            "main =\n    s = \"x\"\n    y = s\n    loop i in [0..3]:\n        s = \"new\"\n    echo 1",
            "`s` gets a value in this loop, but it was moved before the loop",
        ),
        (
            "main =\n    loop i in [0..2]:\n        inner = i\n    echo inner",
            "cannot find symbol `inner`",
        ),
        (
            "main =\n    loop i in [0..2]:\n    echo 1",
            "this loop has no body",
        ),
        (
            "main =\n    loop i in [0..2.5]:\n        echo i",
            "the end of a range must be an int",
        ),
        (
            "main =\n    loop x in [0..5]:\n        x = \"s\"",
            "`x` is the variable of a loop and must stay an int, but this is String",
        ),
        (
            "main =\n    loop x in [0..2]:\n        loop y in [0..2]:\n            x = 2.5",
            "`x` is the variable of a loop and must stay an int, but this is Float",
        ),
        // borrowing: what is borrowed cannot be given away or changed
        (
            "f @a = a\nmain = echo (f \"x\")",
            "`a` is borrowed (`@a`): its value still belongs to the caller, so it cannot be given away",
        ),
        (
            "f @a = [a]\nmain = echo (f \"x\")",
            "`a` is borrowed (`@a`)",
        ),
        (
            "f @a =\n    a = \"y\"\n    a\nmain = echo (f \"x\")",
            "`a` is borrowed, so it cannot get a new value",
        ),
        (
            "main =\n    a = [\"x\"]\n    y = head a\n    echo y",
            "this is an element still inside `a`",
        ),
        (
            "main =\n    a = [\"x\"]\n    echo [a !! 0]",
            "this is an element still inside `a`",
        ),
        (
            "f @x y = x + y\nmain =\n    s = \"x\"\n    echo (f s s)",
            "`s` is moved here while it is borrowed",
        ),
        (
            "g a = a\nmain =\n    a = \"x\"\n    echo (a == g a)",
            "`a` is moved here while it is borrowed",
        ),
        (
            "main =\n    a = [\"x\"]\n    loop w in @a:\n        b = a\n    echo 1",
            "`a` is moved here while it is borrowed",
        ),
        (
            "main =\n    a = [\"x\"]\n    loop w in @a:\n        a = [\"y\"]\n    echo 1",
            "`a` gets a new value here while it is borrowed",
        ),
        (
            "main =\n    a = [\"x\"]\n    loop w in @a:\n        w = \"z\"\n    echo 1",
            "`w` is borrowed, so it cannot get a new value",
        ),
        (
            "main =\n    a = [\"x\"]\n    loop w in @a:\n        b = w\n    echo 1",
            "`w` is an element borrowed from what the loop goes through",
        ),
        (
            "main =\n    a = [\"x\"]\n    echo [w | w in @a]",
            "`w` is an element borrowed",
        ),
        (
            "f @(x:xs) = x\nmain = echo 1",
            "`@` borrows a parameter given by name",
        ),
        (
            "f @a = 1\nf a :: int = 2\nmain = echo (f 1)",
            "every definition of `f` with 1 parameter(s) must borrow (`@`) the same parameters",
        ),
        (
            "f a = a\nmain =\n    s = \"x\"\n    echo (f @s)",
            "`@` only marks a borrowed parameter",
        ),
        // trails
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nbuy m :: Int -> Fruit = {name = 1}\nmain = echo (buy 1).peel",
            "the field `name` of `Fruit` is String, but this is Int",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nbuy m :: Int -> Fruit = {peel = true}\nmain = echo (buy 1).peel",
            "every `Fruit` needs a value for `name`, which has no default",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nbuy m :: Int -> Fruit = {name = \"a\", money = 5}\nmain = echo (buy 1).peel",
            "`Fruit` has no field `money`",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain =\n    t = {x = \"a\"}\n    echo 1",
            "this `{...}` needs the name of its trail",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain =\n    t = Fruit {name = \"a\"}\n    n = t.name\n    echo n",
            "a field cannot be moved out of its value",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain =\n    t = Fruit {name = \"a\"}\n    echo (copy t)",
            "`copy` needs a value whose trail follows Copy, but `Fruit` does not follow Copy",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain =\n    t = Fruit {name = \"a\"}\n    echo (t == t)",
            "`Fruit` values cannot be compared: `Fruit` does not follow Compare",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\ntrail Shop:\n    x :: Int\nmain = echo [Fruit {name = \"a\"}, Shop {x = 1}]",
            "give Fruit and Shop, but a value has one trail at a time",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\ntrail Fruit:\n    x :: Int\nmain = echo 1",
            "the trail `Fruit` is defined more than once",
        ),
        (
            "trail Int:\n    x :: Int\nmain = echo 1",
            "`Int` is an embedded trail",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nFruit = 1\nmain = echo 1",
            "names both a trail and a function",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain = echo Fruit",
            "`Fruit` is a trail, not a value",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain = echo (Fruit {name = \"a\"}).color",
            "`Fruit` has no field `color`",
        ),
        (
            "main =\n    x = 5\n    echo x.name",
            "`.name` reads a field of a trail's value, but this is Int",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\ntrail C follow Copy:\n    f :: Fruit\nmain = echo (copy (C {f = Fruit {name = \"a\"}}))",
            "`C` follows Copy, so its fields must too",
        ),
        (
            "trail Fruit:\n    name :: String\n    peel :: Boolean = false\nmain =\n    t = Fruit {name = \"a\"}\n    echo t.name 5",
            "`name` is a field of `Fruit`, not a function",
        ),
        (
            "trail Fruit:\n    name :: String\n    label = name\nmain = echo (Fruit {name = \"a\"}).label",
            "a default cannot give it away",
        ),
        (
            "trail Fruit:\n    name :: Shape\nmain = echo 1",
            "cannot find the trail `Shape`",
        ),
        // following trails
        (
            "trail Shop follow Show:\n    name :: String\nmain = echo (Shop {name = \"a\"})",
            "every `Shop` needs a value for `textualize`",
        ),
        (
            "trail Fruit follow Show:\n    name :: String\n    textualize = copy name\ntrail Plain:\n    x :: Int\ndescribe @f :: Fruit -> String = f.name\nmain = echo (describe 5)",
            "argument 1 of `describe` must follow Fruit, but this is Int",
        ),
        (
            "trail Fruit follow Show:\n    name :: String\n    textualize = copy name\ntrail Plain:\n    x :: Int\nshow @x :: Show -> Int = 1\nmain = echo (show (Plain {x = 1}))",
            "argument 1 of `show` must follow Show, but this is Plain",
        ),
        (
            "trail A follow B:\n    x :: Int\ntrail B follow A:\n    y :: Int\nmain = echo 1",
            "follows itself",
        ),
        (
            "trail A follow Iterable:\n    x :: Int\nmain = echo 1",
            "only the embedded values follow `Iterable`, for now",
        ),
        (
            "trail Fruit follow Show:\n    name :: String\n    textualize = copy name\ntrail Plain:\n    x :: Int\ntrail A follow Fruit:\n    textualize :: Int = 5\nmain = echo 1",
            "`textualize` is String in the trail `A` follows, so it cannot be Int here",
        ),
        (
            "trail A:\n    x = y + 1\n    y = x + 1\nmain = echo (A {}).x",
            "the defaults of `x` and `y` use each other",
        ),
        (
            "trail Fruit follow Show:\n    name :: String\n    textualize = copy name\ntrail Plain:\n    x :: Int\nmain = echo (Plain {x = 1}).textualize",
            "`Plain` has no field `textualize` (it does not follow Show)",
        ),
        (
            "trail Fruit follow Show:\n    name :: String\n    textualize = copy name\ntrail Plain:\n    x :: Int\npick n :: Int -> Fruit = Plain {x = 1}\nmain = echo (pick 1)",
            "`pick` is declared to return Fruit, but this is Plain, which does not follow it",
        ),
        // functions of trails
        (
            "trail Half follow Compare:\n    x :: Int\nmain = echo (Half {x = 1}).x",
            "a `Half` cannot be made: it does not give the function `compare`, which `Compare` asks for",
        ),
        (
            "trail Bad follow Compare:\n    x :: Int\n    compare other :: Self -> Int = 0\nmain = echo 1",
            "`compare` must take the parameters `Compare` asks for",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain = echo ((Box {x = 1}).get 1 2)",
            "`get` of `Box` takes 1 argument(s), but it is given 2",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain = echo ((Box {x = 1}).nope 1)",
            "`Box` has no function `nope`",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain =\n    b = Box {x = 1}\n    echo (b.x 1)",
            "`x` is a field of `Box`, not a function",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain =\n    b = Box {x = 1}\n    echo b.get",
            "(it is a function: give it its arguments)",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain = echo (Plain {x = 1} < Plain {x = 2})",
            "`Plain` values cannot be compared: `Plain` does not follow Compare",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain = echo (Box {x = 1} < Plain {x = 2})",
            "on values of a trail it needs two of the same trail",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain = echo (Box {x = 1} + Box {x = 2})",
            "`Box` values cannot be used as numbers: `Box` does not follow Numeric",
        ),
        (
            "trail Box follow Compare:\n    x :: Int\n    compare @other :: Self -> Int = x - other.x\n    get n = x + n\ntrail Plain:\n    x :: Int\nmain = echo (check Plain {x = 1}:\n    Plain {x = 1} -> 1;\n    else -> 2)",
            "`Plain` does not follow Compare",
        ),
        (
            "f x :: Self -> Int = 1\nmain = echo 1",
            "cannot find the trail `Self`",
        ),
        (
            "trail Q:\n    x :: Int\n    show n = x + n\n    use = show 1\nmain = echo (Q {x = 1}).use",
            "a field's default cannot call `show`",
        ),
        // embedded trails and trail functions
        (
            "trail Show:\n    x :: Int\nmain = echo 1",
            "`Show` is an embedded trail and cannot be defined again",
        ),
        (
            "main = echo (Show {textualize = \"a\"})",
            "`Show` is an embedded trail: it has no values of its own",
        ),
        (
            "trail T:\n    x :: Int\n    f @a = 1\n    f a = 2\nmain = echo 1",
            "every definition of `f` with 1 parameter(s) must borrow (`@`) the same parameters",
        ),
        (
            "trail T:\n    x :: Int\n    f @a = 1\ntrail U follow T:\n    f a = 2\nmain = echo 1",
            "must borrow (`@`) the same parameters as in `T`",
        ),
        (
            "describe @f :: Show -> Int = 1\ntrail P:\n    x :: Int\nmain = echo (describe [P {x = 1}])",
            "argument 1 of `describe` must follow Show, but this is [P]",
        ),
        // && and ||
        (
            "main = echo (1 && true)",
            "`&&` needs a Boolean on each side, but this is Int",
        ),
        (
            "main = echo (true || \"s\")",
            "`||` needs a Boolean on each side, but this is String",
        ),
        (
            "main = echo (head [true] && true)",
            "but this is Boolean?: it may be void",
        ),
        (
            "take s = len s > 1\nmain =\n    s = \"abc\"\n    echo (true && take s)\n    echo s",
            "`s` is used after its value was moved",
        ),
        ("main = echo (true & false)", "`&` alone is not an operator"),
        ("main = echo (!5)", "`!` needs a Boolean, but this is Int"),
        (
            "main = echo (!(head [true]))",
            "`!` needs a Boolean, but this is Boolean?: it may be void",
        ),
        ("main = echo (!!true)", "to negate twice, write `!(!b)`"),
        // moving: loops, `++` and `tail` take what they are given
        (
            "main =\n    a = [1]\n    loop x in a:\n        echo x\n    echo a",
            "`a` is used after its value was moved",
        ),
        (
            "main =\n    a = [1]\n    b = a ++ [2]\n    echo a",
            "`a` is used after its value was moved",
        ),
        (
            "main =\n    a = [1]\n    echo (tail a)\n    echo a",
            "`a` is used after its value was moved",
        ),
        (
            "main =\n    a = [\"x\"]\n    loop i in [0..2]:\n        echo [w | w in a]",
            "`a` is moved in this loop",
        ),
        (
            "f n = check n:\n    1 -> void;\n    else -> 5\ng x :: int -> int = x\nmain = echo (g (f 1))",
            "argument 1 of `g` is Int?, which may be void, but `g` does not accept void",
        ),
        (
            "f n = check n:\n    1 -> void;\n    else -> 5\nmain =\n    loop i in [0..f 2]:\n        echo i",
            "the end of a range must be an int, but this is Int?, which may be void",
        ),
        (
            "f n = check n:\n    1 -> void;\n    else -> 5\nmain = check (f 1):\n    5 -> echo 1",
            "(or a `void ->` arm for the void case)",
        ),
        (
            "main =\n    x = \"s\"\n    y = x\n    echo x",
            "`x` is used after its value was moved",
        ),
        (
            "show s = echo s\nmain =\n    x = \"s\"\n    show x\n    show x",
            "the value of `x` (String) was moved here",
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
        ("main = echo (1 :? Nope)", "cannot find the trail `Nope`"),
        (
            "main = echo (1 :? Int == true)",
            "`:?` cannot be chained with another comparison",
        ),
        (
            "main = echo (1 == 1 :? Boolean)",
            "comparison operators cannot be chained",
        ),
        ("main = echo (1 + 2", "SyntaxError"),
        (
            "echo 5\nmain = 1",
            "top-level code must live inside a function",
        ),
        (
            "f x :: int -> string = x\nmain = f 1",
            "declared to return String",
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

/// Mistakes in linking files: each program is a set of files, the first
/// one the main file.
#[test]
fn link_errors() {
    let cases: &[(&[(&str, &str)], &str)] = &[
        (
            &[("main.luna", "| nothere\nmain = echo 1")],
            "cannot find the file `nothere.luna` or `nothere/nothere.luna`",
        ),
        (
            &[
                ("main.luna", "| m > q\nmain = echo q.nope"),
                ("m.luna", "f a = a"),
            ],
            "cannot find symbol `q.nope`: `m.luna` has no definition `nope`",
        ),
        (
            &[
                ("main.luna", "| m\nmain = echo zz.f 1"),
                ("m.luna", "f a = a"),
            ],
            "no file is linked as `zz`",
        ),
        (
            &[("main.luna", "main = echo 1\n| m"), ("m.luna", "f a = a")],
            "links (`| file`) go at the top of the file",
        ),
        (
            &[
                ("main.luna", "| m#f\nf x = x\nmain = echo (f 1)"),
                ("m.luna", "f a = a"),
            ],
            "`f` is defined in this file and linked from `m.luna`",
        ),
        (
            &[
                ("main.luna", "| m\n| m\nmain = echo 1"),
                ("m.luna", "f a = a"),
            ],
            "two files are linked as `m`",
        ),
        (
            &[
                ("main.luna", "| m#f, g > a\nmain = echo 1"),
                ("m.luna", "f a = a\ng = 1"),
            ],
            "2 definition(s) are linked but 1 new name(s) are given",
        ),
        (
            &[
                ("main.luna", "| m#nope\nmain = echo 1"),
                ("m.luna", "f a = a"),
            ],
            "`m.luna` has no definition `nope`",
        ),
        (
            &[("main.luna", "m.f x = x\nmain = echo 1")],
            "a name defined here cannot contain `.`",
        ),
        // a linked file sees only its own names, what it links, and the library
        (
            &[
                ("main.luna", "| m\nhelper = 1\nmain = echo m.uses"),
                ("m.luna", "uses = helper"),
            ],
            "cannot find symbol `helper` (the main file defines it, but `m.luna` does not link it)",
        ),
        // an error in a linked file points into it
        (
            &[
                ("main.luna", "| lib/m\nmain = echo m.bad"),
                ("lib/m.luna", "bad = \"a\" - 1"),
            ],
            "--> lib/m.luna:1:7",
        ),
        (
            &[
                ("main.luna", "| m\nmain = echo m.f 1"),
                ("m.luna", "f a = a"),
                ("m/m.luna", "f a = a"),
            ],
            "both `m.luna` and `m/m.luna` exist",
        ),
    ];
    for (files, expected) in cases {
        let main = SourceFile::new(files[0].0, files[0].1);
        let load = |path: &str| {
            files[1..]
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, text)| text.to_string())
        };
        let opts = Options {
            files: Some(&load),
            ..Options::default()
        };
        match driver::compile(Input::Luna(&main), &opts) {
            Ok(_) => panic!("expected an error for:\n{files:?}"),
            Err(e) => assert!(
                e.message.contains(expected),
                "for:\n{files:?}\nexpected `{expected}` in:\n{}",
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
        let mut module = driver::frontend_files(&src, &case_file, &mut artifacts)
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
            &case_inputs(luna),
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

/// A function that takes arrays apart but has nothing for an empty array.
#[test]
fn missing_empty_pattern_warning() {
    let file = SourceFile::new("first.luna", "first (x:xs) = x\nmain = echo (first [1])");
    let artifacts = driver::compile(Input::Luna(&file), &Options::default())
        .unwrap_or_else(|e| panic!("{}", e.message));
    assert!(
        artifacts
            .warnings
            .iter()
            .any(|w| w.contains("no definition of `first` accepts an empty array")),
        "{:?}",
        artifacts.warnings
    );
}
