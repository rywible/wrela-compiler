//! wrela's syntax: the lexer (spec/lexical.md), the AST, the hand-written parser (held to
//! spec/grammar.ebnf by the oracle tests in `wrela-grammar`) and the formatter.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod token;

pub use lexer::{Lexed, lex};
pub use parser::{MAX_EXPR_DEPTH, MAX_NESTING, Parsed, parse};
pub use token::{Comment, Token, TokenKind};
pub mod fmt;
