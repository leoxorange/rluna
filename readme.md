# About Luna

English | [简体中文](readme.zh-CN.md) | [繁體中文](readme.zh-TW.md)

## chapter 0. about luna

luna is a haskell-like language. The key concept of luna is to use as few keywords as possible, so that everything you write looks like a formula. The data goes through the pipeline you designed for it and turns into the shape you want. That's how this language works, and it is why luna does some things differently from other languages.

### using the compiler

`lunac` compiles a `.luna` file into an executable. It needs nothing else: no assembler, no linker, no LLVM.

```
lunac hello.luna                       # builds the executable `hello`
lunac hello.luna -o out -r             # builds `out` and runs it
lunac hello.luna --target=windows-x64  # builds hello.exe for 64-bit Windows
lunac hello.luna --emit=ast,ir,asm     # also writes the syntax tree, the IR and the machine code
```

The targets are `macos-arm64` (Apple silicon Macs) and `windows-x64`. Without `--target`, lunac builds for the system it runs on.

## chapter 1. basic syntax

### calculation

```
echo (5 > 6) # false
echo (5 == 5) # true
echo (4 < 10) # true
foo :: boolean = 5 > 6 # foo is false
echo (7 / 2) # 3.5, `/` always gives a float
echo (7 * 2) # 14, int with int stays an int for + - *
echo (1 + 0.5) # 1.5, an int with a float gives a float
```

`+` also joins two strings: `"con" + "cat"` is `"concat"`. Strings compare with `<` and `>` in alphabetical order.

### symbol definition

```
sum = 1
foo = 10 + 8 * 7 # foo is 66, The space is not necessary, this is just for beauty
echo foo # foo is 66
foo = (5 + 1)*6 # Now foo becomes 36, we can easily redefine foo by doing this
echo foo # foo is 36
```
This is not only for variable definition. This language works like haskell: the sum here can also be treated as a function, which takes no arguments but always returns 1. Every calculation step follows the four fundamental operations of arithmetic; we don't have more operations for advanced calculations in the alpha version.

A definition starts in column 1. Redefining a symbol replaces the earlier definition (the compiler warns you about it). A symbol whose value is constant, like `foo` above, is computed while compiling, so using it costs nothing. A symbol that does something, like `banner = echo "welcome"`, runs every time it is used.

Using **#** to start a line means a single line comment.

and we have some special internal symbols for easy use.
```
PI # 3.141592654...
infinite # every number is smaller than it
-infinite # every number is greater than it
```
They cannot be redefined.

### array definition

this part i want haskell style.
Array positions start from 0 like in other languages, and all arrays are immutable. All the elements of an array have the same type.
```
a = [1, 2, 3] # this is an array
b = a ++ [4, 5] # this will create a new array [1,2,3,4,5] instead of changing the old one.
c = [1, 2, 3] ** [4, 5] # [4, 10, 3]: multiplies element by element, the longer array keeps its extra elements
d = [0..5] # this will create an array [0, 1, 2, 3, 4]
```

`++` and `**` take the arrays they are given: their elements move into the new array, so nothing is copied, and `a` cannot be used after `b = a ++ [4, 5]`. Chapter 3 explains this, and how to keep `a`.

#### list comprehension

A list comprehension builds an array by its rules:
```
a = [x | x in [0..5]] # [0, 1, 2, 3, 4]
a = [x * 2 | x in [0..5]] # [0, 2, 4, 6, 8]
a = [x * x| x in [0..5]] # [0, 1, 4, 9, 16]

# a is [0, 1, 4, 9, 16]
echo (a !! 4) # this will grab the pos 4 in array a which will be 16
echo (a !! 9) # void: there is no position 9
```

After the `|` come generators (`x in ...`) and conditions:
```
big = [x | x in [0..10], x > 5] # [6, 7, 8, 9]: only the elements for which the condition is true
pairs = [x * y | x in [1..3], y in [10..12]] # [10, 11, 20, 22]: every x with every y
flat = [y | part in [[1, 2], [3]], y in part] # [1, 2, 3]: a later generator can use an earlier one
firsts = [x | i, x in [5, 6, 7, 8], i < 2] # [5, 6]: `i, x` also names the position of x
chars = [c | c in "héllo"] # ["h", "é", "l", "l", "o"]: a string is a sequence of characters
```
For now a list comprehension builds the whole array at once. Lazy lists, whose elements are only computed when they are needed, will come later.

array internal functions see the next chapter.

#### array patterns

and now we have array, so we have a new argument declaration.
```
# this is an example for showing how internal function head works
head (x:xs) = x
head [] = void
```
(x:xs) is a special argument pattern for matching any array with at least one element: x stands for the first element and xs stands for the rest. With a one-element array, xs is `[]`. You can also write (x:y:xs): now x still stands for the first one, y stands for the second one, and xs stands for the rest.

head [] will match an empty array and do nothing.

The definitions are tried from top to bottom, so this works too:
```
sum (x:xs) = x + sum xs
sum [] = 0
```
If a function takes arrays apart with (x:xs) but has no definition for `[]`, the compiler warns you: calling it with an empty array would stop the program.

### conditions

process control is necessary for any language, so we have a powerful system for this situation.
```
foo = 5
pred a = a - 1
test _ = check _: # _ is a hidden argument symbol for the function arguments you don't want to give a real name, and it only works in this function
    1 -> echo "success";
    else -> test (pred _)

main = test foo
```
the check keyword works like match in kotlin or rust. the code above will loop 5 times to reach the success output.
```
check [enumerable types | numeric | string]:
    condition1 -> [expression];
    condition2 -> [expression];
    else -> [expression]
```
the check body must be indented: each branch on its own line, separated by `;`.
the **else** branch is always required if it is checking a numeric or a string, or if some values are not covered by the branches above. A check on a boolean that has both `true ->` and `false ->` needs no else:
```
sign n = check n < 0:
    true -> "negative";
    false -> "not negative"
```
A check can be nested inside a branch, with its branches indented further.

### loop

recursion is expensive especially for those deep ones, so here's a simple way to repeat something.
```
main =
    loop x in [0..5]: # [0..5] means 0, 1, 2, 3, 4
        echo x
    total = 0
    loop x in [1, 2, 3]: # a loop can go through an array
        total = total + x
    echo total # 6
    loop i, c in "abc": # or a string; `i, c` also names the position
        echo i # 0, 1, 2
```
the loop body must be indented.

- Variables created in the body only exist inside it, and are new in every round.
- A variable from outside that the body changes (`total` above) keeps its value from one round to the next, and after the loop.
- The variable of a loop over a range is always an int: you can give it another int (`x = 40`), but giving it a string is an error.

### function definition and call

```
b = 10
sum a b = a + b
summon a = check a:
    1 -> summon (sum a 9);
    10 -> echo "summon successfully";
    else -> echo "summon failed"

main =
    summon 1 # will output summon successfully
    summon 2 # will output summon failed
```
Now sum takes two arguments **a** and **b** and returns a plus b.
The function body ends when another definition starts.
As you can see we already have a b above the sum, but sum still has an argument b here, and in its body it uses the b symbol. This is called "Name Shadow", (global scope(arguments scope(function body scope))): the symbol search tries to find a symbol from the center to the global scope, layer by layer, and if it can not find the symbol after all the searching, it throws an error called "SymbolNotFoundError".
And functions can have multiple definitions (overloads), with different amounts of arguments or different type definitions.

The **main** function will be used as the main entrance like other languages, so lacking a main function is an error. The example code here is just for showing, not real project code.

And since we have overloads, here's a new way to play with function definitions.
```
test a :: numeric -> void = echo "winning"
test _ = echo "nothing left"
```
this function has two definitions, so test 1 will output winning but other arguments will get nothing left; this can be seen as pattern matching. the compiler searches from top to bottom to find the correct definition for the caller, and test _ catches everything else.

#### calls

A call binds tighter than any operator: `sum a 9 + 1` is `(sum a 9) + 1`.

When a call has more arguments than the function takes, the extra ones are grouped by how many arguments each function takes, reading from left to right:
```
echo sum [x | x in [1..101]] # echo (sum [...]): 5050
echo head tail [1, 2, 3] # echo (head (tail [1, 2, 3])): 2
```
If the arguments can be grouped in more than one way, the compiler asks you to add parentheses.

`echo` takes one argument: a name, a value, a call (like `echo sum xs`), or anything in parentheses. So `echo (5 > 6)` needs its parentheses.

#### ret

The last line of a function is its return value. `ret value` leaves the function early, from anywhere (even inside a loop), and bare `ret` leaves with void:
```
first_negative a =
    loop x in a:
        check x < 0:
            true -> ret x;
            false -> void
    void

main = echo (first_negative [3, -1, 4]) # -1
```
`ret` cannot be used inside a list comprehension.

### type definition

we now have these embedded types:

- any (this is the basic type, all other types extend it)
- iterable (string, array or vector(come later))
- array (for all array types)
- numeric (for all numbers like integer and float)
- int (for integer)
- float
- string (for char sequences)
- boolean (true and false)
- void (like null in other languages)

```
sum a b :: numeric, numeric -> numeric = a + b + 5 # correct
sum "hello" 5 # error, "hello" is not a number
```

the last line of any function will be treated as the return value, and its type will be seen as the return type. or you can use the ret keyword to return early.
you can use the following code to define a function that returns void.
```
foo _ :: any -> void = 5
```
although you write a 5 here, nothing will be returned since this function has a void return type. The 5 will be discarded when exiting this function.



#### void and maybe types

void is the one "nothing" value, and `echo` prints it as `void`. A function that gives a value on some paths and void on others has a *maybe* type: `head [1, 2]` is an `int?`, because head gives void for an empty array. The compiler finds maybe types by itself.

- void flows through operators: `head [] + 1` is void.
- Where a real value is needed (an argument that does not accept void, the end of a range, a declared return type), a maybe value is an error until you handle the void case with a check:
```
first_or_zero a =
    h = head a
    check h:
        void -> 0;
        else -> h # here h is an int, not an int?
```
`void ->` only matches void. Maybe types cannot be written in type definitions yet.

### multiple files and how to link them

sometimes you can not write all the codes into single file, so here's the solution for a large project.

for example:

```
src - main.luna
    - entity
        - try.luna
        - entity.luna
    - test
        - test.luna
```
in entity.luna
```
pred a = a - 1
process a = a + 10
next a = a + 1
```
now you want to use the pred in main.luna
```
# the search root is where the main entry file is, in this example, src will be the root path.
# since entity has the same name of the directory, you can only write the entity here
| entity > et # this must be placed at the top of the file, et is an alias for entity and its optional.
# but for try.luna, you have to write the full path
| entity/try > try 

echo et.pred 5 # get 4
```
for partically link
```
| entity#pred, next > # only links pred and next
# if you want to give them alias
| entity#pred, next > p, n # the alias will match the read symbols pair by pair

echo pred 5 # get 4
```

you are no need to write all files when using ``lunac``, you only provides the main.luna(or other file contains the ``main`` entry), compiler will find others automatically.

a few more rules:

- without `>`, a whole file is used under its file name: after `| entity/try`, write `try.pred`.
- each file only sees its own definitions, the files it links, and the standard library. So two files can define the same name, and a linked file does not see the definitions of main.luna unless it links main.luna.
- a linked file can link other files too (its paths also start at the folder of main.luna), and files may link each other.
- a name defined in a file cannot also be linked into it by name: give the linked one another name with `>`.

## chapter 2. internal values and functions

since we have numeric, string and array, now we need some internal functions to help us do more things
```
a = [1, 2, 3]
hi = "hello"
main =
    echo (len a) # 3
    echo (head a) # 1
    echo (tail a) # [2, 3]
    echo (len hi) # 5
    echo (head hi) # "h"
    echo (tail hi) # "ello"
    echo (init a) # [1, 2]
    echo (last a) # 3
    echo (init hi) # "hell"
    echo (last hi) # "o"
    echo (reverse a) # [3, 2, 1]
    echo (reverse hi) # "olleh"
    echo (head []) # void
    echo (copy a) # [1, 2, 3]
```
len returns the length of an iterable object.
head grabs the first element in an iterable object, tail grabs the rest.
init grabs all the elements except the last one, and last only grabs the last one.
reverse gives the elements in the opposite order.
head, tail, init and last give void when there is no element.
copy gives a copy of any value (see chapter 3).

A string is a sequence of characters, not bytes: `len "héllo"` is 5 and `"héllo" !! 1` is `"é"`.

A program that defines a function with the same name as an internal one (like `head (x:xs) = x` above) uses its own.

## chapter 3. ownership

luna has no garbage collector, and you never free memory yourself. Instead, every value has exactly one owner: the variable that holds it. When the owner is done (its function ends, or the round of the loop it lives in), the value is freed. This works like Rust.

### moving

Giving a value to someone else **moves** it, and the old name can no longer be used:
```
main =
    s = "hello"
    t = s # the string moves to t
    echo t # hello
    echo s # error: `s` is used after its value was moved
```
A value moves when you:

- bind it to another name (`t = s`)
- pass it to a function (`f s`)
- return it, or put it in an array (`[s]`)
- give it to `++`, `**`, `tail`, `init`, `reverse`, or to a loop or list comprehension that goes through it

A move copies nothing: the new owner gets the very same value. So `x = x ++ [4]` adds to the old array without copying it. Giving a name a new value (`s = "again"`) makes it usable again.

Numbers are different: `int`, `float`, `boolean` and `void` values (and mixes of them, like `numeric`) are **copied** instead, because copying a number costs no more than moving it. They follow the `Copy` *trail*, a mark that says what a type can do:
```
n = 5
m = n
echo (n + m) # 10, n is still usable
```

### borrowing

A parameter marked with `@` only **borrows** its argument: the caller keeps it.
```
greet @name = "hello " + name

main =
    s = "luna"
    echo (greet s) # hello luna
    echo s # luna: greet only borrowed s
```
Inside the function, a borrowed parameter still belongs to the caller, so it is read-only:

- you can read it, and lend it on to another `@` parameter: `total @a = sum a` is fine, because `sum` borrows too
- you cannot give it a new value: `@a` with `a = 1` in the body is an error
- you cannot give it away: `keep @a = a` and `wrap @a = [a]` are errors

To keep something borrowed, ask for a copy: `keep @a = copy a`. `copy` is the only way luna copies a string or an array, so you always see where copies happen.

All the definitions of a function with the same number of arguments must borrow the same ones, because a call cannot know in advance which definition it will reach.

### loops

A loop over an array takes it: each element moves into the loop variable. To keep the array, borrow it with `@`:
```
main =
    names = ["ann", "bob"]
    loop n in @names: # borrows names
        echo n
    echo names # still usable
    loop n in names: # takes names: each element moves into n
        echo (n + "!")
```
List comprehensions work the same way: `[len w | w in @names]` borrows `names`, `[w + "!" | w in names]` takes it. The variable of a loop over `@names` is a borrowed element: it cannot get a new value or be given away.

### elements

`head a`, `last a` and `a !! i` give you the element itself, still inside `a`, not a copy. You can read it, but to keep it you need a copy:
```
main =
    names = ["ann", "bob"]
    echo (head names) # fine: only reading
    first = head names # error: the element is still inside names
    first = copy (head names) # fine
```
Numbers are copied as usual, so `x = head [1, 2]` is fine.

### while something is borrowed

While something is borrowed, its owner must keep it. You cannot move it, or give it a new value, until the borrow is over:
```
f @x y = x + y

main =
    s = "x"
    echo (f s s) # error: the second s would move s while f borrows it
    names = ["ann"]
    loop n in @names:
        names = [] # error: names is borrowed by the loop
```

Trails and user-defined types (types you make yourself) will come later; for now `Copy` is the only trail, and it is built in.

## chapter 4. standard library

These functions are written in luna itself (`src/std.luna`) and come with every program; only the ones a program uses end up in its executable. A program that defines a function with the same name uses its own definition instead.

| function | what it gives | its array or string |
|---|---|---|
| `abs x`, `max a b`, `min a b` | the distance from zero, the larger, the smaller number | |
| `sum a`, `product a` | the sum (0 if empty), the product (1 if empty) | borrowed |
| `average a` | the average as a float, or void if empty | borrowed |
| `maximum a`, `minimum a` | the largest, the smallest element, or void if empty | taken |
| `contains a x`, `count a x` | whether `a` has an element equal to `x`, how many | borrowed |
| `index_of a x` | the position of the first element equal to `x`, or void | borrowed |
| `remove a x` | the elements not equal to `x` | taken |
| `take n a`, `drop n a` | the first `n` elements, the elements after them | taken |
| `flatten a` | the arrays in `a` joined: `flatten [[1], [2, 3]]` is `[1, 2, 3]` | taken |
| `is_empty a` | whether there are no elements | borrowed |
| `join a sep` | the strings joined with `sep`: `join ["a", "b"] ", "` is `"a, b"` | borrowed |
| `split s sep` | the parts between the characters `sep`: `split "a,b" ","` is `["a", "b"]` | borrowed |
| `repeat s n` | `s` repeated `n` times | borrowed |

"borrowed" means the caller keeps the array (`@a`); "taken" means it moves in, because the result is made of its elements.
