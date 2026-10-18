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
| driver   | `src/driver.rs`         | runs the stages; reads linked files, joins them into one AST   |
| hir      | `src/hir.rs`            | name resolution, overload groups, `SymbolNotFoundError`        |
| typeck   | `src/typeck.rs`         | type inference and specialization → typed IR (TIR)             |
| ownership| `src/ownership.rs`      | move checking: no value is used after it was moved             |
| consteval| `src/consteval.rs`      | computes constant globals at compile time                      |
| lower    | `src/lower.rs`          | TIR → LIR, value representation, union dispatch, drops         |
| typefns  | `src/typefns.rs`        | per-type LIR functions: free, copy, print, compare             |
| lir      | `src/lir/`              | IR data structures, printer, parser, verifier, optimizations   |
| codegen  | `src/codegen/aarch64/`  | LIR → AArch64 machine code (Apple arm64 calling convention)    |
|          | `src/codegen/x86_64/`   | LIR → x86-64 machine code (Windows x64 calling convention)     |
| link     | `src/link/macho.rs`     | Mach-O writer: segments, dyld binding, ad-hoc code signature   |
|          | `src/link/pe.rs`        | PE32+ writer: sections, import table, relocations              |
| std      | `src/std.luna`          | the standard library, in Luna                                  |
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
- **Loops.** `loop x in [a..b]:` runs the lines below it that are indented
  further than `loop` for `x` = a, a + 1, …, b - 1 (none if `b <= a`). The
  bounds are ints, evaluated once. The loop variable is always an int: it
  can be given other ints (`x = 40`), but giving it any other type is a
  `TypeError`, and each iteration still starts with the next value of the
  range. `x` and the variables first bound in the
  body exist only inside the body, and are new in every iteration; a
  variable from outside that the body updates keeps its value across
  iterations and after the loop. Its type is the union of its types before
  the loop and at the end of the body (and may not mix in `void`). A loop
  over an array or a string takes it (`loop x in a:`, see Ownership), or
  borrows it with `@` (`loop x in @a:`).
- **Top level** holds only definitions; executable code goes in `main`
  (`main = ...`, no arguments, required).
- **Application** binds tighter than operators (`sum a 9 + 1` is
  `(sum a 9) + 1`, and `x + sum xs` is `x + (sum xs)`). `echo` is applied
  the same way and takes one argument: a name or literal, a call
  (`echo sum xs`), or anything else in parentheses. `echo 5 > 6` means
  `(echo 5) > 6`, a type error.
- **Call chains.** When a call has more arguments than any definition of
  the function takes, they are grouped by how many each function accepts,
  reading left to right: `echo sum xs` is `echo (sum xs)`, `head tail a` is
  `head (tail a)`, and `add double 1 3` is `add (double 1) 3`. A call whose
  arguments already fit is never regrouped (`f g x` with a two-parameter `f`
  passes `g` and `x`), and a call that can be grouped in more than one way
  is an error asking for parentheses. (This is not currying: Luna has no
  partial application, so the grouping comes from the functions' parameter
  counts, which name resolution knows.)
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
  two strings. Strings compare lexicographically. Arrays add `++` (between
  comparisons and `+`), `!!` (just below function application) and `**`
  (with `*` and `/`), and compare with `==` element by element.
- **`a ** b`** multiplies two arrays of numbers element by element and
  gives a new array as long as the longer one, whose extra elements are kept
  as they are: `[1, 2, 3] ** [4, 5]` is `[4, 10, 3]`. Like `++`, it takes
  both operands (`copy a ** b` keeps `a`). Each
  element is what `*` gives for its pair (`int` with `float` gives `float`),
  so `[1.5, 2.0] ** [2, 2, 2]` is `[3.0, 4.0, 2]`, of type `[numeric]`.
- **`echo`** prints any value followed by a newline. Floats print in Python
  style with up to 15 significant digits (`3.5`, `66.0`, `1e-05`, `1e+20`,
  `infinite`); `void` prints as `void`; arrays as `[1, 2, 3]`, with strings
  in quotes inside them (`["a", "b"]`).
- **`void`** is the one "nothing" value (`void` in code). A function or
  `check` whose branches give a value or void has a *maybe* type such as
  `int?`, found automatically (`head [] = void` with `head (x:xs) = x`).
  Nothing flows through operators: `int? + 1` is `int?`, and `echo` prints
  `void`. Where a real value is needed (a parameter that does not accept
  void, a range bound, a declared return type), a maybe value is a compile
  error until the void case is handled, with a `check` that has a
  `void ->` arm (or `else`). In the other arms of a `check` on a variable,
  the variable has the plain type: in `check x: void -> 0; else -> f x`,
  `x` is an `int`. `void ->` matches only void.
- **Constants.** `PI` and `infinite` are built-in floats and cannot be
  redefined; `-infinite` is `-` applied to `infinite`.
- **Constant globals.** A global without arguments (other than `main`) whose
  evaluation has no side effect is computed by the compiler, and its uses
  become constants: `foo = "he" + "llo"` is the literal `"hello"`. A global
  with a side effect, such as `banner = echo "welcome"`, stays a function
  and runs at every use. So does one that hits a runtime error, or needs
  more than 1000 nested calls or a million steps to compute.
- **Arrays.** `[1, 2, 3]`; all elements have the same type (`[int]`), or
  that type and void (`[int?]`, as in `[void, 2]`). `[a..b]` is the ints a … b-1 (`[0..5]`).
  `[x * 2 | x in source]` builds an array from a range, an array or a
  string, eagerly. Arrays are immutable: `a ++ b` builds a new array, and
  the elements of both move into it. `a !! i` is element `i` itself, still
  in `a` (a copy if it is a number), or void if `i` is out of range. A string is a sequence of characters
  (Unicode code points, not bytes): `"héllo" !! 1` is `"é"`.
- **Built-in functions** `len`, `head`, `tail`, `init`, `last` and
  `reverse` take an array or a string; `head`, `tail`, `init` and `last`
  give void when it is empty. `len` only reads it, `head` and `last` borrow
  it and give the element itself, and `tail`, `init` and `reverse` take it
  and move its elements into their result. `copy x` gives a copy of any
  value. `reverse` is built in
  because it needs the elements in the opposite order, which no loop or
  comprehension gives. A program that defines a function with the
  same name uses its own instead.
- **Standard library.** `src/std.luna` is written in Luna and compiled
  together with every program (only what a program uses reaches its
  executable): `abs max min sum product average maximum minimum contains
  count index_of remove take drop flatten is_empty join split repeat`. A program that defines a name the library defines uses its own
  definitions instead; the library keeps calling its own (they are renamed
  to `std.name`, which no program can write). Library functions that only
  read an array or a string borrow it (`sum @a`); those that build their
  result from its elements take it (`take`, `drop`, `flatten`, `remove`,
  `maximum`, `minimum`). Errors inside the
  library point into `std.luna`: every file gets its own range of byte
  offsets (`diag::Sources`), so a span tells which file it is in.
- **List comprehensions** take clauses after `|`: generators and
  conditions, starting with a generator. `[x | x in a, x > 0]` keeps the
  elements for which every condition (a `boolean`) is true. With several
  generators, each runs once for every element of the ones before it and can
  use their variables: `[y | part in a, y in part]` flattens,
  `[x * y | x in a, y in b]` is a cross product. Like a loop, a generator
  takes what it goes through, or borrows it with `@` (`x in @a`). The first
  generator's source is evaluated once; a later one's once per element of
  the generator before it, so it may only move that generator's variables
  (`part` in `y in part`). The body may only move the
  last generator's variables (the others are reused). With one generator the
  result is allocated up front; otherwise it grows in a buffer that doubles
  (`rt.vec_push`).
- **Positions.** `loop i, x in a:` and `[… | i, x in a]` name each
  element's position (0, 1, …) along with the element: the character
  position for a string, the index for a range. The position is an int and
  stays one. This is how to work with positions without `!!`, whose result is
  a maybe value: `take n a = [x | i, x in a, i < n]`.
- **Array patterns.** A parameter `(x:xs)` takes an array of at least one
  element apart (`(x:y:xs)`: at least two); `[]` matches an empty array.
  Definitions with patterns are alternatives tried top to bottom (see
  Overloads), with the length tested at run time. The function owns the
  array, so its elements are moved into `x` and `xs` without copying. A
  group with `(x:xs)` but nothing for `[]` gets a warning: calling it with an
  empty array stops the program.
- **`ret value`** (or bare `ret`) leaves the function early, from anywhere,
  including inside loops and `check` arms; its value joins the function's
  result type (a bare `ret` adds void). It cannot be used inside a list
  comprehension.
- **Redefinition.** A later definition with the same name, arity and
  parameter types replaces the earlier one (with a warning). Definitions that
  differ in arity or parameter types are overloads.

## Multiple files

A program can be several files. The file given to `lunac` is the main
file; `| path` lines at the top of a file link others (see the readme):

- `| entity/try > try` links `entity/try.luna`, whose definitions are then
  written `try.pred` (without `>`, the name is the path's last part).
  `| entity` means `entity.luna`, or `entity/entity.luna` for a folder of
  the same name (both existing is an error).
- `| entity#pred, next > p, n` links only `pred` and `next`, written without
  a prefix, here under the names `p` and `n`.
- Paths always start at the main file's folder, also in linked files. Each
  file is read once, so files may link each other in any way, cycles
  included.

The lexer reads `et.pred` as one name, and a `#` right after the path of a
link line as `#` (anywhere else it starts a comment).

**How it works.** The driver reads all the files first, following the links
(`driver::frontend_files`; the file system is behind a `Loader`, so tests
use files in memory). Then it joins their definitions into one program,
where every file but the main one names its definitions after the file:
`pred` in `entity/try.luna` becomes `entity/try.pred`, a name no program can
write (`/` is division), so files never clash. Each file also gets a table,
`hir::FileNames`, of what its names mean:

- its own definitions;
- the files it links whole (`et` → the definitions of `entity/entity.luna`);
- the definitions it links by name;
- in a linked file, the main file's definitions are hidden: their names
  mean the standard library's function (or the built-in one) of that name,
  if there is one, and are otherwise unknown;
- every other name is looked up as before (the library, built-ins).

Name resolution (`hir`) looks every global name up in the table of the
file the definition came from (`Def::file`). After that, nothing knows
there were several files: types, ownership and lowering see one program.
Symbols in LIR use `.` for `/` (`entity.try.pred`). Every file has its own
range of byte offsets (`diag::Sources`), so errors point into the right
file.

## Ownership

Every value has exactly one owner: the local variable or parameter that
holds it. The design follows Rust: values move by default, borrowing is
marked, and nothing is copied unless the program asks.

- **Moves.** Binding (`y = x`), passing an argument (`f x`), returning (the
  last expression, `ret`), putting a value in an array (`[x]`), the result
  of a `check` arm, and the operations that build something new out of a
  value's parts: `a ++ b`, `a ** b`, `tail a`, `init a`, `reverse a`, and a
  loop or list comprehension going through `a`. The old name is invalid
  afterwards, and using it is an `OwnershipError`. Moving copies nothing:
  the new owner gets the same object. A `check` arm always moves, even
  inside `echo (check ...)`, as with Rust's `if`. `id a = a` is fine: the
  callee owns its parameter and returns it, and `wrap a = [a]` gives the
  caller one value, the array, which owns `a`.
- **Borrows.** Operators that compute a new value (`+`, `==`, …), `echo`,
  `len`, `copy`, the scrutinee and patterns of `check`, a parameter marked
  `@` (`sum @a = …`), and a loop or comprehension over `@a`. The owner keeps
  its value. Borrowing copies nothing either.
- **Borrowed values** cannot be given away, because they still belong to
  someone else: a `@` parameter inside its function, the variable of a loop
  over `@a`, and the elements `head a`, `last a` and `a !! i`, which are the
  elements themselves, still inside `a`. They can be read, and lent on to
  another `@` parameter (`total @a = sum a`). To keep one, the program asks
  for a copy: `copy a`, `copy (head a)`. A borrowed parameter or loop
  variable cannot get a new value either (it is immutable).
- **`Copy` types.** `int`, `float`, `boolean`, `void`, and unions of them
  (`numeric`, `int?`) are copied where other values move, so the old name
  stays valid and a borrowed one can be given away. Copying one costs no
  more than moving it (`Ty::is_copy`). Strings and arrays (and unions with
  them) are never copied implicitly. This is the built-in `Copy` trail;
  user-defined types and trails are planned (`todo.md`).
- **Overloads.** All definitions with the same name and number of
  parameters must borrow the same ones: a call cannot tell in advance which
  definition it reaches, yet it must know whether its arguments move.
- **While borrowed.** An owner cannot be moved, or given a new value (which
  frees the old one), while something borrowed from it is in use: `f (head
  a) a` with `f @x y` is an error (the second argument would move `a` while
  `f` reads its element), and so is `a == g a`, or `a = …` in the body of
  `loop x in @a`. The checker keeps the owners borrowed by the operands of
  the operation being evaluated (and by enclosing loops over `@a`) on a
  list, which each operation truncates when it is done.
- **New values.** `x = …` makes `x` usable again after a move, and frees
  the value `x` still held, if any.
- **Branches.** After a `check`, a name counts as moved if any arm moved it
  (arms that leave the function with `ret` do not count).
- **Loops.** A loop body must leave every outside variable as it found it:
  moving `s` inside the body is an error unless `s` gets a new value before
  the body ends (otherwise the next iteration would find it empty), and a
  variable moved before the loop cannot get a value inside it (whether it
  held one afterwards would depend on how often the loop ran). So the state
  after a loop never depends on the number of iterations. What a loop goes
  through is evaluated once, before the loop, so `loop x in a` moves `a`
  for good.
- Arguments are evaluated left to right, so `f x x` moves `x` and then uses
  it: an error for strings, fine for ints.

Because copying depends on the type, the check runs per specialized
instance, after type checking. Like type errors, ownership errors are only
found in functions reachable from `main`.

**Drops.** Lowering frees every heap value exactly once, with no reference
counts or runtime flags:

- an owner (parameter or local) that still holds its value when the
  function ends is dropped there; a moved value is dropped by its new owner;
  a `@` parameter is never dropped by the callee;
- a temporary that is only read (`echo (a + b)`, an operand, a `check`
  scrutinee, a borrowed argument) is dropped right after that read; a
  discarded statement value is dropped at once;
- a borrowed value (`TExpr::is_borrowed`) is never dropped by its reader.
  `head a`, `last a` and `a !! i` on an array of strings or arrays give the
  element itself (boxed on the stack if the result is a maybe type), so a
  temporary array they come from (`echo (head (f x))`) must outlive the
  element: its drop is *pending* until the reader is done (`release`);
- a loop that owns what it goes through moves each element out into the
  variable and leaves an immortal placeholder in the slot (the empty object
  `{ len 0 }` for strings and arrays, the void box for boxes); at the end
  only the array itself is freed;
- `ret` drops the owners, and every pending value: a loop's source, a
  `check`'s scrutinee, an array whose element is in use. Thanks to the
  placeholders, dropping a half-consumed source frees exactly the elements
  not taken yet;
- when only some `check` arms move an owner, the other arms drop it before
  joining, so every path leaves the same owners behind;
- unboxing an owned value (`numeric` → `int`) frees the box; unboxing a
  borrowed one only reads it.

Dropping a string calls `rt.free`; arrays and boxes get a drop function
per type (`typefns`), which frees what they own and then themselves. The
runtime counts live objects (`rt.live`); the `no_leaks` test runs every
test program and checks the count is 0 at exit.

## Types and specialization

A static type is a set of runtime kinds: `int`, `float`, `string`,
`boolean`, `void`, `array`, plus the element type of arrays (`[int]`,
`[[string]]`). `numeric` = {int, float}, `iterable` = {string, array},
`array` is any array, `any` = everything. A union with void is a maybe
type, shown as `int?`. Element types are stored once per compilation and
shared, so a type stays a small copyable value.

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
test a :: numeric -> void = echo "winning"
test _ = echo "nothing left"      # everything that is not numeric
```

Usually the argument types decide this at compile time. When an argument
has a mixed type (`int|string`), a definition may accept only some of its
values, and the program tests the value's kind at run time, trying the
definitions in the same order (`TExprKind::Dispatch`). Array patterns add a
runtime length test; `(x:xs)` together with `[]` covers every array. If no
definition accepts the values, the program stops with `TypeError: no
definition of `test` accepts these arguments`. A definition that an earlier
one always matches first gets a warning, since it can never run. A value
that may be void must reach a definition that accepts void, or the call is
a compile error.

## Value representation

| Type                          | Machine representation                                  |
|-------------------------------|---------------------------------------------------------|
| `int`, `boolean`, `void`      | `i64` (`boolean` is 0/1, `void` is 0)                   |
| `float`                       | `f64`                                                   |
| `string`                      | pointer to `{ len: i64, bytes... }`                     |
| arrays (`[int]`, …)           | pointer to `{ len: i64, elements... }`, 8 bytes each    |
| unions (`numeric`, `int?`, …) | pointer to a box `{ tag: i64, payload: i64 }`           |

Box tags: 1 int, 2 float, 3 string, 4 boolean, 5 void, 6 array. Every boxed
void is the same immortal box in the runtime's data (`rt.void_box`): there
is one void, and it is never allocated or freed. Freeing, copying, printing
and comparing an array or a box depend on what it holds, so the compiler
generates those functions once per type (`src/typefns.rs`, e.g.
`drop.[string]`), like C++ templates or Rust's `Drop`/`Clone`.

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
- lazy list comprehensions (infinite lists such as `[x | x in [0..infinite]]`)
- writing maybe types in signatures (`-> int?`)
- a Linux (ELF) target; most of the work is an ELF writer and a
  `runtime/linux.lir` making system calls directly
