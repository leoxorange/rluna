# 關於 Luna

[English](readme.md) | [简体中文](readme.zh-CN.md) | 繁體中文

## 第 0 章 關於 luna

luna 是一門類似 haskell 的語言。它的核心理念是盡可能少用關鍵字，讓你寫下的一切看起來都像一條公式。資料流過你為它設計的管線，最後變成你想要的形狀。這就是這門語言的運作方式，也正因如此，luna 有一些和其他語言不同的地方。

### 使用編譯器

`lunac` 把一個 `.luna` 檔案編譯成可執行檔。它不依賴任何其他工具：不需要組譯器、連結器，也不需要 LLVM。

```
lunac hello.luna                       # 產生可執行檔 `hello`
lunac hello.luna -o out -r             # 產生 `out` 並執行它
lunac hello.luna --target=windows-x64  # 產生 64 位元 Windows 程式 hello.exe
lunac hello.luna --emit=ast,ir,asm     # 同時輸出語法樹、中介表示（IR）和機器碼
```

目標平台有 `macos-arm64`（Apple 晶片的 Mac）和 `windows-x64`。不寫 `--target` 時，lunac 會為它目前執行的系統編譯。

## 第 1 章 基本語法

### 計算

```
echo (5 > 6) # false
echo (5 == 5) # true
echo (4 < 10) # true
foo :: boolean = 5 > 6 # foo 是 false
echo (7 / 2) # 3.5，`/` 的結果一定是浮點數
echo (7 * 2) # 14，整數與整數做 + - * 仍是整數
echo (1 + 0.5) # 1.5，整數與浮點數運算得到浮點數
```

`+` 也可以串接兩個字串：`"con" + "cat"` 是 `"concat"`。字串之間用 `<` 和 `>` 依字母順序比較。

### 符號定義

```
sum = 1
foo = 10 + 8 * 7 # foo 是 66，空格不是必要的，只是為了好看
echo foo # foo 是 66
foo = (5 + 1)*6 # 現在 foo 變成 36，這樣就能輕鬆地重新定義 foo
echo foo # foo 是 36
```
這不只是變數定義。這門語言像 haskell 一樣運作：這裡的 sum 也可以看成一個函式，它不接受參數，但總是回傳 1。每一步計算都遵循四則運算；alpha 版本中還沒有更進階的運算。

定義從第 1 欄開始。重新定義一個符號會取代先前的定義（編譯器會發出警告）。值是常數的符號（例如上面的 `foo`）在編譯時就會算好，使用它沒有任何成本。會做事情的符號（例如 `banner = echo "welcome"`）則在每次使用時都會執行。

以 **#** 開頭表示單行註解。

我們還有一些方便使用的內建符號：
```
PI # 3.141592654...
infinite # 所有數字都比它小
-infinite # 所有數字都比它大
```
它們不能被重新定義。

### 陣列定義

這部分我想要 haskell 的風格。
和其他語言一樣，陣列位置從 0 開始，而且所有陣列都是不可變的。一個陣列的所有元素型別相同。
```
a = [1, 2, 3] # 這是一個陣列
b = a ++ [4, 5] # 這會建立一個新陣列 [1,2,3,4,5]，而不是修改舊陣列
c = [1, 2, 3] ** [4, 5] # [4, 10, 3]：逐一元素相乘，較長陣列多出的元素會保留下來
d = [0..5] # 這會建立陣列 [0, 1, 2, 3, 4]
```

`++` 和 `**` 會拿走交給它們的陣列：元素被移動到新陣列裡，所以沒有任何複製，執行 `b = a ++ [4, 5]` 之後 `a` 就不能再用了。第 3 章會說明這一點，以及如何保留 `a`。

#### 串列推導式

串列推導式依規則建構一個陣列：
```
a = [x | x in [0..5]] # [0, 1, 2, 3, 4]
a = [x * 2 | x in [0..5]] # [0, 2, 4, 6, 8]
a = [x * x| x in [0..5]] # [0, 1, 4, 9, 16]

# a 是 [0, 1, 4, 9, 16]
echo (a !! 4) # 取出陣列 a 中位置 4 的元素，也就是 16
echo (a !! 9) # void：沒有位置 9
```

`|` 後面是產生器（`x in ...`）和條件：
```
big = [x | x in [0..10], x > 5] # [6, 7, 8, 9]：只保留條件為真的元素
pairs = [x * y | x in [1..3], y in [10..12]] # [10, 11, 20, 22]：每個 x 搭配每個 y
flat = [y | part in [[1, 2], [3]], y in part] # [1, 2, 3]：後面的產生器可以使用前面的
firsts = [x | i, x in [5, 6, 7, 8], i < 2] # [5, 6]：`i, x` 同時給出 x 的位置
chars = [c | c in "héllo"] # ["h", "é", "l", "l", "o"]：字串是字元的序列
```
目前串列推導式會一次建構出整個陣列。惰性串列（元素只在需要時才計算）之後會加入。

陣列的內建函式請見下一章。

#### 陣列模式

有了陣列，我們就有了一種新的參數宣告方式：
```
# 這個例子展示內建函式 head 是如何運作的
head (x:xs) = x
head [] = void
```
(x:xs) 是一種特殊的參數模式，比對至少有一個元素的陣列：x 代表第一個元素，xs 代表剩下的部分。如果陣列只有一個元素，xs 就是 `[]`。你也可以寫 (x:y:xs)：x 仍然代表第一個，y 代表第二個，xs 代表剩下的部分。

head [] 比對空陣列，什麼也不做。

定義會由上而下依序嘗試，所以這樣也可以：
```
sum (x:xs) = x + sum xs
sum [] = 0
```
如果一個函式用 (x:xs) 拆開陣列，卻沒有為 `[]` 寫定義，編譯器會警告你：用空陣列呼叫它會讓程式停止。

### 條件

流程控制對任何語言都是必要的，所以我們為此準備了一套強大的機制。
```
foo = 5
pred a = a - 1
test _ = check _: # _ 是一個隱藏的參數符號，用於你不想取名字的參數，它只在這個函式裡有效
    1 -> echo "success";
    else -> test (pred _)

main = test foo
```
check 關鍵字的作用類似 kotlin 或 rust 中的 match。上面的程式碼會循環 5 次才輸出 success。
```
check [可列舉型別 | numeric | string]:
    條件1 -> [運算式];
    條件2 -> [運算式];
    else -> [運算式]
```
check 的主體必須縮排：每個分支各占一行，用 `;` 分隔。
如果檢查的是數值或字串，或是上面的分支沒有涵蓋所有情況，就必須寫 **else** 分支。對布林值的 check 如果同時有 `true ->` 和 `false ->`，就不需要 else：
```
sign n = check n < 0:
    true -> "negative";
    false -> "not negative"
```
check 可以巢狀放在分支裡，它的分支要縮排得更深。

### 迴圈

遞迴的成本很高，尤其是很深的遞迴，所以這裡有一種簡單的重複方式。
```
main =
    loop x in [0..5]: # [0..5] 表示 0, 1, 2, 3, 4
        echo x
    total = 0
    loop x in [1, 2, 3]: # 迴圈可以走訪陣列
        total = total + x
    echo total # 6
    loop i, c in "abc": # 也可以走訪字串；`i, c` 同時給出位置
        echo i # 0, 1, 2
```
迴圈主體必須縮排。

- 在迴圈主體裡建立的變數只存在於主體內，而且每一輪都是新的。
- 迴圈主體修改的外部變數（上面的 `total`）會把值從這一輪帶到下一輪，迴圈結束後也會保留。
- 走訪範圍的迴圈變數一定是整數：你可以給它另一個整數（`x = 40`），但給它字串是錯誤的。

### 函式定義與呼叫

```
b = 10
sum a b = a + b
summon a = check a:
    1 -> summon (sum a 9);
    10 -> echo "summon successfully";
    else -> echo "summon failed"

main =
    summon 1 # 會輸出 summon successfully
    summon 2 # 會輸出 summon failed
```
現在 sum 接受兩個參數 **a** 和 **b**，回傳 a 加 b。
函式主體在另一個定義開始時結束。
可以看到，sum 上面已經有一個 b，但 sum 自己也有一個參數 b，並且在函式主體中使用了 b 這個符號。這叫作「名稱遮蔽」（全域作用域(參數作用域(函式主體作用域))）：尋找符號時會從最內層開始，一層一層往外直到全域；如果找遍了都找不到，就會拋出 「SymbolNotFoundError」 錯誤。
函式可以有多個定義（多載），只要參數個數不同或型別定義不同。

和其他語言一樣，**main** 函式是程式的進入點，缺少 main 函式是錯誤。這裡的範例程式碼只是為了示範，並不是真正的專案程式碼。

有了多載，函式定義還可以這樣玩：
```
test a :: numeric -> void = echo "winning"
test _ = echo "nothing left"
```
這個函式有兩個定義，所以 test 1 會輸出 winning，而其他參數會得到 nothing left，這可以看成模式比對。編譯器由上而下為呼叫者尋找正確的定義，test _ 接住其餘所有情況。

#### 呼叫

函式呼叫比任何運算子結合得都緊：`sum a 9 + 1` 就是 `(sum a 9) + 1`。

當呼叫給出的引數比函式接受的多時，多出來的引數會依每個函式接受的參數個數，由左到右分組：
```
echo sum [x | x in [1..101]] # 即 echo (sum [...])：5050
echo head tail [1, 2, 3] # 即 echo (head (tail [1, 2, 3]))：2
```
如果引數可以有不只一種分組方式，編譯器會要求你加上括號。

`echo` 只接受一個引數：一個名字、一個值、一個呼叫（例如 `echo sum xs`），或是括號裡的任何東西。所以 `echo (5 > 6)` 的括號是必要的。

#### ret

函式的最後一行就是它的回傳值。`ret value` 可以在任何地方（包括迴圈內部）提早離開函式，單獨的 `ret` 以 void 離開：
```
first_negative a =
    loop x in a:
        check x < 0:
            true -> ret x;
            false -> void
    void

main = echo (first_negative [3, -1, 4]) # -1
```
`ret` 不能在串列推導式裡使用。

### 型別定義

目前有這些內建型別：

- any（基礎型別，其他所有型別都擴充自它）
- iterable（字串、陣列或向量（之後加入））
- array（所有陣列型別）
- numeric（所有數字，例如整數和浮點數）
- int（整數）
- float（浮點數）
- string（字元序列）
- boolean（true 和 false）
- void（類似其他語言中的 null）

```
sum a b :: numeric, numeric -> numeric = a + b + 5 # 正確
sum "hello" 5 # 錯誤，"hello" 不是數字
```

任何函式的最後一行都會被當成回傳值，它的型別就是回傳型別。你也可以用 ret 關鍵字提早回傳。
可以用下面的程式碼定義一個回傳 void 的函式：
```
foo _ :: any -> void = 5
```
雖然這裡寫了 5，但因為這個函式的回傳型別是 void，所以什麼也不會回傳。離開這個函式時，5 會被丟棄。

#### void 與 maybe 型別

void 是唯一的「無」值，`echo` 把它印成 `void`。一個函式如果在某些路徑上給出值、在另一些路徑上給出 void，它的型別就是 *maybe* 型別：`head [1, 2]` 是 `int?`，因為 head 對空陣列給出 void。編譯器會自己推斷出 maybe 型別。

- void 會穿過運算子：`head [] + 1` 是 void。
- 在需要真實值的地方（不接受 void 的參數、範圍的結束值、宣告的回傳型別），maybe 值是錯誤的，直到你用 check 處理了 void 的情況：
```
first_or_zero a =
    h = head a
    check h:
        void -> 0;
        else -> h # 這裡 h 是 int，而不是 int?
```
`void ->` 只比對 void。目前還不能在型別定義中寫出 maybe 型別。

## 第 2 章 內建值與函式

有了數值、字串和陣列，我們還需要一些內建函式來幫我們做更多事情：
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
len 回傳可迭代物件的長度。
head 取出可迭代物件的第一個元素，tail 取出剩下的部分。
init 取出除了最後一個之外的所有元素，last 只取最後一個。
reverse 依相反的順序給出元素。
沒有元素時，head、tail、init 和 last 給出 void。
copy 給出任何值的一份副本（見第 3 章）。

字串是字元的序列，而不是位元組的序列：`len "héllo"` 是 5，`"héllo" !! 1` 是 `"é"`。

如果程式定義了和內建函式同名的函式（例如上面的 `head (x:xs) = x`），就會使用它自己的定義。

## 第 3 章 所有權

luna 沒有垃圾回收，你也永遠不需要自己釋放記憶體。取而代之的是：每個值都恰好有一個擁有者，也就是持有它的變數。當擁有者用完時（它所在的函式結束，或是它所在的那一輪迴圈結束），這個值就會被釋放。這和 Rust 的做法一樣。

### 移動

把一個值交給別人就會**移動**它，舊的名字就不能再用了：
```
main =
    s = "hello"
    t = s # 字串移動到 t
    echo t # hello
    echo s # 錯誤：s 的值已經被移走了
```
以下情況會移動一個值：

- 把它綁定到另一個名字（`t = s`）
- 把它傳給函式（`f s`）
- 回傳它，或是把它放進陣列（`[s]`）
- 把它交給 `++`、`**`、`tail`、`init`、`reverse`，或是走訪它的迴圈、串列推導式

移動不會複製任何東西：新的擁有者拿到的就是同一個值。所以 `x = x ++ [4]` 是在舊陣列的基礎上附加，而不是複製它。給一個名字新的值（`s = "again"`）會讓它重新可用。

數字則不同：`int`、`float`、`boolean` 和 `void` 的值（以及它們的混合，例如 `numeric`）會被**複製**，因為複製一個數字的成本並不比移動它高。它們遵循 `Copy` *trail*，trail 是一種標記，說明某個型別能做什麼：
```
n = 5
m = n
echo (n + m) # 10，n 仍然可用
```

### 借用

用 `@` 標記的參數只**借用**它的引數：呼叫者仍然保有它。
```
greet @name = "hello " + name

main =
    s = "luna"
    echo (greet s) # hello luna
    echo s # luna：greet 只是借用了 s
```
在函式內部，被借用的參數仍然屬於呼叫者，所以它是唯讀的：

- 你可以讀取它，也可以把它轉借給另一個 `@` 參數：`total @a = sum a` 沒有問題，因為 `sum` 也是借用
- 你不能給它新的值：參數是 `@a` 時，函式主體裡寫 `a = 1` 是錯誤的
- 你不能把它交出去：`keep @a = a` 和 `wrap @a = [a]` 都是錯誤的

想保留借來的東西，就要求一份副本：`keep @a = copy a`。`copy` 是 luna 複製字串或陣列的唯一方式，所以你總是看得到複製發生在哪裡。

同一個函式中參數個數相同的所有定義，必須借用相同的參數，因為呼叫無法事先知道它會走到哪個定義。

### 迴圈

走訪陣列的迴圈會拿走這個陣列：每個元素依序移動到迴圈變數中。想保留陣列，就用 `@` 借用它：
```
main =
    names = ["ann", "bob"]
    loop n in @names: # 借用 names
        echo n
    echo names # 仍然可用
    loop n in names: # 拿走 names：每個元素移動到 n 中
        echo (n + "!")
```
串列推導式也一樣：`[len w | w in @names]` 借用 `names`，`[w + "!" | w in names]` 拿走它。走訪 `@names` 的迴圈變數是一個借來的元素：它不能被給予新的值，也不能被交出去。

### 元素

`head a`、`last a` 和 `a !! i` 給你的是元素本身，它仍然在 `a` 裡面，而不是一份副本。你可以讀取它，但想保留它就需要複製：
```
main =
    names = ["ann", "bob"]
    echo (head names) # 沒問題：只是讀取
    first = head names # 錯誤：這個元素仍然在 names 裡面
    first = copy (head names) # 沒問題
```
數字照常複製，所以 `x = head [1, 2]` 沒有問題。

### 借用期間

當某個東西被借用時，它的擁有者必須保有它。在借用結束之前，你不能移動它，也不能給它新的值：
```
f @x y = x + y

main =
    s = "x"
    echo (f s s) # 錯誤：f 借用 s 的同時，第二個 s 會把 s 移走
    names = ["ann"]
    loop n in @names:
        names = [] # 錯誤：names 正被迴圈借用
```

trail 和使用者自訂型別（你自己建立的型別）之後會加入；目前 `Copy` 是唯一的 trail，而且是內建的。

## 第 4 章 標準函式庫

這些函式是用 luna 本身寫的（`src/std.luna`），會跟每個程式一起編譯；只有程式用到的函式才會進入可執行檔。如果程式定義了同名的函式，就會改用它自己的定義。

| 函式 | 結果 | 它的陣列或字串 |
|---|---|---|
| `abs x`、`max a b`、`min a b` | 與零的距離、較大的數、較小的數 | |
| `sum a`、`product a` | 總和（空陣列為 0）、乘積（空陣列為 1） | 借用 |
| `average a` | 平均值（浮點數），空陣列為 void | 借用 |
| `maximum a`、`minimum a` | 最大、最小的元素，空陣列為 void | 拿走 |
| `contains a x`、`count a x` | `a` 中是否有等於 `x` 的元素、有幾個 | 借用 |
| `index_of a x` | 第一個等於 `x` 的元素的位置，沒有則為 void | 借用 |
| `remove a x` | 不等於 `x` 的元素 | 拿走 |
| `take n a`、`drop n a` | 前 `n` 個元素、其後的元素 | 拿走 |
| `flatten a` | 把 `a` 中的陣列串接起來：`flatten [[1], [2, 3]]` 是 `[1, 2, 3]` | 拿走 |
| `is_empty a` | 是否沒有元素 | 借用 |
| `join a sep` | 用 `sep` 串接字串：`join ["a", "b"] ", "` 是 `"a, b"` | 借用 |
| `split s sep` | 依字元 `sep` 分割出的各部分：`split "a,b" ","` 是 `["a", "b"]` | 借用 |
| `repeat s n` | 把 `s` 重複 `n` 次 | 借用 |

「借用」表示呼叫者保有這個陣列（`@a`）；「拿走」表示它被移動進函式，因為結果是由它的元素構成的。
