# Luna — to do

Planned work, roughly in the order it makes sense to do it. See
`docs/compiler.md` for how the compiler works today.

## Language

### Functions as values
Pass a function to a function: `map f a`, `filter f a`, `fold f start a`,
sorting with a comparison. The standard library needs this, and it is the
foundation for lazy lists (below): a lazy list is essentially a function
that computes the next element.

- How are functions written as values? (a name like `double`, and maybe
  anonymous functions)
- A function value that uses variables of the function it was made in (a
  closure) must own them: fits the ownership rules?

### Lazy list comprehensions (option B)
A comprehension produces a lazy list: a value that can be stored, passed
and returned, whose elements are computed only when something asks for
them, as in Haskell. Makes infinite lists possible:

```
naturals = [x | x in [0..infinite]]
echo take 5 [x * x | x in naturals, x > 10]
```

Depends on functions as values. To decide:
- the runtime representation: an object holding the state and the code
  for "the next element";
- ownership: a lazy list reads its source later, so it must own it.
  `[x | x in a]` already moves `a`; `[x | x in @a]` borrows it, so a lazy
  version could not outlive `a` (an error, or it becomes eager);
- which operations force the whole list, and what happens on an infinite
  one (`len`, `reverse`, `echo`, `++`): an error, or a run that never ends;
- whether a lazy list remembers the elements it has computed.

(Option A, merging a comprehension that feeds straight into a loop or
another comprehension so no intermediate array is built, could come first
as an optimization; it does not change the language.)

### User-defined types and trails
Let programs define their own types (records), and mark what a type can
do with *trails* (like Rust's traits or Java's interfaces): a trail is a
name, `trail Copy`, and a type declares that it has one with `follow`.
Today the only trail is the built-in `Copy` (`int`, `float`, `boolean`,
`void` and unions of them: copied where other values move, see
`Ty::is_copy`). To decide:
- the syntax for types and for `follow` (on the type's definition, or a
  separate line);
- the rules: a type can follow `Copy` only if all its parts do;
- whether a trail can later require functions (a `Print` or `Eq` that a
  type must provide), and whether trails can be used in signatures
  (`show @x :: Print -> void`), next to `numeric`, `iterable` and `any`;
- the trails `readme.md` will describe.

### Maybe types in signatures
Allow `-> int?` (and `int?` parameters) in type annotations. Maybe types
are inferred today but cannot be written.

## Projects and the standard library

### Modules and multiple files
A Luna project made of several files that use each other's definitions,
to build larger applications. To decide: how a file names what it uses
from another (imports), what is private to a file, and how the files are
found and compiled together. `std.luna` would become a module too.

### Standard library
- More functions once functions as values exist: `map`, `filter`, `fold`,
  `sort`, `any`, `all`, `zip`, ...
- An error inside `std.luna` should also say which call in the program led
  there.

## Compiler

- Free a value at its last use instead of at the end of its owner's
  function (`x = x + "!"` keeps nothing extra alive today, but other
  patterns do).
- Strings have no spare capacity, so `s = s + "!"` builds a new string.
  Since `+` only borrows, it cannot reuse `s`; a `+` that takes its left
  operand (or a string with capacity, like `rt.vec_push` for arrays) could
  append in place.
- `head`, `last` and `!!` on a value that may be an array of strings or a
  string (`[string]|string`) copy the element in the array case, because
  the string case gives a new character either way.
- Deep recursion uses a lot of stack (a frame per call, no tail calls):
  tail calls, or a smaller frame layout.
- Constant global arrays (`a = [1, 2, 3]`) are rebuilt at each use; they
  could live in the executable's data like string literals.
- Register allocation and more LIR optimizations (constant folding,
  inlining).

## Targets

- A Linux (ELF) target: mostly an ELF writer and a `runtime/linux.lir`.
- The Windows (x86-64) target is built and linked by the tests but has not
  been run on Windows since the macOS port; run the test suite there.

## Documentation

`readme.md` (with `readme.zh-CN.md` and `readme.zh-TW.md`, which must be
kept in step with it) describes the language as it is now. To add when
they exist: lazy lists, functions as values, modules, user-defined types
and trails. `tests/cases/readme.luna` runs the readme's examples.
