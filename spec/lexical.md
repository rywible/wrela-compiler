# wrela lexical structure

Normative (D-104). `spec/grammar.ebnf` is written over the tokens defined here, and the compiler's
lexer (`compiler/syntax/src/lexer.rs`) implements exactly these rules; the oracle parser in
`compiler/grammar` uses that same lexer. Rules are numbered (L1, L2, …) so tests and diagnostics
can cite them. A number is never reused; new rules are appended.

Error codes come from the lexical range E0001–E0099.

## Source text

- **L1.** A source file is UTF-8 text with the extension `.wrela`. A file that isn't valid UTF-8 is
  rejected before lexing.
- **L2.** A byte-order mark (U+FEFF) at the very start of a file is ignored; byte offsets still
  count it.
- **L3.** Outside comments and string literals, a file holds only ASCII letters, digits, `_`, the
  punctuation of L15, space, tab and line breaks. Any other character is E0001, reported once per
  run of such characters and otherwise skipped. A name with non-ASCII letters or digits in it
  stays one token (with one E0001), so later phases see the name the author meant.
- **L4.** A line break is `\n` or `\r\n`. A `\r` without a `\n` after it is E0001 (its fix replaces
  it with `\n`) and is read as a line break.
- **L5.** Space and tab separate tokens and are otherwise ignored.

## Comments

- **L6.** `//` starts a comment that runs to the end of the line, not including the line break.
- **L7.** A comment that starts with exactly three slashes (`///`, not `////`) is a doc comment.
  It documents the item, field or variant that follows it.
- **L8.** Comments are trivia: the grammar never sees them, and they are never the previous or
  next token in L17. The formatter keeps them.
- **L9.** `/*` is E0002: wrela has no block comments. The lexer skips to the matching `*/`
  (nesting, as in Rust) or to the end of the file, and continues. When the comment is on one line,
  its fix rewrites it as a `//` comment.

## Identifiers and keywords

- **L10.** An identifier is `[A-Za-z_][A-Za-z0-9_]*`, taking the longest run. `_` alone is not an
  identifier: it's the wildcard token `_`.
- **L11.** These identifiers are keywords, reserved everywhere:

  ```
  as borrow break const continue dyn else enum false fn for if impl in let loop match mut package
  pub return self Self struct take trait true type unsafe use var while yield
  ```

  This is the final list: each word has a use. Type names (`f32`, `u32`, `vec3`, `bool`, …) and
  attribute names (`compute` in `@compute`) are ordinary identifiers. `package` is only in
  `pub(package)` (language.md §3). wrela has no `dyn`
  (language.md §18): the word stays reserved so the compiler can say what to use instead. `where`
  isn't a keyword: bounds go on the parameters.

## Number literals

- **L12.** A number token starts with a digit and takes the longest run of letters, digits and
  `_`. A decimal number (one that doesn't start with `0x`, `0b` or `0o`) also takes one `.` that
  is directly followed by a digit, and a `+` or `-` directly after an `e` or `E` when a digit
  directly follows the sign. Two exceptions keep a `.` out: a `.` followed by another `.` (`1..5`
  is `1`, `..`, `5`), and a number directly after a `.` token takes no `.` and no exponent sign
  (`t.0.1` is `t`, `.`, `0`, `.`, `1`).
- **L13.** The token's text is then classified. `_` may appear among a form's digits and is
  ignored, but each run of digits needs at least one real digit.

  | Form | Syntax, ignoring `_` | Token |
  |---|---|---|
  | decimal integer | `[0-9]+` | INT |
  | hexadecimal integer | `0x[0-9A-Fa-f]+` | INT |
  | binary integer | `0b[01]+` | INT |
  | octal integer | `0o[0-7]+` | INT |
  | float | `[0-9]+ "." [0-9]+ exponent?`, or `[0-9]+ exponent` | FLOAT |
  | exponent | `[eE] [+-]? [0-9]+` | |

  A number whose text is exactly one of these forms is that INT or FLOAT. Otherwise, after the
  longest form at its start, the rest of the text is a suffix:

  - **A type suffix** (`i8 u8 i16 u16 i32 u32 i64 u64 f32 f64`, as in `1.0f32`) is E0003. A
    literal takes its type from context; convert with `f32(x)` where it can't.
  - **A suffix starting with `e` or `E`** is a malformed exponent, E0004. No unit is named `e`
    (D-025), so `1e5` is always a float.
  - **Any other suffix that is an identifier** makes the token SUFFIXED: a unit suffix, as in
    `15cm`, which means `15 * cm` (language.md §5).
  - **Anything else** (`0x`, `0b2`, `0X1F`, `1.5e`) is E0004.

  A float has digits on both sides of its `.`: `1.` is `1` then `.`, and `.5` is `.` then `5`.
  There are no negative literals: `-1` is unary minus applied to `1`.
  A literal must fit the type it takes (E0006): an integer is in its type's range, a float is
  finite (it may round), and an integer that becomes a float is exact (`16777217` isn't an
  `f32`; `16777217.0` rounds).

## String literals

- **L14.** A string literal is `"` … `"` on one line, with the escapes `\\ \" \n \r \t \0`. It
  lexes as a STRING token. An unterminated string, or an unknown escape, is E0005. There are no
  character literals: `'` is E0001.

## Operators and punctuation

- **L15.** Operators and punctuation, matched longest first:

  ```
  **=  <<=  >>=  ..=
  ::  ->  =>  ==  !=  <=  >=  &&  ||  +=  -=  *=  /=  %=  ^=  &=  |=  <<  >>  **  ..
  (  )  [  ]  {  }  ,  ;  :  .  @  =  +  -  *  /  %  ^  &  |  !  <  >  ?  _
  ```

  `**` is exponentiation and `^` is XOR (D-076). `?` propagates an error (language.md §15).
  `#`, `$`, `~` and `` ` `` are E0001.
- **L16.** Where the grammar expects a `>` that closes a generic argument list (`GT_CLOSE` in
  the grammar), a `>>`, `>=` or `>>=` token is split: its first `>` closes the list, and the rest
  of its text is the next token (`Option<Option<T>>`, `let x: Option<T>= y`).

## Newlines

- **L17.** A line break produces a NEWLINE token only when all three hold:
  1. the innermost open bracket (L19), if there is one, is `{`, not `(` or `[`;
  2. the previous token can end a statement: an identifier, INT, FLOAT, SUFFIXED, STRING,
     `true`, `false`, `self`, `Self`, `return`, `break`, `continue`, `yield`, `)`, `]`, `}`, `>`,
     `>>` or `?`;
  3. the next token is not `.` (the single dot; `..` and `..=` don't count).

  "Previous" and "next" skip comments and line breaks (L8). Otherwise the line break is
  whitespace.
- **L18.** Line breaks with no token between them (blank lines, comment-only lines) produce at
  most one NEWLINE. There is no NEWLINE before the first token. The token stream ends with EOF,
  and a line break before EOF follows L17 (EOF is not `.`).
- **L19.** `(`, `[` and `{` open brackets. `)`, `]` and `}` close the innermost open bracket of
  their kind, and with it any brackets opened after it. A closing bracket with no open bracket of
  its kind changes nothing (the parser reports it).
- **L20.** Consequences of L17, for readers (not extra rules):
  - Only a leading `.` continues a line that could have ended. A line that starts with `(`, `[`,
    `-` or `|` begins a new statement: there's no JavaScript-style continuation (D-038, D-079).
  - A binary operator that continues an expression trails its line: `a +`, then `b` on the next
    line. `>` and `>>` can't, because they may close a generic argument list: break a comparison
    inside parentheses instead.
  - `else` goes on the line of the `}` before it, and a block's `{` on the line of its head.
  - `;` separates statements on one line; a line break after `;` is whitespace.

## Tokens the grammar sees

- **L21.** `spec/grammar.ebnf` uses the token classes IDENT (L10), INT and FLOAT (L13), SUFFIXED
  (L13), STRING (L14), FSTRING, FSTRING_HEAD, FSTRING_MID and FSTRING_TAIL (L22), NEWLINE (L17,
  L18), EOF (L18) and GT_CLOSE (L16). Keywords (L11) and punctuation, `_` included (L15), appear in
  the grammar as quoted text, such as `"fn"` and `"+="`.

## F-strings

- **L22.** `f"` (an `f` directly before `"`) starts an f-string: text, with holes in `{ }` that hold
  expressions, on one line. The text has L14's escapes, and `{{` and `}}` for literal braces; a
  lone `}` in it is E0005. The lexer splits an f-string at its holes:
  - `f"` … `"` without a hole is one FSTRING token.
  - `f"` … `{` is an FSTRING_HEAD, which opens a hole. The hole's contents are ordinary tokens.
  - Inside a hole, outside any bracket opened in it, `}` ends the hole, and so does `:` (not
    `::`), which starts a **format spec** running to the next `}`. From there the text goes on:
    up to the next hole's `{` it's an FSTRING_MID (`}` … `{` or `:spec}` … `{`), and up to the
    closing `"` an FSTRING_TAIL (`}` … `"` or `:spec}` … `"`).

  Brackets inside a hole nest as anywhere else (L19), and an f-string can hold another. A line
  break or the file's end inside an f-string or a hole is E0005. A hole's expression is checked
  by the grammar (`fstring`); its format spec by the checker (language.md §4).

## `let … else`

- **L23.** Not a lexical rule, but one the lexer's layout rules make necessary: in
  `let PATTERN = VALUE else { … }`, the value can't end with `}`, or be an operator's result, since
  the `else` would then belong to it. It's a literal, a path, or a parenthesized or bracketed
  expression, with any postfixes (calls, fields, indexes, `?`) and prefix operators
  (`let_else_init` in the grammar). The parser's error (E0100) offers to wrap it in parentheses.
