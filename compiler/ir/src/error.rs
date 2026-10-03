//! Why an IR pass failed: a program it can't handle, which is the user's to change, or a bug in
//! the compiler.

use std::fmt;

use wrela_diag::Span;

/// Why an IR pass failed. A program error says where in the source, when the IR knows
/// (`Stmt::At`).
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The function can't be given a derived interpretation (language.md §13): it uses
    /// something the interpretation doesn't support.
    NotDerivable(String, Option<Span>),
    /// An interval of a function with a loop whose exit depends on the input.
    ActiveLoopExit(String, Option<Span>),
    /// A bug: the IR breaks a rule the pass relies on.
    Internal(String),
}

impl Error {
    pub fn not_derivable(message: impl Into<String>) -> Error {
        Error::NotDerivable(message.into(), None)
    }

    pub fn internal(message: impl Into<String>) -> Error {
        Error::Internal(message.into())
    }

    pub fn message(&self) -> &str {
        match self {
            Error::NotDerivable(m, _) | Error::ActiveLoopExit(m, _) | Error::Internal(m) => m,
        }
    }

    /// Where in the source the program's error is, if known.
    pub fn span(&self) -> Option<Span> {
        match self {
            Error::NotDerivable(_, s) | Error::ActiveLoopExit(_, s) => *s,
            Error::Internal(_) => None,
        }
    }

    /// The error, placed at `at` unless it's placed already: the innermost place wins.
    pub fn at(self, at: Option<Span>) -> Error {
        match self {
            Error::NotDerivable(m, None) => Error::NotDerivable(m, at),
            Error::ActiveLoopExit(m, None) => Error::ActiveLoopExit(m, at),
            e => e,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
