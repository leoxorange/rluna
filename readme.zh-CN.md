# 关于 Luna

[English](readme.md) | 简体中文 | [繁體中文](readme.zh-TW.md)

## 第 0 章 关于 luna

luna 是一门类似 haskell 的语言。它的核心理念是尽可能少用关键字，让你写下的一切看起来都像一个公式。数据流过你为它设计的管道，最终变成你想要的形状。这就是这门语言的工作方式，也正因如此，luna 有一些与其他语言不同的地方。

### 使用编译器

`lunac` 把一个 `.luna` 文件编译成可执行文件。它不依赖任何其他工具：不需要汇编器、链接器，也不需要 LLVM。

```
lunac hello.luna                       # 生成可执行文件 `hello`
lunac hello.luna -o out -r             # 生成 `out` 并运行它
lunac sum.luna -r -- a.txt b.txt       # 运行它，并传给它参数 a.txt 和 b.txt
lunac hello.luna --target=windows-x64  # 生成 64 位 Windows 程序 hello.exe
lunac hello.luna --emit=ast,ir,asm     # 同时输出语法树、中间表示（IR）和机器码
```

目标平台有 `macos-arm64`（Apple 芯片的 Mac）和 `windows-x64`。不写 `--target` 时，lunac 为它当前运行的系统编译。

### 编辑器支持

`editors/vscode` 是一个 Visual Studio Code 扩展，可以为 luna 代码提供语法高亮。使用时，在这个仓库的根目录把它链接到 VS Code 的扩展文件夹，然后重新加载 VS Code（Windows 和其他方式见 `editors/vscode/README.md`）：

```
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/luna
```

## 第 1 章 基本语法

### 计算

```
echo (5 > 6) # false
echo (5 == 5) # true
echo (4 < 10) # true
foo :: Boolean = 5 > 6 # foo 是 false
echo (7 / 2) # 3.5，`/` 的结果总是浮点数
echo (7 * 2) # 14，整数与整数做 + - * 仍是整数
echo (1 + 0.5) # 1.5，整数与浮点数运算得到浮点数
echo (7 % 3) # 1，余数：`%` 用于两个整数
echo (-7 % 3) # 2，余数的符号与右边相同，就像 haskell 的 mod
echo (1 < 2 && 2 < 3) # true：两边都为真
echo (5 > 6 || 4 < 10) # true：有一边为真
echo (!(5 > 6)) # true：`!` 把 false 变成 true，把 true 变成 false
```

`+` 也可以连接两个字符串：`"con" + "cat"` 是 `"concat"`。字符串之间用 `<` 和 `>` 按字母顺序比较。

`&&` 和 `||` 接受两个布尔值，优先级低于比较运算符，`&&` 又高于 `||`：`a || b && c` 就是 `a || (b && c)`。只有当左边不能决定结果时，才会计算右边：在 `x != 0 && 10 / x > 1` 中，只有 `x` 不是 0 时才会做除法。`!b` 把一个布尔值反过来。它的优先级和 `-` 一样，高于其他运算符：`!a && b` 就是 `(!a) && b`，`!even 3` 就是 `!(even 3)`。（`!!` 用来取数组的元素，所以取反两次要写成 `!(!b)`。）

`%` 的优先级和 `*`、`/` 一样：`1 + 10 % 4 * 3` 是 7。它的结果在 0 和右边之间，所以即使 `i` 是 0，`(i - 1) % 12` 也仍然落在钟面上。`x % 0` 会以 `ZeroDivisionError` 结束程序。

### 符号定义

```
sum = 1
foo = 10 + 8 * 7 # foo 是 66，空格不是必需的，只是为了好看
echo foo # foo 是 66
foo = (5 + 1)*6 # 现在 foo 变成 36，这样就能轻松地重新定义 foo
echo foo # foo 是 36
```
这不仅仅是变量定义。这门语言像 haskell 一样工作：这里的 sum 也可以看作一个函数，它不接受参数，但总是返回 1。每一步计算都遵循四则运算；在 alpha 版本中还没有更高级的运算。

定义从第 1 列开始。重新定义一个符号会替换之前的定义（编译器会给出警告）。值是常量的符号（比如上面的 `foo`）在编译时就会算好，使用它没有任何开销。会做事情的符号（比如 `banner = echo "welcome"`）则在每次使用时都会执行。

以 **#** 开头表示单行注释。

我们还有一些方便使用的内置符号：
```
PI # 3.141592654...
infinite # 所有数都比它小
-infinite # 所有数都比它大
```
它们不能被重新定义。

### 数组定义

这部分我想要 haskell 的风格。
和其他语言一样，数组位置从 0 开始，并且所有数组都是不可变的。一个数组的所有元素类型相同。
```
a = [1, 2, 3] # 这是一个数组
b = a ++ [4, 5] # 这会创建一个新数组 [1,2,3,4,5]，而不是修改旧数组
c = [1, 2, 3] ** [4, 5] # [4, 10, 3]：逐个元素相乘，较长数组多出的元素保留下来
d = [0..5] # 这会创建数组 [0, 1, 2, 3, 4]
```

`++` 和 `**` 会拿走交给它们的数组：元素被移动到新数组里，所以没有任何复制，执行 `b = a ++ [4, 5]` 之后 `a` 就不能再用了。第 3 章会解释这一点，以及如何保留 `a`。

#### 列表推导式

列表推导式按规则构造一个数组：
```
a = [x | x in [0..5]] # [0, 1, 2, 3, 4]
a = [x * 2 | x in [0..5]] # [0, 2, 4, 6, 8]
a = [x * x| x in [0..5]] # [0, 1, 4, 9, 16]

# a 是 [0, 1, 4, 9, 16]
echo (a !! 4) # 取出数组 a 中位置 4 的元素，也就是 16
echo (a !! 9) # void：没有位置 9
```

`|` 后面是生成器（`x in ...`）和条件：
```
big = [x | x in [0..10], x > 5] # [6, 7, 8, 9]：只保留条件为真的元素
pairs = [x * y | x in [1..3], y in [10..12]] # [10, 11, 20, 22]：每个 x 搭配每个 y
flat = [y | part in [[1, 2], [3]], y in part] # [1, 2, 3]：后面的生成器可以使用前面的
firsts = [x | i, x in [5, 6, 7, 8], i < 2] # [5, 6]：`i, x` 同时给出 x 的位置
chars = [c | c in "héllo"] # ["h", "é", "l", "l", "o"]：字符串是字符的序列
```
目前列表推导式会一次性构造出整个数组。惰性列表（元素只在需要时才计算）以后会加入。

数组的内置函数见下一章。

#### 数组模式

有了数组，我们就有了一种新的参数声明方式：
```
# 这个例子展示内置函数 head 是如何工作的
head (x:xs) = x
head [] = void
```
(x:xs) 是一种特殊的参数模式，匹配至少有一个元素的数组：x 代表第一个元素，xs 代表剩下的部分。如果数组只有一个元素，xs 就是 `[]`。你也可以写 (x:y:xs)：x 仍然代表第一个，y 代表第二个，xs 代表剩下的部分。

head [] 匹配空数组，什么也不做。

定义从上到下依次尝试，所以这样也可以：
```
sum (x:xs) = x + sum xs
sum [] = 0
```
如果一个函数用 (x:xs) 拆开数组，却没有为 `[]` 写定义，编译器会警告你：用空数组调用它会让程序停止。

### 条件

流程控制对任何语言都是必需的，所以我们为此准备了一套强大的机制。
```
foo = 5
pred a = a - 1
test _ = check _: # _ 是一个隐藏的参数符号，用于你不想起名字的参数，它只在这个函数里有效
    1 -> echo "success";
    else -> test (pred _)

main = test foo
```
check 关键字的作用类似 kotlin 或 rust 中的 match。上面的代码会循环 5 次才输出 success。
```
check [可枚举类型 | numeric | string]:
    条件1 -> [表达式];
    条件2 -> [表达式];
    else -> [表达式]
```
check 的主体必须缩进：每个分支各占一行，用 `;` 分隔。
如果检查的是数值或字符串，或者上面的分支没有覆盖所有情况，就必须写 **else** 分支。对布尔值的 check 如果同时有 `true ->` 和 `false ->`，就不需要 else：
```
sign n = check n < 0:
    true -> "negative";
    false -> "not negative"
```
check 可以嵌套在分支里，它的分支要缩进得更深。check 的最后一个分支后面没有 `;`，嵌套的 check 也一样这样结束：下一行如果和外层分支对齐，就是外层的下一个分支。
```
both a b = check a:
    true -> check b:
        true -> "both";
        false -> "only a"
    false -> "not a"
```

### if

只有两种情况时，用 `check` 就有点重了。`if` 接受一个布尔值，运行两个块中的一个：
```
abs_diff a b = if a > b:
    a - b
else:
    b - a

main =
    n = 7
    if n % 2 == 1:
        echo "odd"
    size = if n > 5:
        "big"
    else if n > 2:
        "medium"
    else:
        "small"
    echo size # big
```

- 块写在 `if cond:` 下面的行，缩进要比 `if` 所在的行更深。`else:` 单独占一行，和 `if` 所在的行对齐，它的块写在下面。`else if cond:` 可以再加一个条件。
- 条件必须是 Boolean。maybe 值（`Boolean?`）需要先用 `check` 处理 void 的情况。
- 和 `check` 一样，运行的那个块的最后一行就是 `if` 的值。只关心真的情况时可以省略 `else`；这时条件为假会得到 void，所以 `half n = if n % 2 == 0:` 下面写 `n / 2`，得到的是 `Float?`。
- 块是一个独立的作用域，和循环体一样：块里创建的变量只在块里存在。块给外面的变量一个新值，`if` 之后这个变量会保留这个值：`if x > 0:` 下面写 `total = total + x` 就会累加到 `total`。
- `if` 可以嵌套在块里、`check` 的分支里，或者放进括号：`echo (if n > 4:` ... `)`。
- 列表推导式里的 `if` 不能给变量新值：推导式的每个元素都是独立生成的。

### 循环

递归的开销很大，尤其是很深的递归，所以这里有一种简单的重复方式。
```
main =
    loop x in [0..5]: # [0..5] 表示 0, 1, 2, 3, 4
        echo x
    total = 0
    loop x in [1, 2, 3]: # 循环可以遍历数组
        total = total + x
    echo total # 6
    loop i, c in "abc": # 也可以遍历字符串；`i, c` 同时给出位置
        echo i # 0, 1, 2
```
循环体必须缩进。

- 在循环体里创建的变量只存在于循环体内，并且每一轮都是新的。
- 循环体修改的外部变量（上面的 `total`）会把值从这一轮带到下一轮，循环结束后也保留。
- 遍历范围的循环变量始终是整数：你可以给它另一个整数（`x = 40`），但给它字符串是错误的。

`over` 立即结束循环，`jump` 跳过这一轮剩下的部分，继续下一轮。两者都作用于包围它们的最内层循环：
```
main =
    loop x in [1, 2, 3, 4, 5, 6]:
        if x % 2 == 0:
            jump # 跳过偶数
        if x > 4:
            over # 到 5 停下
        echo x # 1, 3
    loop i in [0..infinite]: # 0, 1, 2, ……只要循环还在继续
        if i * i > 50:
            echo i # 8
            over
```
- `over` 和 `jump` 可以用在循环体的任何地方，包括 `check` 或 `if` 里面，但不能用在列表推导式里，因为它不是循环。
- 循环体修改的外部变量，保留它在 `over` 或 `jump` 那一刻的值。
- `[0..infinite]` 永远不会结束，所以只有循环可以遍历它，而且循环需要一个 `over` 或 `ret` 才能结束。它不是数组：`xs = [0..infinite]`，或者在它上面做列表推导式，都是错误。

### 函数定义与调用

```
b = 10
sum a b = a + b
summon a = check a:
    1 -> summon (sum a 9);
    10 -> echo "summon successfully";
    else -> echo "summon failed"

main =
    summon 1 # 会输出 summon successfully
    summon 2 # 会输出 summon failed
```
现在 sum 接受两个参数 **a** 和 **b**，返回 a 加 b。
函数体在另一个定义开始时结束。
可以看到，sum 上面已经有一个 b，但 sum 自己也有一个参数 b，并且在函数体中使用了 b 这个符号。这叫作“名称遮蔽”（全局作用域(参数作用域(函数体作用域))）：查找符号时会从最里层开始，一层一层向外直到全局；如果找遍了都找不到，就会抛出 “SymbolNotFoundError” 错误。
函数可以有多个定义（重载），只要参数个数不同或类型定义不同。

和其他语言一样，**main** 函数是程序的入口，缺少 main 函数是错误。这里的示例代码只是为了演示，并不是真正的项目代码。

有了重载，函数定义还可以这样玩：
```
test a :: Numeric -> Void = echo "winning"
test _ = echo "nothing left"
```
这个函数有两个定义，所以 test 1 会输出 winning，而其他参数会得到 nothing left，这可以看作模式匹配。编译器从上到下为调用者寻找正确的定义，test _ 接住其余所有情况。

#### 调用

函数调用比任何运算符结合得都紧：`sum a 9 + 1` 就是 `(sum a 9) + 1`。

当调用给出的参数比函数接受的多时，多出来的参数会按每个函数接受的参数个数，从左到右分组：
```
echo sum [x | x in [1..101]] # 即 echo (sum [...])：5050
echo head tail [1, 2, 3] # 即 echo (head (tail [1, 2, 3]))：2
```
如果参数可以有不止一种分组方式，编译器会要求你加上括号。

`echo` 只接受一个参数：一个名字、一个值、一个调用（比如 `echo sum xs`），或者括号里的任何东西。所以 `echo (5 > 6)` 的括号是必需的。

#### ret

函数的最后一行就是它的返回值。`ret value` 可以在任何地方（包括循环内部）提前离开函数，单独的 `ret` 以 void 离开：
```
first_negative a =
    loop x in a:
        check x < 0:
            true -> ret x;
            false -> void
    void

main = echo (first_negative [3, -1, 4]) # -1
```
`ret` 不能在列表推导式里使用。

#### 尾调用

每个调用都要等它里面的调用结束，而它们都要占用栈上的空间，栈大约能容纳几万个这样的调用。比这更深的递归会以一个指出函数名的 `StackOverflowError` 结束程序。但是，如果一个调用的值就是函数的结果，之后再没有别的事要做，它就处于*尾部位置*：这时函数会用新的参数重新开始，而不是再深入一层调用，所以它可以像循环一样重复任意多次。
```
count n total = check n:
    0 -> total;
    else -> count (n - 1) (total + n) # 尾部位置：之后没有别的事要做

depth n = check n:
    0 -> 0;
    else -> 1 + depth (n - 1) # 不在尾部位置：还要做 `1 +`

main =
    echo (count 1000000 0) # 500000500000
    echo (depth 1000000) # StackOverflowError: the calls of `depth` went too deep
```
- 尾部位置是函数的最后一行和 `ret` 的值，以及它们里面的：`check` 的各个分支、`if` 块的最后一行，以及 `&&` 和 `||` 的右边。
- 只有对同一个定义的调用才会让它重新开始。在 `sum_list (x:xs) acc = sum_list xs (acc + x)` 中，只要 `xs` 还有元素，这个调用就会让这个定义重新开始，最后则调用 `sum_list [] acc`。在尾部位置调用另一个函数，仍然是普通的调用。
- 函数借用的参数必须是函数自己借用的参数（或它的一部分）、数字或字面量；否则这次调用就是普通的调用，因为参数借用的东西必须活到调用返回为止。

### trail 定义

trail 描述一类值：它包含什么，以及它能做什么。luna 没有其他的类型：从 `5` 到一个水果，每个值都由 trail 来描述。trail 的名字以大写字母开头，比如 `Fruit` 或 `Int`。这只是一种风格，用来把 trail 和其他名字区分开，并不是规则：`trail fruit` 也是正确的。

目前有这些内置 trail：

- Any（基础 trail，其他所有 trail 都隐式地遵循它）
- Iterable（String、Array 或 Vector（以后加入））
- Show（可打印，它包含一个 textualize 函数，echo 会调用这个函数来得到可打印的 String）
- Compare（包含一个用于比较的 compare 函数）
- Copy（`copy` 的通行证：只有 trail 遵循 Copy 的值才能用 `copy x` 复制）
- Array（所有数组，它遵循 Iterable、Show）
- Numeric（所有数字，比如整数和浮点数，它遵循 Show 和 Compare）
- Int（整数）
- Float（浮点数）
- String（字符序列）
- Boolean（true 和 false）
- Void（类似其他语言中的 null）

所有持有值的内置 trail 都遵循 Show 和 Copy，数字和字符串还遵循 Compare；数组在它的元素遵循这些 trail 时也遵循它们。这些 trail 是用 luna 写在标准库（`src/std.luna`）里的：每个 trail 要求什么（`trail Compare:` 要求 `compare @other :: Self -> Int`），以及内置的值遵循哪些 trail（`trail Int follow Numeric, Copy:`）。内置的值本身是机器值，由编译器给出它们的函数，所以整数和浮点数仍然可以混合运算：`1 + 0.5` 是 1.5，因为它们都是数字。

```
sum a b :: Numeric, Numeric -> Numeric = a + b + 5 # 正确
sum "hello" 5 # 错误，"hello" 不是数字
```

任何函数的最后一行都被当作返回值，它的类型就是返回类型。你也可以用 ret 关键字提前返回。
可以用下面的代码定义一个返回 void 的函数：
```
foo _ :: Any -> Void = 5
```
虽然这里写了 5，但因为这个函数的返回类型是 void，所以什么也不会返回。离开这个函数时，5 会被丢弃。

#### 创建你自己的 trail

```
# 内置的 trail Show，就像用 luna 写出来的样子（它是内置的，你不用自己写）
trail Show:
    textualize :: String # 没有默认值：每个遵循 Show 的 trail 都必须给出它

trail Fruit follow Show:
    name :: String # name 没有默认值，所以它是必需的：每个水果都必须给出 name
    peel :: Boolean = false # peel 有默认值，所以可以省略
    textualize = copy name

# 或者你想要一个更具体的 trail
trail Warehouse follow Show:
    rack :: [Fruit]
    textualize = "count:[" + rack.textualize + "]"
    # 如果你不想在这里给出 textualize 的实现，可以省略，但这意味着创建 Warehouse 的地方必须给出它的实现。

buy money :: Int -> Fruit = check true:
    money < 5 -> {name = 1}; # 错误，name 必须是字符串
    money == 5 -> {peel = true}; # 错误，缺少必需的字段
    money > 5 -> {name = "pineapple", peel = true}; # 正确
    else -> {name = "grape", money = 5} # 错误，不能添加多余的字段

main = 
    echo (buy 6).name
    echo buy 6
    t = buy 6
    n = t.name # 错误，除非复制，否则不能把值从 trail 结构中移出来
    n = copy t.name # 正确
    o = Fruit {name = "orange"} # trail 的名字说明了它是什么
```

这定义了一个 trail Fruit，返回类型就是 Fruit。

- **值。** `Fruit {name = "orange"}` 在任何地方都能创建一个 Fruit。单独的 `{...}` 只能用在它的 trail 已知的地方：声明的返回 trail（`-> Fruit`，比如 `buy`）、声明为 `:: Fruit` 的参数，或者声明为 `test :: Fruit = {...}` 的全局符号。没有默认值的字段必须给出；给出 trail 中没有的字段，或者给出 trail 不对的值，都是错误。`Fruit {}` 使用所有默认值。
- **字段。** `t.name` 读取一个字段。和 `head` 一样，它给出的是字段本身，仍然在 `t` 里面：你可以读取它，想保留它就要复制（`copy t.name`）。
- **有默认值的字段和没有参数的函数是同一回事。** `peel :: Boolean = false` 和 `textualize = copy name` 都是一个名字加一个值。这个值可以按名字使用其他字段，这些字段是借用的（想保留一个就要复制：`textualize = copy name`），并在创建值的时候、在它用到的字段之后计算；`Fruit {...}` 也可以给出它自己的（`Fruit {name = "kiwi", textualize = "a kiwi"}`）。
- **follow。** `trail Apple follow Fruit:` 让 Apple 拥有 Fruit 的字段和函数，再加上它自己的，并且在参数或返回值声明为 `:: Fruit` 的地方都可以给出 Apple。（声明为 `Fruit` 的字段或数组目前只能装 Fruit：见“一次一种 trail”。）一个 trail 可以遵循多个 trail：`trail Box follow Show, Compare:`。
- **trail 作为参数。** `print any :: Show -> Void` 接受任何 trail 遵循 Show 的值；这个函数会为它用到的每个 trail 分别编译一次。
- **一次一种 trail。** 一个数组只装同一种 trail 的值，一个 `check` 的所有分支也给出同一种 trail：`[Fruit {name = "a"}, Warehouse {rack = []}]` 目前是错误的。
- **echo。** `echo` 用值的 `textualize` 打印它。trail 没有遵循 Show 的值会打印它的 trail 名字（`Fruit`），并给出警告。
- **Copy。** `copy x` 只接受 trail 遵循 Copy 的值，并逐个字段地复制它，所以每个字段也必须遵循 Copy。遵循 Copy 永远不会改变程序的行为：`y = x` 仍然会移动 `x`。只有内置的 Int、Float、Boolean 和 Void 会自动复制。
- **Compare。** 遵循 Compare 的 trail 要给出 `compare @other :: Self -> Int`，在一个值上调用：`a.compare b` 在 `a` 在前时为 -1，相等时为 0，`b` 在前时为 1。`<`、`<=`、`>`、`>=`、`==` 和 `!=` 都由它得出（`a < b` 表示 `a.compare b < 0`），所以只有遵循 Compare 的 trail 才能比较，或者用作 `check` 的模式，而且只能和同一种 trail 的值比较。运算符只读取它的操作数，所以 `other` 是借用的（`@other`）。`Self` 表示这个 trail 本身（或遵循它的 trail）。
- **Numeric。** 遵循 Numeric 的 trail 表现得像一个数字：它要给出 `add`、`sub`、`mul` 和 `div`（每个都像 `add @other :: Self -> Self`）、`compare` 和 `textualize`，这样对同一种 trail 的两个值，`a + b` 就表示 `a.add b`。如果 trail 还给出了 `mod @other :: Self -> Self`，就可以使用 `%`（`a % b` 表示 `a.mod b`）；Numeric 并不要求它。不过，只有内置的数字会自动复制。目前只有内置 trail 才能遵循 Iterable 和 Array。
- **暂不支持。** 一个 trail 只能在它自己的 `trail ... follow ...:` 这一行里遵循其他 trail；在别处给已有的 trail（比如 String）加上新的 trail，以后才会支持。

trail 的名字在当前作用域中必须是唯一的，不能与任何其他外部或内部的 trail 或函数同名。

```
# 假设还有另一个文件 food.luna，它也包含一个 Fruit trail。
| food#Fruit > # 会导致错误，因为这里已经有一个 Fruit trail 了
| food > # 可以，因为你必须写 Int -> food.Fruit，这不会和我的混淆。
| food#Fruit > OtherFruit # 可以，这样改变了它的名字。

test = { x = "a", y = "b" } # 错误，编译器应该告诉用户这个未知的结构需要一个名字，并停止编译

test :: Fruit = { name = "orange" } # 正确

```
如果用户的 trail 遵循了 Numeric 这样的内置 trail，它也应该表现得像一个数字，但仍然只有内置的数字（int、float）会默认复制。

trail 的字段可以使用它自己的 trail，从而允许递归的 trail；但如果这样做，这个字段的类型会变成 T?，它可以是 void，以避免产生无穷无尽的链。

#### 在 trail 中声明函数

在 trail 内部，它的字段和函数都可以按名字使用（它们属于这个值本身），而 trail 的函数在一个值上调用：`t.print x`，或者 `(Test {}).print x`。没有函数体的函数（比如下面的 `compare`）要由遵循它的 trail 给出：缺少某个函数的 trail 不能创建值，而且 `{...}` 不能给出带参数的函数。字段的默认值不能调用 trail 的函数，因为这时值还没有创建出来。

trail 的函数可以做任何函数能做的事：它可以有多个定义，从上到下依次尝试（`describe n :: Int -> String = ...` 和 `describe n :: String -> String = ...`），可以用模式拆开数组（`total (x:xs) = x + total xs` 和 `total [] = 0`），也可以用在调用链里（`echo b.total reverse xs` 就是 `echo (b.total (reverse xs))`）。遵循另一个 trail 的 trail 可以给出同名函数的定义：它们会先被尝试，然后才是继承来的定义。
```
trail Test:
    print any :: Show -> Void = echo any

# 内置的 trail Compare，就像用 luna 写出来的样子（它是内置的，你不用自己写）
trail Compare:
    compare @other :: Self -> Int # 没有函数体：每个遵循 Compare 的 trail 都必须给出它
```

Ordering 应该是一个枚举，这以后会加入，现在可以先用 Int 代替。

#### void 与 maybe 类型

void 是唯一的“无”值，`echo` 把它打印为 `void`。一个函数如果在某些路径上给出值、在另一些路径上给出 void，它的类型就是 *maybe* 类型：`head [1, 2]` 是 `Int?`，因为 head 对空数组给出 void。编译器会自己推断出 maybe 类型。

- void 会穿过运算符：`head [] + 1` 是 void。
- 在需要真实值的地方（不接受 void 的参数、范围的结束值、声明的返回类型），maybe 值是错误的，直到你用 check 处理了 void 的情况：
```
first_or_zero a =
    h = head a
    check h:
        void -> 0;
        else -> h # 这里 h 是 Int，而不是 Int?
```
`void ->` 只匹配 void。

maybe 类型也可以写出来：在内置 trail 后面加 `?`。`half n :: Int -> Int?` 可能给出 void，`or_zero x :: Int? -> Int` 接受 void，字段 `nickname :: String?` 可以存放 void。你自己定义的 trail 还不能加 `?`（`Fruit?` 以后会支持）。

#### 一个值是否遵循某个 trail？`:?`

`value :? Trail` 在这个值遵循这个 trail 时为 true，就像 Java 里的 `instanceof`。每个值都遵循 Any，所以 `anything :? Any` 永远为 true。
```
trail Fruit follow Show:
    name :: String
    textualize = copy name

trail Apple follow Fruit:
    crisp :: Boolean = true

main =
    echo ("hello" :? String) # true
    echo ("hello" :? Int) # false
    echo (5 :? Numeric) # true
    echo ([1, 2] :? [Int]) # true
    a = Apple {name = "fuji"}
    echo (a :? Fruit) # true：Apple 遵循 Fruit
    echo (a :? Show) # true：Fruit 遵循 Show
    echo (a :? Compare) # false
    h = head [1, 2]
    echo (h :? Int) # true：h 是一个 Int?，里面是 1
    echo (a :? Any) # true：一切都遵循 Any
```

- 右边的 trail 和签名里的写法一样：内置 trail（`Int`、`Numeric`、`Show`、`Iterable` 等）、程序定义的 trail（`Fruit`，或来自链接文件的 `food.Fruit`）、数组（`[Int]`）或者 maybe 类型（`Int?`）。
- 一个值遵循它自己的 trail，以及这个 trail 遵循的所有 trail，正如它的 `follow` 那一行（内置的 trail 则是 `src/std.luna`）所写的。数组在它的元素被声明为 `T` 时才遵循 `[T]`，而不是恰好都是 `T` 的时候：`[1, 2] :? [Numeric]` 为 true，但一个 `Int?` 数组即使里面没有 void，也不是 `:? [Int]`。
- 对大多数值，编译器就知道答案。只有可能属于几种 trail 的值，比如上面的 `Int?`，或者一个给出字符串或数字的 `check`，才在程序运行时检测。
- `:?` 只读取这个值，和 `==` 一样：`s :? String` 之后，`s` 仍然可以使用。
- `:?` 的优先级和 `==` 相同，也不能和它连写：`x + 1 :? Int` 检测的是 `x + 1`，而 `x :? Int == true` 需要加括号。
- 它不会改变变量的类型：在 `check x :? Int:` 的 `true -> ...` 分支里，`x` 仍是原来的类型。

### 多文件以及如何链接它们

有时你无法把所有代码都写在一个文件里，所以这里是大型项目的解决方案。

例如：

```
src - main.luna
    - entity
        - try.luna
        - entity.luna
    - test
        - test.luna
```
在 entity.luna 中：
```
pred a = a - 1
process a = a + 10
next a = a + 1
```
现在你想在 main.luna 中使用 pred：
```
# 查找的根目录是主入口文件所在的位置，在这个例子中，src 就是根路径。
# 因为 entity 和它所在的目录同名，所以这里只需要写 entity
| entity > et # 这一行必须放在文件的最上面，et 是 entity 的别名，可以省略。
# 但对于 try.luna，你必须写出完整路径
| entity/try > try 

echo et.pred 5 # 得到 4
```
只链接一部分：
```
| entity#pred, next > # 只链接 pred 和 next
# 如果你想给它们起别名
| entity#pred, next > p, n # 别名会与前面读到的符号一一对应

echo pred 5 # 得到 4
```

使用 ``lunac`` 时不需要写出所有文件，只需要提供 main.luna（或者其他包含 ``main`` 入口的文件），编译器会自动找到其他文件。

还有几条规则：

- 不写 `>` 时，整个文件以它的文件名来使用：写了 `| entity/try` 之后，就用 `try.pred`。
- 每个文件只能看到它自己的定义、它链接的文件，以及标准库。所以两个文件可以定义同名的符号，而被链接的文件看不到 main.luna 的定义，除非它链接了 main.luna。
- 被链接的文件也可以链接其他文件（它的路径同样从 main.luna 所在的文件夹开始），文件之间也可以互相链接。
- 一个文件中定义的名字不能再按名字链接进同一个文件：用 `>` 给链接进来的那个起另一个名字。

## 第 2 章 内置值与函数

有了数值、字符串和数组，我们还需要一些内置函数来帮助我们做更多事情：
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
len 返回可迭代对象的长度。
head 取出可迭代对象的第一个元素，tail 取出剩下的部分。
init 取出除最后一个之外的所有元素，last 只取最后一个。
reverse 按相反的顺序给出元素。
没有元素时，head、tail、init 和 last 给出 void。
copy 给出 trail 遵循 Copy 的值的一份副本，所有内置的值都遵循它（见第 3 章和“trail 定义”）。

字符串是字符的序列，而不是字节的序列：`len "héllo"` 是 5，`"héllo" !! 1` 是 `"é"`。

如果程序定义了与内置函数同名的函数（比如上面的 `head (x:xs) = x`），就会使用它自己的定义。

## 第 3 章 所有权

luna 没有垃圾回收，你也永远不需要自己释放内存。取而代之的是：每个值都恰好有一个所有者，也就是持有它的变量。当所有者用完时（它所在的函数结束，或者它所在的那一轮循环结束），这个值就会被释放。这和 Rust 的做法一样。

### 移动

把一个值交给别人就会**移动**它，旧的名字就不能再用了：
```
main =
    s = "hello"
    t = s # 字符串移动到 t
    echo t # hello
    echo s # 错误：s 的值已经被移走了
```
以下情况会移动一个值：

- 把它绑定到另一个名字（`t = s`）
- 把它传给函数（`f s`）
- 返回它，或者把它放进数组（`[s]`）
- 把它交给 `++`、`**`、`tail`、`init`、`reverse`，或者遍历它的循环、列表推导式

移动不会复制任何东西：新的所有者得到的就是同一个值。所以 `x = x ++ [4]` 是在旧数组的基础上追加，而不是复制它。给一个名字赋新值（`s = "again"`）会让它重新可用。

数字则不同：`Int`、`Float`、`Boolean` 和 `Void` 的值（以及它们的混合，比如 `Numeric`）会被**复制**，因为复制一个数字的开销并不比移动它大。它们遵循 `Copy` *trail*，trail 是一种标记，说明某个类型能做什么：
```
n = 5
m = n
echo (n + m) # 10，n 仍然可用
```

### 借用

用 `@` 标记的参数只**借用**它的实参：调用者仍然保留它。
```
greet @name = "hello " + name

main =
    s = "luna"
    echo (greet s) # hello luna
    echo s # luna：greet 只是借用了 s
```
在函数内部，被借用的参数仍然属于调用者，所以它是只读的：

- 你可以读取它，也可以把它转借给另一个 `@` 参数：`total @a = sum a` 没有问题，因为 `sum` 也是借用
- 你不能给它赋新值：参数是 `@a` 时，函数体里写 `a = 1` 是错误的
- 你不能把它交出去：`keep @a = a` 和 `wrap @a = [a]` 都是错误的

想保留借来的东西，就要求一份副本：`keep @a = copy a`。`copy` 是 luna 复制字符串或数组的唯一方式，所以你总能看到复制发生在哪里。

同一个函数中参数个数相同的所有定义，必须借用相同的参数，因为调用无法事先知道它会走到哪个定义。

### 循环

遍历数组的循环会拿走这个数组：每个元素依次移动到循环变量中。想保留数组，就用 `@` 借用它：
```
main =
    names = ["ann", "bob"]
    loop n in @names: # 借用 names
        echo n
    echo names # 仍然可用
    loop n in names: # 拿走 names：每个元素移动到 n 中
        echo (n + "!")
```
列表推导式也是一样：`[len w | w in @names]` 借用 `names`，`[w + "!" | w in names]` 拿走它。遍历 `@names` 的循环变量是一个借来的元素：它不能被赋新值，也不能被交出去。

### 元素

`head a`、`last a` 和 `a !! i` 给你的是元素本身，它仍然在 `a` 里面，而不是一份副本。你可以读取它，但想保留它就需要复制：
```
main =
    names = ["ann", "bob"]
    echo (head names) # 没问题：只是读取
    first = head names # 错误：这个元素仍然在 names 里面
    first = copy (head names) # 没问题
```
数字照常复制，所以 `x = head [1, 2]` 没有问题。

### 借用期间

当某个东西被借用时，它的所有者必须保留它。在借用结束之前，你不能移动它，也不能给它赋新值：
```
f @x y = x + y

main =
    s = "x"
    echo (f s s) # 错误：f 借用 s 的同时，第二个 s 会把 s 移走
    names = ["ann"]
    loop n in @names:
        names = [] # 错误：names 正被循环借用
```

`Copy` 是内置 trail 之一（见第 1 章的“trail 定义”）。

## 第 4 章 标准库

这些函数是用 luna 本身写的（`src/std.luna`），随每个程序一起编译；只有程序用到的函数才会进入可执行文件。如果程序定义了同名的函数，就会改用它自己的定义。

| 函数 | 结果 | 它的数组或字符串 |
|---|---|---|
| `abs x` | 与零的距离 | |
| `max a b`、`min a b` | 两个可以比较的值中较大的、较小的 | |
| `sum a`、`product a` | 总和（空数组为 0）、乘积（空数组为 1） | 借用 |
| `average a` | 平均值（浮点数），空数组为 void | 借用 |
| `maximum a`、`minimum a` | 最大、最小的元素，空数组为 void | 拿走 |
| `contains a x`、`count a x` | `a` 中是否有等于 `x` 的元素、有几个 | 借用 |
| `index_of a x` | 第一个等于 `x` 的元素的位置，没有则为 void | 借用 |
| `remove a x` | 不等于 `x` 的元素 | 拿走 |
| `take n a`、`drop n a` | 前 `n` 个元素、其后的元素 | 拿走 |
| `flatten a` | 把 `a` 中的数组连接起来：`flatten [[1], [2, 3]]` 是 `[1, 2, 3]` | 拿走 |
| `is_empty a` | 是否没有元素 | 借用 |
| `join a sep` | 用 `sep` 连接字符串：`join ["a", "b"] ", "` 是 `"a, b"` | 借用 |
| `split s sep` | 按字符 `sep` 分割出的各部分：`split "a,b" ","` 是 `["a", "b"]` | 借用 |
| `repeat s n` | 把 `s` 重复 `n` 次 | 借用 |

“借用”表示调用者保留这个数组（`@a`）；“拿走”表示它被移动进函数，因为结果是由它的元素构成的。

比较值的函数（`max`、`min`、`maximum`、`minimum`、`contains`、`count`、`index_of`、`remove`）适用于每个遵循 Compare 的 trail，包括程序自己定义的：`max (Money {cents = 150}) (Money {cents = 999})`。把数字加起来的函数（`sum`、`product`、`average`、`abs`）从 0 或 1 开始，所以它们用于内置的数字。

### 输入与输出

程序用下面这些函数读取输入、参数和文件，以及写入文件：

| 函数 | 给出什么 |
|---|---|
| `read_line` | 输入（stdin）的下一行，不含行尾；输入结束时给出 void |
| `read_all` | 剩下的全部输入，一次读完 |
| `print s` | 打印字符串 `s`，后面不加换行（`echo` 会加） |
| `args` | 程序启动时得到的参数，是一个字符串数组（不含程序自己的名字） |
| `read_file path` | 文件 `path` 的内容；读不了时给出 void |
| `write_file path text` | 让文件 `path` 的内容变成 `text`（文件不存在就创建，存在就替换原有内容）：给出是否成功 |
| `exit code` | 立刻结束程序，退出码是 `code`（0 表示一切正常） |
| `lines s` | `s` 的各行：`lines "a\nb\n"` 是 `["a", "b"]`（`\r\n` 行尾也可以） |
| `to_int s`、`to_float s` | `s` 里写的数字；不是数字时给出 void：`to_int "-42"` 是 -42，`to_float "2.5"` 是 2.5 |

```
main =
    print "what is your name? "
    name = read_line
    check name:
        void -> echo "goodbye";
        else -> echo ("hello " + name)
```
程序打印的内容会在它等待输入之前显示出来，所以你输入答案之前就能看到问题。`read_line`、`read_file`、`to_int` 和 `to_float` 可能给出 void，所以使用这个值之前要用 `check` 处理（见“void 与 maybe 类型”）。路径从程序运行时所在的文件夹开始。

```
# sum.luna: the total of the numbers in each file it is given, one per line
number s =
    n = to_int s
    check n:
        void -> 0;
        else -> n

main =
    loop path in args:
        text = read_file path
        check text:
            void -> echo ("cannot read " + path);
            else -> echo (sum [number line | line in lines text])
```
`lunac sum.luna -r -- a.txt b.txt` 会编译它，并带着参数 `a.txt` 和 `b.txt` 运行它。

`read_line`、`read_all`、`print`、`args`、`read_file`、`write_file` 和 `exit` 要接触程序之外的东西，所以由运行时提供：`src/std.luna` 在它们前面写上 `native`，只声明签名，没有函数体（`native read_line :: String?`）。只有标准库可以声明 native 函数。`lines`、`to_int` 和 `to_float` 和库里的其他函数一样，是用 luna 写的。
