# rluna — compiler design notes

`lunac` compiles Luna source (`.luna`) to native executables for

| Target         | Machine code | Executable              | Program depends on        |
|----------------|--------------|-------------------------|---------------------------|
| `macos-arm64`  | AArch64      | Mach-O, ad-hoc signed   | `/usr/lib/libSystem.B.dylib` |
| `windows-x64`  | x86-64       | PE32+ `.exe`            | `kernel32.dll`            |

It does not use LLVM or any external assembler or linker: it emits machine
code itself and writes the executable directly. By default it compiles for
the system it runs on; `--target=` selects another one (cross-compiling
needs nothing extra).

## Usage

```
lunac hello.luna                    # writes hello (hello.exe on Windows)
lunac hello.luna -o out/hello
lunac hello.luna --emit=ast,ir,asm  # also writes hello.ast, hello.lir, hello.asm
lunac hello.luna --run              # compile, then run
lunac hello.lir                     # compile an IR file directly
lunac hello.luna --target=windows-x64   # writes hello.exe from any system
```

## Pipeline

```
.luna ─lexer─▶ tokens ─parser─▶ AST ─hir─▶ HIR ─typeck─▶ TIR ─lower─▶ LIR ─codegen─▶ machine code ─link─▶ executable
                                                                         ▲
                                                   runtime/core.lir + runtime/<os>.lir
```

Everything up to LIR is the same for every target; `src/target.rs` picks
the backend, the linker and the runtime's OS layer.

| Stage    | File                    | Job                                                            |
|----------|-------------------------|----------------------------------------------------------------|
| lexer    | `src/lexer.rs`          | tokens; newlines are significant outside parentheses           |
| parser   | `src/parser.rs`         | AST (`src/ast.rs`), layout rules below                         |
| hir      | `src/hir.rs`            | name resolution, overload groups, `SymbolNotFoundError`        |
| typeck   | `src/typeck.rs`         | type inference and specialization → typed IR (TIR)             |
| ownership| `src/ownership.rs`      | move checking: no value is used after it was moved             |
| consteval| `src/consteval.rs`      | computes constant globals at compile time                      |
| lower    | `src/lower.rs`          | TIR → LIR, value representation, union dispatch                |
| lir      | `src/lir/`              | IR data structures, printer, parser, verifier, optimizations   |
| codegen  | `src/codegen/aarch64/`  | LIR → AArch64 machine code (Apple arm64 calling convention)    |
|          | `src/codegen/x86_64/`   | LIR → x86-64 machine code (Windows x64 calling convention)     |
| link     | `src/link/macho.rs`     | Mach-O writer: segments, dyld binding, ad-hoc code signature   |
|          | `src/link/pe.rs`        | PE32+ writer: sections, import table, relocations              |
| runtime  | `src/runtime/core.lir`  | printing, strings, allocation, runtime type errors — in LIR    |
|          | `src/runtime/<os>.lir`  | entry point, write, exit and memory for `macos` / `windows`    |

## Language decisions

These are choices made where `readme.md` leaves room; they are easy to revisit.

- **Layout.** A top-level definition starts in column 1 with
  `name params... =` or `name params... ::`. Its body runs until the next
  definition starts. An *indented* `name = expr` line inside a body is a
  binding; any other line is an expression. The last line is the result.
- **Variables.** Parameters and locals are variables. `x = expr` gives `x`
  its first value or, if `x` is visible already, a new one (freeing the old
  value), so variables are mutable. The type can change: after `x = 5` then
  `x = "s"`, `x` is a string.
- **Loops.** `loop x in [a, b]:` runs the lines below it that are indented
  further than `loop` for `x` = a, a + 1, …, b - 1 (none if `b <= a`). The
  bounds are ints, evaluated once. The loop variable is always an int: it
  can be given other ints (`x = 40`), but giving it any other type is a
  `TypeError`, and each iteration still starts with the next value of the
  range. `x` and the variables first bound in the
  body exist only inside the body, and are new in every iteration; a
  variable from outside that the body updates keeps its value across
  iterations and after the loop. Its type is the union of its types before
  the loop and at the end of the body (and may not mix in `void`).
- **Top level** holds only definitions; executable code goes in `main`
  (`main = ...`, no arguments, required).
- **Application** binds tighter than operators (`sum a 9 + 1` is
  `(sum a 9) + 1`). `echo` is applied the same way and takes exactly one
  argument, so anything bigger than a name or literal goes in parentheses:
  `echo (5 > 6)`, `echo (sum a 9)`. `echo 5 > 6` means `(echo 5) > 6`, a
  type error, and `echo sum a 9` is a syntax error.
- **`check`** compares the scrutinee with each pattern using `==`, top to
  bottom. `else` is required unless the scrutinee is a `boolean` and both
  `true` and `false` are covered. Like a loop body, the arms go on the
  following lines, each on its own line, indented further than the line
  with `check`, separated by `;`:

  ```
  test _ = check _:
      1 -> echo "success";
      else -> test (pred _)
  ```

  A `check` can be nested in an arm (its arms indented further still) or
  sit inside parentheses (`echo (check n:` … `)`).
- **Numbers.** `5` is an `int` (64-bit), `5.0`/`1e3` are `float` (64-bit).
  `int op int` stays `int` for `+ - *`; `/` always produces a `float`
  (`7 / 2` is `3.5`). Mixed `int`/`float` arithmetic gives `float`.
- **Operators.** `+ - * /`, unary `-`, and `== != < <= > >=`. `+` also joins
  two strings. Strings compare lexicographically.
- **`echo`** prints any value followed by a newline. Floats print in Python
  style with up to 15 significant digits (`3.5`, `66.0`, `1e-05`, `1e+20`);
  `void` prints as `void`.
- **`void`** marks code that produces nothing. There is only one void and
  it is never stored, so it never mixes with other types: a `check` whose
  arms give void and something else is a type error, and operators reject
  void (`void == void` too). `echo` of a void call prints `void`.
- **Constant globals.** A global without arguments (other than `main`) whose
  evaluation has no side effect is computed by the compiler, and its uses
  become constants: `foo = "he" + "llo"` is the literal `"hello"`. A global
  with a side effect, such as `banner = echo "welcome"`, stays a function
  and runs at every use. So does one that hits a runtime error, or needs
  more than 1000 nested calls or a million steps to compute.
- **Redefinition.** A later definition with the same name, arity and
  parameter types replaces the earlier one (with a warning). Definitions that
  differ in arity or parameter types are overloads.

## Ownership

Every value has exactly one owner: the local variable or parameter that
holds it. The design follows Rust, without references:

- **Moves.** Binding (`y = x`), passing an argument (`f x`), returning (the
  last expression) and the result of a `check` arm give the value away; the
  old name is invalid afterwards and using it is an `OwnershipError`. A
  `check` arm always moves, even inside `echo (check ...)`, as with Rust's
  `if`.
  `id a = a` is fine: the callee owns its parameter and returns it.
- **Borrows.** Operators, `echo`, and the scrutinee and patterns of `check`
  only read a value; the name stays valid.
- **Copies.** `int`, `float` and `boolean` values are copied instead of
  moved; `void` is not a value, so there is nothing to own. Strings and
  mixed types (`numeric`, `any`, …) live on the heap and move.
- **New values.** `x = …` makes `x` usable again after a move, and frees
  the value `x` still held, if any.
- **Branches.** After a `check`, a name counts as moved if any arm moved it.
- **Loops.** A loop body must leave every outside variable as it found it:
  moving `s` inside the body is an error unless `s` gets a new value before
  the body ends (otherwise the next iteration would find it empty), and a
  variable moved before the loop cannot get a value inside it (whether it
  held one afterwards would depend on how often the loop ran). So the state
  after a loop never depends on the number of iterations.
- Arguments are evaluated left to right, so `f x x` moves `x` and then uses
  it: an error for strings, fine for ints.

Because copying depends on the type, the check runs per specialized
instance, after type checking. Like type errors, ownership errors are only
found in functions reachable from `main`.

**Drops.** Lowering frees every heap value exactly once, with no reference
counts or runtime flags:

- an owner (parameter or local) that still holds its value when the
  function ends is dropped there; a moved value is dropped by its new owner;
- a temporary that is only read (`echo (a + b)`, an operand, a `check`
  scrutinee) is dropped right after that read; a discarded statement value
  is dropped at once;
- when only some `check` arms move an owner, the other arms drop it before
  joining, so every path leaves the same owners behind;
- unboxing a value (`numeric` → `int`) frees the box.

Dropping a string calls `rt.free`; dropping a box calls `rt.drop_box`, which
also frees a string payload. The runtime counts live objects (`rt.live`);
the `no_leaks` test runs every test program and checks the count is 0 at
exit.

## Types and specialization

A static type is a set of runtime kinds: `int`, `float`, `string`,
`boolean`, `void`. `numeric` = {int, float}, `any` = all five. As a
parameter constraint `any` accepts a void argument, but no value has a
type that mixes `void` with other kinds.

Functions are specialized per call site argument types, starting from
`main` (like C++ templates or Julia methods). `sum 1 2` and `sum 1.5 2`
produce two machine-code instances. Return types are inferred by fixpoint
iteration, which handles recursion. Unannotated parameters are `any`.

Annotations are constraints: an argument whose type cannot match is a
compile-time `TypeError`; one that *might* match (e.g. a value of type
`int|string` passed to a `numeric` parameter) gets a runtime check.

**Overloads** are tried from top to bottom; the first definition (with the
right number of parameters) whose parameter types accept the arguments is
called:

```
head a :: numeric -> void = echo "winning"
head _ = echo "nothing left"      # everything that is not numeric
```

Usually the argument types decide this at compile time. When an argument
has a mixed type (`int|string`), a definition may accept only some of its
values, and the program tests the value's kind at run time, trying the
definitions in the same order (`TExprKind::Dispatch`). If none accepts it,
the program stops with `TypeError: no definition of `head` accepts these
arguments`. A definition that an earlier one always matches first gets a
warning, since it can never run.

## Value representation

| Type                          | Machine representation                                  |
|-------------------------------|---------------------------------------------------------|
| `int`, `boolean`, `void`      | `i64` (`boolean` is 0/1, `void` is 0)                   |
| `float`                       | `f64`                                                   |
| `string`                      | pointer to `{ len: i64, bytes... }`                     |
| unions (`numeric`, `any`, …)  | pointer to a box `{ tag: i64, payload: i64 }`           |

Heap objects come from `rt.alloc` and can be returned with `rt.free`
(`runtime/core.lir`). Each object follows an 8-byte header:

| Header   | Meaning                                                            |
|----------|--------------------------------------------------------------------|
| `0..7`   | small block of size class c: `16 << c` bytes (16 … 2048)           |
| `≥ 8`    | large block of that many bytes, from and back to the OS            |
| `< 0`    | immortal, never freed (string literals in the data section)         |

Small blocks are carved from 4 MiB chunks and recycled through one free
list per size class, so `rt.alloc` and `rt.free` are O(1). Compiled
programs free values as described under Ownership.

## LIR — the intermediate representation (`.lir`)

LIR is a typed SSA IR in the style of LLVM IR and Cranelift CLIF. Machine
types are `i64` and `f64`. Control flow uses basic blocks with **block
parameters** instead of phi nodes. The text form can be printed
(`--emit=ir`) and parsed back (`lunac file.lir`), and the runtime library is
written in it.

For example,

```
count n = check n:
    0 -> 0;
    else -> 1 + count (n - 1)
```

called with an `int` becomes:

```
; count(int) -> int
func @count(%0: i64) -> i64 {
entry:
  %2 = iconst 0
  %3 = icmp eq %0, %2
  br %3, arm2, next3
check_end1(%1: i64):
  ret %1
arm2:
  %4 = iconst 0
  jmp check_end1(%4)
next3:
  %5 = iconst 1
  %6 = iconst 1
  %7 = isub %0, %6
  %8 = call i64 @count(%7)
  %9 = iadd %5, %8
  jmp check_end1(%9)
}
```

Data and imports look like:

```
data @str.0 = { i64 5, bytes "hello" }
extern @libSystem.write(i64, i64, i64) -> i64
```

Instructions:

| Group        | Instructions                                                         |
|--------------|----------------------------------------------------------------------|
| constants    | `iconst N`, `fconst X`                                               |
| integer      | `iadd isub imul sdiv srem udiv urem and or xor shl shr sar`          |
| float        | `fadd fsub fmul fdiv`                                                |
| compare      | `icmp eq/ne/slt/sle/sgt/sge/ult/ule/ugt/uge`, `fcmp eq/ne/lt/le/gt/ge` (→ i64 0/1) |
| conversion   | `sitofp`, `fptosi`, `bitcast %v to T`                                |
| memory       | `load i64/f64/u8 %p, off`, `store i64/f64/u8 %p, off, %v`, `alloca N`, `addr @sym` |
| calls        | `call T @f(args)` (T is `i64`, `f64` or `void`)                      |
| terminators  | `jmp b(args)`, `br %c, b1(args), b2(args)`, `ret [%v]`, `unreachable` |

`extern @library.Name` declares an imported function: `@libSystem.write`
imports `_write` from `/usr/lib/libSystem.B.dylib` on macOS, and
`@kernel32.WriteFile` imports `WriteFile` from `kernel32.dll` on Windows.
Every module is checked by
`lir::verify` before code generation. `lir::opt` currently runs dead code
elimination inside each function and, once the program and runtime are
combined, removes every function, data object and import that `rt.start`
cannot reach, so a program only carries the parts of the runtime it uses
(`--emit=ir` still shows the whole runtime; `--emit=asm` shows what was
kept).

## Runtime

The runtime is LIR, compiled together with every program. `core.lir` is the
same for every target; it reaches the operating system only through four
functions that each `<os>.lir` provides:

| Function                         | macOS (libSystem)      | Windows (kernel32)          |
|----------------------------------|------------------------|-----------------------------|
| `@rt.start()` — entry point      | called by dyld as `main` | PE entry point            |
| `@rt.sys_write(stream, p, n)`    | `write`, looped         | `WriteFile` on std handles |
| `@rt.sys_exit(code)`             | `exit`                  | `ExitProcess`              |
| `@rt.sys_alloc(size) -> ptr/0`   | `mmap`                  | `VirtualAlloc`             |
| `@rt.sys_free(ptr, size)`        | `munmap`                | `VirtualFree`              |

Stream 1 is stdout and 2 is stderr. On macOS all system calls go through
libSystem, as Apple requires; there is no C runtime startup code.

## Code generation

Both backends are intentionally simple: each SSA value has its own stack
slot, and each instruction loads its operands into scratch registers,
computes, and stores the result. Each uses its platform's C calling
convention for all calls, so runtime, program and OS functions interoperate:

- **AArch64** (`macos-arm64`): integer arguments in x0–x7 and float
  arguments in d0–d7, counted separately, then 8-byte stack slots; frames
  are built with x29/x30; scratch registers are x9, x10, x16, d0 and d1.
  Data addresses use `adrp` + `add`; imported functions are called through
  the GOT.
- **x86-64** (`windows-x64`): the Windows x64 convention (rcx, rdx, r8, r9 /
  xmm0–3 by position, 32 bytes of shadow space). Data addresses are
  RIP-relative; imported functions are called through the import address
  table.

The resulting code is correct but slow; a register allocator is the natural
next step and LIR is designed to support one.

`--emit=asm` writes a listing with addresses, bytes and mnemonics.

## Executables

**Mach-O (macOS).** `__PAGEZERO`, then `__TEXT` (header, load commands,
code), `__DATA` (data objects and the GOT) and `__LINKEDIT` (bind opcodes,
symbol table, code signature). The image is position-independent and needs
no rebasing: code reaches everything PC-relatively and the data holds no
pointers. dyld fills the GOT from `LC_DYLD_INFO_ONLY` bind opcodes and jumps
to `rt.start` through `LC_MAIN`. Apple silicon only runs signed code, so the
file ends with an ad-hoc signature (SHA-256 of every 4 KiB page, as `ld`
writes it); `codesign -v` accepts it. Because macOS caches signatures per
file, `lunac` replaces an existing output file instead of rewriting it in
place.

**PE (Windows).** `.text`, `.data` and `.idata` sections at a fixed image
base; no base relocations.

## Testing

`cargo test` runs unit tests and `tests/cases.rs`, which compiles every
`tests/cases/*.luna` for the host, runs it, compares stdout (and stderr)
with the `.out`/`.err` files, then recompiles the emitted `.lir` and checks
it behaves the same. It also links a program for every target, so the
backend that cannot run on the host is still built and verified. The
instruction encoders are unit-tested against bytes from the system
assemblers.

## Possible next steps

- register allocation and more LIR optimizations (constant folding, inlining)
- more of the standard library
- a Linux (ELF) target; most of the work is an ELF writer and a
  `runtime/linux.lir` making system calls directly
