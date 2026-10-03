//! Tokens (spec/lexical.md).

use wrela_diag::Span;

macro_rules! token_kinds {
    (
        classes { $($cname:ident = $cgrammar:literal,)* }
        keywords { $($kname:ident = $ktext:literal,)* }
        punct { $($pname:ident = $ptext:literal,)* }
    ) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum TokenKind {
            $($cname,)*
            $($kname,)*
            $($pname,)*
        }

        impl TokenKind {
            /// How spec/grammar.ebnf names this token: a class name such as `IDENT`, or the
            /// quoted text of a keyword or punctuation token, without quotes.
            pub fn grammar_name(self) -> &'static str {
                match self {
                    $(TokenKind::$cname => $cgrammar,)*
                    $(TokenKind::$kname => $ktext,)*
                    $(TokenKind::$pname => $ptext,)*
                }
            }

            /// The fixed text of a keyword or punctuation token.
            pub fn fixed_text(self) -> Option<&'static str> {
                match self {
                    $(TokenKind::$cname => None,)*
                    $(TokenKind::$kname => Some($ktext),)*
                    $(TokenKind::$pname => Some($ptext),)*
                }
            }

            pub fn is_keyword(self) -> bool {
                matches!(self, $(TokenKind::$kname)|*)
            }

            pub fn keyword(text: &str) -> Option<TokenKind> {
                match text {
                    $($ktext => Some(TokenKind::$kname),)*
                    _ => None,
                }
            }

            /// Every token class (the tokens whose text varies, and NEWLINE and EOF), in spec
            /// order.
            pub const CLASSES: &'static [TokenKind] = &[$(TokenKind::$cname),*];
            /// Every keyword, in spec order.
            pub const KEYWORDS: &'static [TokenKind] = &[$(TokenKind::$kname),*];
            /// Every punctuation token, longest first (the order L15 matches in).
            pub const PUNCT: &'static [TokenKind] = &[$(TokenKind::$pname),*];
        }
    };
}

token_kinds! {
    classes {
        Ident = "IDENT",
        Int = "INT",
        Float = "FLOAT",
        Suffixed = "SUFFIXED",
        Str = "STRING",
        Newline = "NEWLINE",
        Eof = "EOF",
    }
    keywords {
        As = "as",
        Borrow = "borrow",
        Break = "break",
        Const = "const",
        Continue = "continue",
        Dyn = "dyn",
        Else = "else",
        Enum = "enum",
        False = "false",
        Fn = "fn",
        For = "for",
        If = "if",
        Impl = "impl",
        In = "in",
        Let = "let",
        Loop = "loop",
        Match = "match",
        Mut = "mut",
        Pub = "pub",
        Return = "return",
        SelfValue = "self",
        SelfType = "Self",
        Struct = "struct",
        Take = "take",
        Trait = "trait",
        True = "true",
        Type = "type",
        Unsafe = "unsafe",
        Use = "use",
        Var = "var",
        Where = "where",
        While = "while",
    }
    punct {
        StarStarEq = "**=",
        ShlEq = "<<=",
        ShrEq = ">>=",
        DotDotEq = "..=",
        ColonColon = "::",
        Arrow = "->",
        FatArrow = "=>",
        EqEq = "==",
        Ne = "!=",
        Le = "<=",
        Ge = ">=",
        AndAnd = "&&",
        OrOr = "||",
        PlusEq = "+=",
        MinusEq = "-=",
        StarEq = "*=",
        SlashEq = "/=",
        PercentEq = "%=",
        CaretEq = "^=",
        AmpEq = "&=",
        PipeEq = "|=",
        Shl = "<<",
        Shr = ">>",
        StarStar = "**",
        DotDot = "..",
        LParen = "(",
        RParen = ")",
        LBracket = "[",
        RBracket = "]",
        LBrace = "{",
        RBrace = "}",
        Comma = ",",
        Semi = ";",
        Colon = ":",
        Dot = ".",
        At = "@",
        Eq = "=",
        Plus = "+",
        Minus = "-",
        Star = "*",
        Slash = "/",
        Percent = "%",
        Caret = "^",
        Amp = "&",
        Pipe = "|",
        Bang = "!",
        Lt = "<",
        Gt = ">",
        Question = "?",
        Underscore = "_",
    }
}

impl TokenKind {
    /// L17 condition 2: can this token end a statement?
    pub fn can_end_statement(self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Ident
                | Int
                | Float
                | Suffixed
                | Str
                | True
                | False
                | SelfValue
                | SelfType
                | Return
                | Break
                | Continue
                | RParen
                | RBracket
                | RBrace
                | Gt
                | Shr
                | Question
        )
    }

    /// The opening bracket that this closing bracket closes.
    pub fn opener(self) -> Option<TokenKind> {
        match self {
            TokenKind::RParen => Some(TokenKind::LParen),
            TokenKind::RBracket => Some(TokenKind::LBracket),
            TokenKind::RBrace => Some(TokenKind::LBrace),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// A line break comes between the previous token (or the file start) and this one.
    pub line_break_before: bool,
}

/// A comment, kept for the formatter (L8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub span: Span,
    /// The comment's text, `//` included.
    pub text: String,
    pub doc: bool,
    /// Nothing but whitespace comes before it on its line.
    pub own_line: bool,
}
