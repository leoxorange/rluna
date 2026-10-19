# Luna for Visual Studio Code

Syntax highlighting for the Luna language (`.luna` files):

- definitions (`sum a b = ...`), parameters, borrowed parameters (`@a`) and
  patterns (`(x:xs)`, `[]`);
- trails (`trail Fruit follow Show, Compare:`), their fields and functions,
  and trail names (`Int`, `Fruit`, `Self`) in signatures (`:: Int -> Fruit`);
- links to other files (`| entity > et`, `| entity#pred, next > p, n`);
- native definitions (`native read_line :: String?`) and maybe types in
  signatures (`-> Int?`);
- keywords (`check`, `if`, `else`, `loop`, `in`, `over`, `jump`, `ret`), the most used
  library functions (`echo`, `print`, `len`, `head`, `tail`, `init`, `last`, `reverse`, `copy`,
  `slice`) and
  constants (`true`, `false`, `void`, `PI`, `infinite`);
- strings with their escapes, numbers, comments, fields (`t.name`,
  `(buy 6).name`, `{name = "kiwi"}`) and operators (`++`, `**`, `!!`, `..`,
  `->`, `::`, and `:?` with the trail after it).

It also sets up `#` comments (toggled with Ctrl+/ or Cmd+/), bracket
matching and closing, and indents the line after one that ends with `:` or
`=` (a loop body, the arms of a `check`, a function body).

## Installing

From the repository's root, link the extension into VS Code's extensions
folder, then reload VS Code (Command Palette: "Developer: Reload Window"):

```
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/luna
```

On Windows (in PowerShell, from the repository's root):

```
New-Item -ItemType Junction -Path "$env:USERPROFILE\.vscode\extensions\luna" -Target "$PWD\editors\vscode"
```

Or build a package and install that:

```
cd editors/vscode
npx @vscode/vsce package --allow-missing-repository
code --install-extension luna-0.1.0.vsix
```

## Files

- `package.json`: the extension: the `luna` language and its grammar.
- `syntaxes/luna.tmLanguage.json`: the TextMate grammar that colors the
  code. Scopes follow the usual names (`keyword.control`, `entity.name.type`,
  ...), so every color theme works with it.
- `language-configuration.json`: comments, brackets and indentation.
