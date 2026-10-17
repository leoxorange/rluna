# About Luna

## chatper 0. about luna

luna is a haskell-like language, the key concept of luna is we use as much less as keywords to simplify our languages so everything you write will be like a formula. The data goes through the pipeline which you designed for them and turns into the shape you want. That's how this language works. So, based on the above, we have something different from other languages.

## chapter 1. basic syntax

- calculation

```
echo 5 > 6 # false
echo 5 == 5 # true
echo 4 < 10 # true
foo :: boolean = 5 > 6 # foo is false
```


- symbol definition

```
sum = 1
foo = 10 + 8 * 7 # foo is 66, The space is not necessory, this is just for beautiful
echo foo # foo is 66
foo = (5 + 1)*6 # Now foo goes into 36, and we can easily redefine the foo by doing this
echo foo # foo is 36
```
This is not only for variable definition, this language works like haskell, the sum here can alse be treated as a function, it takes no arguements but always return 1. Every calculation step follows the four fundamental operations of arithmetic, we don't have more operations for advanced calculations in alpha version.

Using **#** to start a line means a single line comment.

- conditions

process contorl is necessory for any languages, so we have a powerful system for this situation.
```
foo = 5
pred a = a - 1
test _ = check _: # _ is a hidden arguement symbol for the function arguements you dont want give it a real name, and it only works in this function
1 -> echo "success";
else -> test (pred _)

test foo
```
check keywords works like match in kotlin or rust. the code above will loop 5 times to reach the success output.
```
check [enumerable types | numeric | string]:
condition1 -> [expression];
condition2 -> [expression];
else -> [expression]
```
the **else** branch always be required if it is checking a numeric, string or we still have some conditions that have not be covered by the branch above.

- function definition and call

```
b = 10
sum a b = a + b
summon a = check a:
1 -> summon (sum a 9);
10 -> echo "summon successfully";
else -> echo "summon failed"

summon 1 # will output summon successfully
summon 2 # will output summon failed
```
Now sum takes two arguements **a** and **b** and returns the a plus b.
The function body will ends when another definition starts.
As you can see we already have a b above the sum, but sum still have an arguement b here, and in it's body it uses the b symbol, this called "Name Shadow", (global scope(arguments scope(function body scope)), the symbol search will try to find a symbol from the center to global layer by layer and if it can not find the symbol after all the searching, it will throw an error called "SymbolNotFoundError".
And functions can have multiple definition(override) but needs different amount of arguements, or different type definition.

The **main** function will be used as main entrance like other languages, so lack of main function is an error. The example code here is just for showing not the real project code.

- type definition
  
we now have these embedded types:

1. numeric(for all numbers like integer and float)
2. int(for integer) 
3. float
4. string(for char sequences)
5. boolean(true and false)
6. void(like null in other languages)
7. any(this is basic type, all other type extends this)

```
sum a b :: numeric, numeric -> numeric = a + b + 5 # correct
sum "hello" 5 # error, "hello" is not a number
```

the last line of any functions will be treated as return value, and it's type will be seen as return type.
you can use the following code to define a function that returns void.
```
foo _ :: any -> void = 5
```
although you write a 5 here, but nothing will be returned since this function has a void return type. The 5 will be discard when exit this function.