//! Why an IR pass failed: a program it can't handle, which is the user's to change, or a bug in
//! the compiler.

use std::fmt;

use wrela_diag::Span;

/// Why an IR pass failed. A program error says where in the source, when the IR knows
/// (`Stmt::At`).
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The function can't be given a derived interpretation (language.md §14): it uses
    /// something the interpretation doesn't support.
    NotDerivable(String, Option<Span>),
    /// An interval of a function with a loop whose exit depends on the input.
    ActiveLoopExit(String, Option<Span>),
    /// A tape (`std::derive::tape`) of a function that uses what a tape can't hold, and the
    /// calls it was reached through, innermost first.
    NotTapeable(String, Option<Span>, Vec<Span>),
    /// A bug: the IR breaks a rule the pass relies on.
    Internal(String),
}

impl Error {
    pub fn not_derivable(message: impl Into<String>) -> Error {
        Error::NotDerivable(message.into(), None)
    }

    pub fn not_tapeable(message: impl Into<String>) -> Error {
        Error::NotTapeable(message.into(), None, Vec::new())
    }

    pub fn internal(message: impl Into<String>) -> Error {
        Error::Internal(message.into())
    }

    pub fn message(&self) -> &str {
        match self {
            Error::NotDerivable(m, _)
            | Error::ActiveLoopExit(m, _)
            | Error::NotTapeable(m, ..)
            | Error::Internal(m) => m,
        }
    }

    /// The error, reached through a call at `at` (a tape's: [`Error::NotTapeable`]).
    pub fn through(self, at: Option<Span>) -> Error {
        match self {
            Error::NotTapeable(m, s, mut calls) => {
                calls.extend(at);
                Error::NotTapeable(m, s, calls)
            }
            e => e,
        }
    }

    /// Where in the source the program's error is, if known.
    pub fn span(&self) -> Option<Span> {
        match self {
            Error::NotDerivable(_, s)
            | Error::ActiveLoopExit(_, s)
            | Error::NotTapeable(_, s, _) => *s,
            Error::Internal(_) => None,
        }
    }

    /// The error, placed at `at` unless it's placed already: the innermost place wins.
    pub fn at(self, at: Option<Span>) -> Error {
        match self {
            Error::NotDerivable(m, None) => Error::NotDerivable(m, at),
            Error::ActiveLoopExit(m, None) => Error::ActiveLoopExit(m, at),
            Error::NotTapeable(m, None, calls) => Error::NotTapeable(m, at, calls),
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
