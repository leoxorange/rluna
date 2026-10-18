# 關於 Luna

[English](readme.md) | [简体中文](readme.zh-CN.md) | 繁體中文

## 第 0 章 關於 luna

luna 是一門類似 haskell 的語言。它的核心理念是盡可能少用關鍵字，讓你寫下的一切看起來都像一條公式。資料流過你為它設計的管線，最後變成你想要的形狀。這就是這門語言的運作方式，也正因如此，luna 有一些和其他語言不同的地方。

### 使用編譯器

`lunac` 把一個 `.luna` 檔案編譯成可執行檔。它不依賴任何其他工具：不需要組譯器、連結器，也不需要 LLVM。

```
lunac hello.luna                       # 產生可執行檔 `hello`
lunac hello.luna -o out -r             # 產生 `out` 並執行它
lunac sum.luna -r -- a.txt b.txt       # 執行它，並傳給它參數 a.txt 和 b.txt
lunac hello.luna --target=windows-x64  # 產生 64 位元 Windows 程式 hello.exe
lunac hello.luna --emit=ast,ir,asm     # 同時輸出語法樹、中介表示（IR）和機器碼
```

目標平台有 `macos-arm64`（Apple 晶片的 Mac）和 `windows-x64`。不寫 `--target` 時，lunac 會為它目前執行的系統編譯。

### 編輯器支援

`editors/vscode` 是一個 Visual Studio Code 擴充功能，可以為 luna 程式碼提供語法高亮。使用時，在這個儲存庫的根目錄把它連結到 VS Code 的擴充功能資料夾，然後重新載入 VS Code（Windows 和其他方式見 `editors/vscode/README.md`）：

```
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/luna
```

## 第 1 章 基本語法

### 計算

```
echo (5 > 6) # false
echo (5 == 5) # true
echo (4 < 10) # true
foo :: Boolean = 5 > 6 # foo 是 false
echo (7 / 2) # 3.5，`/` 的結果一定是浮點數
echo (7 * 2) # 14，整數與整數做 + - * 仍是整數
echo (1 + 0.5) # 1.5，整數與浮點數運算得到浮點數
echo (7 % 3) # 1，餘數：`%` 用於兩個整數
echo (-7 % 3) # 2，餘數的正負號與右邊相同，就像 haskell 的 mod
```

`+` 也可以串接兩個字串：`"con" + "cat"` 是 `"concat"`。字串之間用 `<` 和 `>` 依字母順序比較。

`%` 的優先順序和 `*`、`/` 一樣：`1 + 10 % 4 * 3` 是 7。它的結果介於 0 和右邊之間，所以即使 `i` 是 0，`(i - 1) % 12` 也仍然落在鐘面上。`x % 0` 會以 `ZeroDivisionError` 結束程式。

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
check 可以巢狀放在分支裡，它的分支要縮排得更深。check 的最後一個分支後面沒有 `;`，巢狀的 check 也一樣這樣結束：下一行如果和外層分支對齊，就是外層的下一個分支。
```
both a b = check a:
    true -> check b:
        true -> "both";
        false -> "only a"
    false -> "not a"
```

### if

只有兩種情況時，用 `check` 就有點重了。`if` 接受一個布林值，執行兩個區塊中的一個：
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

- 區塊寫在 `if cond:` 下面的行，縮排要比 `if` 所在的行更深。`else:` 單獨占一行，和 `if` 所在的行對齊，它的區塊寫在下面。`else if cond:` 可以再加一個條件。
- 條件必須是 Boolean。maybe 值（`Boolean?`）需要先用 `check` 處理 void 的情況。
- 和 `check` 一樣，執行的那個區塊的最後一行就是 `if` 的值。只在意真的情況時可以省略 `else`；這時條件為假會得到 void，所以 `half n = if n % 2 == 0:` 下面寫 `n / 2`，得到的是 `Float?`。
- 區塊是一個獨立的作用域，和迴圈主體一樣：區塊裡建立的變數只在區塊裡存在。區塊給外面的變數一個新值，`if` 之後這個變數會保留這個值：`if x > 0:` 下面寫 `total = total + x` 就會累加到 `total`。
- `if` 可以巢狀放在區塊裡、`check` 的分支裡，或是放進括號：`echo (if n > 4:` ... `)`。
- 串列推導式裡的 `if` 不能給變數新值：推導式的每個元素都是獨立產生的。

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
test a :: Numeric -> Void = echo "winning"
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

### trail 定義

trail 描述一類值：它包含什麼，以及它能做什麼。luna 沒有其他的型別：從 `5` 到一個水果，每個值都由 trail 來描述。trail 的名字以大寫字母開頭，例如 `Fruit` 或 `Int`。這只是一種風格，用來把 trail 和其他名字區分開來，並不是規則：`trail fruit` 也是正確的。

目前有這些內建 trail：

- Any（基礎 trail，其他所有 trail 都隱含地遵循它）
- Iterable（String、Array 或 Vector（之後加入））
- Show（可列印，它包含一個 textualize 函式，echo 會呼叫這個函式來取得可列印的 String）
- Compare（包含一個用於比較的 compare 函式）
- Copy（`copy` 的通行證：只有 trail 遵循 Copy 的值才能用 `copy x` 複製）
- Array（所有陣列，它遵循 Iterable、Show）
- Numeric（所有數字，例如整數和浮點數，它遵循 Show 和 Compare）
- Int（整數）
- Float（浮點數）
- String（字元序列）
- Boolean（true 和 false）
- Void（類似其他語言中的 null）

所有持有值的內建 trail 都遵循 Show 和 Copy，數字和字串還遵循 Compare；陣列在它的元素遵循這些 trail 時也遵循它們。這些 trail 是用 luna 寫在標準函式庫（`src/std.luna`）裡的：每個 trail 要求什麼（`trail Compare:` 要求 `compare @other :: Self -> Int`），以及內建的值遵循哪些 trail（`trail Int follow Numeric, Copy:`）。內建的值本身是機器值，由編譯器給出它們的函式，所以整數和浮點數仍然可以混合運算：`1 + 0.5` 是 1.5，因為它們都是數字。

```
sum a b :: Numeric, Numeric -> Numeric = a + b + 5 # 正確
sum "hello" 5 # 錯誤，"hello" 不是數字
```

任何函式的最後一行都會被當成回傳值，它的型別就是回傳型別。你也可以用 ret 關鍵字提早回傳。
可以用下面的程式碼定義一個回傳 void 的函式：
```
foo _ :: Any -> Void = 5
```
雖然這裡寫了 5，但因為這個函式的回傳型別是 void，所以什麼也不會回傳。離開這個函式時，5 會被丟棄。

#### 建立你自己的 trail

```
# 內建的 trail Show，就像用 luna 寫出來的樣子（它是內建的，你不用自己寫）
trail Show:
    textualize :: String # 沒有預設值：每個遵循 Show 的 trail 都必須給出它

trail Fruit follow Show:
    name :: String # name 沒有預設值，所以它是必要的：每個水果都必須給出 name
    peel :: Boolean = false # peel 有預設值，所以可以省略
    textualize = copy name

# 或是你想要一個更具體的 trail
trail Warehouse follow Show:
    rack :: [Fruit]
    textualize = "count:[" + rack.textualize + "]"
    # 如果你不想在這裡給出 textualize 的實作，可以省略，但這表示建立 Warehouse 的地方必須給出它的實作。

buy money :: Int -> Fruit = check true:
    money < 5 -> {name = 1}; # 錯誤，name 必須是字串
    money == 5 -> {peel = true}; # 錯誤，缺少必要的欄位
    money > 5 -> {name = "pineapple", peel = true}; # 正確
    else -> {name = "grape", money = 5} # 錯誤，不能加入多餘的欄位

main = 
    echo (buy 6).name
    echo buy 6
    t = buy 6
    n = t.name # 錯誤，除非複製，否則不能把值從 trail 結構中移出來
    n = copy t.name # 正確
    o = Fruit {name = "orange"} # trail 的名字說明了它是什麼
```

這定義了一個 trail Fruit，回傳型別就是 Fruit。

- **值。** `Fruit {name = "orange"}` 在任何地方都能建立一個 Fruit。單獨的 `{...}` 只能用在它的 trail 已知的地方：宣告的回傳 trail（`-> Fruit`，例如 `buy`）、宣告為 `:: Fruit` 的參數，或是宣告為 `test :: Fruit = {...}` 的全域符號。沒有預設值的欄位必須給出；給出 trail 中沒有的欄位，或是給出 trail 不對的值，都是錯誤。`Fruit {}` 使用所有預設值。
- **欄位。** `t.name` 讀取一個欄位。和 `head` 一樣，它給出的是欄位本身，仍然在 `t` 裡面：你可以讀取它，想保留它就要複製（`copy t.name`）。
- **有預設值的欄位和沒有參數的函式是同一回事。** `peel :: Boolean = false` 和 `textualize = copy name` 都是一個名字加上一個值。這個值可以依名字使用其他欄位，這些欄位是借用的（想保留一個就要複製：`textualize = copy name`），並在建立值的時候、在它用到的欄位之後計算；`Fruit {...}` 也可以給出它自己的（`Fruit {name = "kiwi", textualize = "a kiwi"}`）。
- **follow。** `trail Apple follow Fruit:` 讓 Apple 擁有 Fruit 的欄位和函式，再加上它自己的，而且在參數或回傳值宣告為 `:: Fruit` 的地方都可以給出 Apple。（宣告為 `Fruit` 的欄位或陣列目前只能裝 Fruit：見「一次一種 trail」。）一個 trail 可以遵循多個 trail：`trail Box follow Show, Compare:`。
- **trail 作為參數。** `print any :: Show -> Void` 接受任何 trail 遵循 Show 的值；這個函式會為它用到的每個 trail 各編譯一次。
- **一次一種 trail。** 一個陣列只裝同一種 trail 的值，一個 `check` 的所有分支也給出同一種 trail：`[Fruit {name = "a"}, Warehouse {rack = []}]` 目前是錯誤的。
- **echo。** `echo` 用值的 `textualize` 印出它。trail 沒有遵循 Show 的值會印出它的 trail 名字（`Fruit`），並發出警告。
- **Copy。** `copy x` 只接受 trail 遵循 Copy 的值，並逐一欄位地複製它，所以每個欄位也必須遵循 Copy。遵循 Copy 永遠不會改變程式的行為：`y = x` 仍然會移動 `x`。只有內建的 Int、Float、Boolean 和 Void 會自動複製。
- **Compare。** 遵循 Compare 的 trail 要給出 `compare @other :: Self -> Int`，在一個值上呼叫：`a.compare b` 在 `a` 在前時為 -1，相等時為 0，`b` 在前時為 1。`<`、`<=`、`>`、`>=`、`==` 和 `!=` 都由它得出（`a < b` 表示 `a.compare b < 0`），所以只有遵循 Compare 的 trail 才能比較，或是用作 `check` 的模式，而且只能和同一種 trail 的值比較。運算子只讀取它的運算元，所以 `other` 是借用的（`@other`）。`Self` 表示這個 trail 本身（或遵循它的 trail）。
- **Numeric。** 遵循 Numeric 的 trail 表現得像一個數字：它要給出 `add`、`sub`、`mul` 和 `div`（每個都像 `add @other :: Self -> Self`）、`compare` 和 `textualize`，這樣對同一種 trail 的兩個值，`a + b` 就表示 `a.add b`。如果 trail 還給出了 `mod @other :: Self -> Self`，就可以使用 `%`（`a % b` 表示 `a.mod b`）；Numeric 並不要求它。不過，只有內建的數字會自動複製。目前只有內建 trail 才能遵循 Iterable 和 Array。
- **暫不支援。** 一個 trail 只能在它自己的 `trail ... follow ...:` 這一行裡遵循其他 trail；在別處幫既有的 trail（例如 String）加上新的 trail，之後才會支援。

trail 的名字在目前的作用域中必須是唯一的，不能和任何其他外部或內部的 trail 或函式同名。

```
# 假設還有另一個檔案 food.luna，它也包含一個 Fruit trail。
| food#Fruit > # 會造成錯誤，因為這裡已經有一個 Fruit trail 了
| food > # 可以，因為你必須寫 Int -> food.Fruit，這不會和我的混淆。
| food#Fruit > OtherFruit # 可以，這樣改變了它的名字。

test = { x = "a", y = "b" } # 錯誤，編譯器應該告訴使用者這個未知的結構需要一個名字，並停止編譯

test :: Fruit = { name = "orange" } # 正確

```
如果使用者的 trail 遵循了 Numeric 這類內建 trail，它也應該表現得像一個數字，但仍然只有內建的數字（int、float）會預設複製。

trail 的欄位可以使用它自己的 trail，進而允許遞迴的 trail；但如果這樣做，這個欄位的型別會變成 T?，它可以是 void，以避免產生無窮無盡的鏈。

#### 在 trail 中宣告函式

在 trail 內部，它的欄位和函式都可以依名字使用（它們屬於這個值本身），而 trail 的函式在一個值上呼叫：`t.print x`，或是 `(Test {}).print x`。沒有函式主體的函式（例如下面的 `compare`）要由遵循它的 trail 給出：缺少某個函式的 trail 不能建立值，而且 `{...}` 不能給出帶參數的函式。欄位的預設值不能呼叫 trail 的函式，因為這時值還沒有建立出來。

trail 的函式可以做任何函式能做的事：它可以有多個定義，由上而下依序嘗試（`describe n :: Int -> String = ...` 和 `describe n :: String -> String = ...`），可以用模式拆開陣列（`total (x:xs) = x + total xs` 和 `total [] = 0`），也可以用在呼叫鏈裡（`echo b.total reverse xs` 就是 `echo (b.total (reverse xs))`）。遵循另一個 trail 的 trail 可以給出同名函式的定義：它們會先被嘗試，然後才是繼承來的定義。
```
trail Test:
    print any :: Show -> Void = echo any

# 內建的 trail Compare，就像用 luna 寫出來的樣子（它是內建的，你不用自己寫）
trail Compare:
    compare @other :: Self -> Int # 沒有函式主體：每個遵循 Compare 的 trail 都必須給出它
```

Ordering 應該是一個列舉，之後會加入，現在可以先用 Int 代替。

#### void 與 maybe 型別

void 是唯一的「無」值，`echo` 把它印成 `void`。一個函式如果在某些路徑上給出值、在另一些路徑上給出 void，它的型別就是 *maybe* 型別：`head [1, 2]` 是 `Int?`，因為 head 對空陣列給出 void。編譯器會自己推斷出 maybe 型別。

- void 會穿過運算子：`head [] + 1` 是 void。
- 在需要真實值的地方（不接受 void 的參數、範圍的結束值、宣告的回傳型別），maybe 值是錯誤的，直到你用 check 處理了 void 的情況：
```
first_or_zero a =
    h = head a
    check h:
        void -> 0;
        else -> h # 這裡 h 是 Int，而不是 Int?
```
`void ->` 只比對 void。

maybe 型別也可以寫出來：在內建 trail 後面加 `?`。`half n :: Int -> Int?` 可能給出 void，`or_zero x :: Int? -> Int` 接受 void，欄位 `nickname :: String?` 可以存放 void。你自己定義的 trail 還不能加 `?`（`Fruit?` 以後會支援）。

#### 一個值是否遵循某個 trail？`:?`

`value :? Trail` 在這個值遵循這個 trail 時為 true，就像 Java 裡的 `instanceof`。每個值都遵循 Any，所以 `anything :? Any` 永遠為 true。
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
    echo (h :? Int) # true：h 是一個 Int?，裡面是 1
    echo (a :? Any) # true：一切都遵循 Any
```

- 右邊的 trail 和簽名裡的寫法一樣：內建 trail（`Int`、`Numeric`、`Show`、`Iterable` 等）、程式定義的 trail（`Fruit`，或來自連結檔案的 `food.Fruit`）、陣列（`[Int]`）或者 maybe 型別（`Int?`）。
- 一個值遵循它自己的 trail，以及這個 trail 遵循的所有 trail，正如它的 `follow` 那一行（內建的 trail 則是 `src/std.luna`）所寫的。陣列在它的元素被宣告為 `T` 時才遵循 `[T]`，而不是剛好都是 `T` 的時候：`[1, 2] :? [Numeric]` 為 true，但一個 `Int?` 陣列即使裡面沒有 void，也不是 `:? [Int]`。
- 對大多數的值，編譯器就知道答案。只有可能屬於幾種 trail 的值，例如上面的 `Int?`，或者一個給出字串或數字的 `check`，才在程式執行時檢測。
- `:?` 只讀取這個值，和 `==` 一樣：`s :? String` 之後，`s` 仍然可以使用。
- `:?` 的優先順序和 `==` 相同，也不能和它連寫：`x + 1 :? Int` 檢測的是 `x + 1`，而 `x :? Int == true` 需要加括號。
- 它不會改變變數的型別：在 `check x :? Int:` 的 `true -> ...` 分支裡，`x` 仍是原來的型別。

### 多檔案以及如何連結它們

有時你沒辦法把所有程式碼都寫在一個檔案裡，所以這裡是大型專案的解決方案。

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
現在你想在 main.luna 中使用 pred：
```
# 搜尋的根目錄是主進入點檔案所在的位置，在這個例子中，src 就是根路徑。
# 因為 entity 和它所在的資料夾同名，所以這裡只需要寫 entity
| entity > et # 這一行必須放在檔案的最上面，et 是 entity 的別名，可以省略。
# 但對於 try.luna，你必須寫出完整路徑
| entity/try > try 

echo et.pred 5 # 得到 4
```
只連結一部分：
```
| entity#pred, next > # 只連結 pred 和 next
# 如果你想幫它們取別名
| entity#pred, next > p, n # 別名會和前面讀到的符號一一對應

echo pred 5 # 得到 4
```

使用 ``lunac`` 時不需要寫出所有檔案，只要提供 main.luna（或是其他包含 ``main`` 進入點的檔案），編譯器會自動找到其他檔案。

還有幾條規則：

- 不寫 `>` 時，整個檔案以它的檔名來使用：寫了 `| entity/try` 之後，就用 `try.pred`。
- 每個檔案只看得到它自己的定義、它連結的檔案，以及標準函式庫。所以兩個檔案可以定義同名的符號，而被連結的檔案看不到 main.luna 的定義，除非它連結了 main.luna。
- 被連結的檔案也可以連結其他檔案（它的路徑同樣從 main.luna 所在的資料夾開始），檔案之間也可以互相連結。
- 一個檔案中定義的名字不能再依名字連結進同一個檔案：用 `>` 幫連結進來的那個取另一個名字。

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
copy 給出 trail 遵循 Copy 的值的一份副本，所有內建的值都遵循它（見第 3 章和「trail 定義」）。

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

數字則不同：`Int`、`Float`、`Boolean` 和 `Void` 的值（以及它們的混合，例如 `Numeric`）會被**複製**，因為複製一個數字的成本並不比移動它高。它們遵循 `Copy` *trail*，trail 是一種標記，說明某個型別能做什麼：
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

`Copy` 是內建 trail 之一（見第 1 章的「trail 定義」）。

## 第 4 章 標準函式庫

這些函式是用 luna 本身寫的（`src/std.luna`），會跟每個程式一起編譯；只有程式用到的函式才會進入可執行檔。如果程式定義了同名的函式，就會改用它自己的定義。

| 函式 | 結果 | 它的陣列或字串 |
|---|---|---|
| `abs x` | 與零的距離 | |
| `max a b`、`min a b` | 兩個可以比較的值中較大的、較小的 | |
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

比較值的函式（`max`、`min`、`maximum`、`minimum`、`contains`、`count`、`index_of`、`remove`）適用於每個遵循 Compare 的 trail，包括程式自己定義的：`max (Money {cents = 150}) (Money {cents = 999})`。把數字加起來的函式（`sum`、`product`、`average`、`abs`）從 0 或 1 開始，所以它們用於內建的數字。

### 輸入與輸出

程式用下面這些函式讀取輸入、參數和檔案，以及寫入檔案：

| 函式 | 給出什麼 |
|---|---|
| `read_line` | 輸入（stdin）的下一行，不含行尾；輸入結束時給出 void |
| `read_all` | 剩下的全部輸入，一次讀完 |
| `print s` | 印出字串 `s`，後面不加換行（`echo` 會加） |
| `args` | 程式啟動時得到的參數，是一個字串陣列（不含程式自己的名字） |
| `read_file path` | 檔案 `path` 的內容；讀不了時給出 void |
| `write_file path text` | 讓檔案 `path` 的內容變成 `text`（檔案不存在就建立，存在就取代原有內容）：給出是否成功 |
| `exit code` | 立刻結束程式，結束代碼是 `code`（0 表示一切正常） |
| `lines s` | `s` 的各行：`lines "a\nb\n"` 是 `["a", "b"]`（`\r\n` 行尾也可以） |
| `to_int s`、`to_float s` | `s` 裡寫的數字；不是數字時給出 void：`to_int "-42"` 是 -42，`to_float "2.5"` 是 2.5 |

```
main =
    print "what is your name? "
    name = read_line
    check name:
        void -> echo "goodbye";
        else -> echo ("hello " + name)
```
程式印出的內容會在它等待輸入之前顯示出來，所以你輸入答案之前就能看到問題。`read_line`、`read_file`、`to_int` 和 `to_float` 可能給出 void，所以使用這個值之前要用 `check` 處理（見「void 與 maybe 型別」）。路徑從程式執行時所在的資料夾開始。

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
`lunac sum.luna -r -- a.txt b.txt` 會編譯它，並帶著參數 `a.txt` 和 `b.txt` 執行它。

`read_line`、`read_all`、`print`、`args`、`read_file`、`write_file` 和 `exit` 要接觸程式之外的東西，所以由執行環境提供：`src/std.luna` 在它們前面寫上 `native`，只宣告簽名，沒有函式本體（`native read_line :: String?`）。只有標準函式庫可以宣告 native 函式。`lines`、`to_int` 和 `to_float` 和函式庫裡的其他函式一樣，是用 luna 寫的。
