# rluna — compiler design notes

`lunac` compiles Luna source (`.luna`) to native Windows x64 executables.
It does not use LLVM or any external assembler or linker: it emits machine
code itself and writes the PE `.exe` directly. The only dependency of a
compiled program is `kernel32.dll`.

## Usage

```
lunac hello.luna                    # writes hello.exe
lunac hello.luna -o out\hello.exe
lunac hello.luna --emit=ast,ir,asm  # also writes hello.ast, hello.lir, hello.asm
lunac hello.luna --run              # compile, then run
lunac hello.lir                     # compile an IR file directly
```

## Pipeline

```
.luna ─lexer─▶ tokens ─parser─▶ AST ─hir─▶ HIR ─typeck─▶ TIR ─lower─▶ LIR ─codegen─▶ x86-64 ─link─▶ .exe
                                                                         ▲
                                                     runtime/windows_x64.lir
```

| Stage    | File                    | Job                                                            |
|----------|-------------------------|----------------------------------------------------------------|
| lexer    | `src/lexer.rs`          | tokens; newlines are significant outside parentheses           |
| parser   | `src/parser.rs`         | AST (`src/ast.rs`), layout rules below                         |
| hir      | `src/hir.rs`            | name resolution, overload groups, `SymbolNotFoundError`        |
| typeck   | `src/typeck.rs`         | type inference and specialization → typed IR (TIR)             |
| lower    | `src/lower.rs`          | TIR → LIR, value representation, union dispatch                |
| lir      | `src/lir/`              | IR data structures, printer, parser, verifier, optimizations   |
| codegen  | `src/codegen/`          | LIR → x86-64 machine code (Windows x64 calling convention)     |
| link     | `src/link/pe.rs`        | PE32+ writer: sections, import table, relocations              |
| runtime  | `src/runtime/*.lir`     | printing, strings, allocation, runtime type errors — in LIR    |

## Language decisions

These are choices made where `readme.md` leaves room; they are easy to revisit.

- **Layout.** A top-level definition starts in column 1 with
  `name params... =` or `name params... ::`. Its body runs until the next
  definition starts. An *indented* `name = expr` line inside a body is a local
  binding; any other line is an expression. The last line is the result.
- **Top level** holds only definitions; executable code goes in `main`
  (`main = ...`, no arguments, required).
- **Application** binds tighter than operators (`sum a 9 + 1` is
  `(sum a 9) + 1`), except `echo`, which takes the whole expression after it
  (`echo 5 > 6` prints `false`).
- **`check`** compares the scrutinee with each pattern using `==`, top to
  bottom. `else` is required unless the scrutinee is a `boolean` and both
  `true` and `false` are covered.
- **Numbers.** `5` is an `int` (64-bit), `5.0`/`1e3` are `float` (64-bit).
  `int op int` stays `int` for `+ - *`; `/` always produces a `float`
  (`7 / 2` is `3.5`). Mixed `int`/`float` arithmetic gives `float`.
- **Operators.** `+ - * /`, unary `-`, and `== != < <= > >=`. `+` also joins
  two strings. Strings compare lexicographically.
- **`echo`** prints any value followed by a newline. Floats print in Python
  style with up to 15 significant digits (`3.5`, `66.0`, `1e-05`, `1e+20`);
  `void` prints as `void`.
- **Redefinition.** A later definition with the same name, arity and
  parameter types replaces the earlier one (with a warning). Definitions that
  differ in arity or parameter types are overloads.

## Types and specialization

A static type is a set of runtime kinds: `int`, `float`, `string`,
`boolean`, `void`. `numeric` = {int, float}, `any` = all five.

Functions are specialized per call site argument types, starting from
`main` (like C++ templates or Julia methods). `sum 1 2` and `sum 1.5 2`
produce two machine-code instances. Return types are inferred by fixpoint
iteration, which handles recursion. Unannotated parameters are `any`.

Annotations are constraints: an argument whose type cannot match is a
compile-time `TypeError`; one that *might* match (e.g. a value of type
`int|string` passed to a `numeric` parameter) gets a runtime check.

Overloads are chosen at compile time by arity and by the most specific
matching parameter types. Runtime dispatch between overloads (when the
argument's type is only known at runtime) is not implemented yet.

## Value representation

| Type                          | Machine representation                                  |
|-------------------------------|---------------------------------------------------------|
| `int`, `boolean`, `void`      | `i64` (`boolean` is 0/1, `void` is 0)                   |
| `float`                       | `f64`                                                   |
| `string`                      | pointer to `{ len: i64, bytes... }`                     |
| unions (`numeric`, `any`, …)  | pointer to a box `{ tag: i64, payload: i64 }`           |

Memory comes from a bump allocator over `VirtualAlloc` chunks and is never
freed (there is no garbage collector yet).

## LIR — the intermediate representation (`.lir`)

LIR is a typed SSA IR in the style of LLVM IR and Cranelift CLIF. Machine
types are `i64` and `f64`. Control flow uses basic blocks with **block
parameters** instead of phi nodes. The text form can be printed
(`--emit=ir`) and parsed back (`lunac file.lir`), and the runtime library is
written in it.

For example, `count n = check n: 0 -> 0; else -> 1 + count (n - 1)` called
with an `int` becomes:

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
extern @kernel32.ExitProcess(i64)
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

`extern @dll.Name` declares an imported function (`@kernel32.WriteFile`
imports `WriteFile` from `kernel32.dll`). Every module is checked by
`lir::verify` before code generation. `lir::opt` currently runs dead code
elimination.

## Code generation

The first backend is intentionally simple: each SSA value has its own stack
slot, and each instruction loads its operands into scratch registers,
computes, and stores the result. It uses the Windows x64 calling convention
for all calls, so runtime, program and OS functions interoperate. The
resulting code is correct but slow; a register allocator is the natural next
step and LIR is designed to support one.

`--emit=asm` writes a listing with addresses, bytes and mnemonics.

## Testing

`cargo test` runs unit tests and `tests/cases.rs`, which compiles every
`tests/cases/*.luna`, runs it, compares stdout (and stderr) with the
`.out`/`.err` files, then recompiles the emitted `.lir` and checks it
behaves the same.

## Possible next steps

- register allocation and more LIR optimizations (constant folding, inlining)
- loop syntax, more of the standard library
- runtime overload dispatch on union-typed arguments
- garbage collection
- a Linux x86-64 (ELF) target
