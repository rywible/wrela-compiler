//! Why an IR pass failed: a program it can't handle, which is the user's to change, or a bug in
//! the compiler.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The function can't be given a derived interpretation (language.md §13): it uses
    /// something the interpretation doesn't support.
    NotDerivable(String),
    /// An interval of a function with a loop whose exit depends on the input.
    ActiveLoopExit(String),
    /// A bug: the IR breaks a rule the pass relies on.
    Internal(String),
}

impl Error {
    pub fn not_derivable(message: impl Into<String>) -> Error {
        Error::NotDerivable(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Error {
        Error::Internal(message.into())
    }

    pub fn message(&self) -> &str {
        match self {
            Error::NotDerivable(m) | Error::ActiveLoopExit(m) | Error::Internal(m) => m,
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
