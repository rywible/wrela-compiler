//! What can go wrong loading or running a program.

use std::fmt;
use std::path::PathBuf;
use wrela_abi::manifest::ManifestError;
use wrela_abi::stream::{Opcode, StreamError};

/// Everything the host can report. Each variant says whose fault it is: the build directory
/// (`Io`, `Manifest`, `Program`, `Shader`), the program at run time (`Stream`, `Command`, `Trap`),
/// or the machine (`Gpu`, `Lock`).
#[derive(Debug)]
pub enum Error {
    /// A file in the build directory couldn't be read, or an output couldn't be written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Manifest(ManifestError),
    /// The WASM module is invalid or breaks the program ABI (imports, exports).
    Program(String),
    /// A pipeline's WGSL failed to compile, or its pipeline failed to build.
    Shader {
        pipeline: String,
        shader: String,
        message: String,
    },
    /// A submitted batch is malformed or out of sequence.
    Stream(StreamError),
    /// A well-formed command the host can't carry out: an unknown handle, a pipeline of the wrong
    /// kind, the wrong number of buffers or uniform bytes, a write past a buffer's end.
    Command {
        opcode: Opcode,
        why: String,
    },
    /// The program trapped, or called the host with arguments it can't use.
    Trap(String),
    /// The GPU, its adapter or device failed, or reported a validation error.
    Gpu(String),
    /// The GPU lock (see [`crate::lock`]) couldn't be taken.
    Lock(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Manifest(e) => write!(f, "{e}"),
            Error::Program(why) => write!(f, "invalid program: {why}"),
            Error::Shader { pipeline, shader, message } => {
                write!(f, "pipeline `{pipeline}` (shader {shader}) failed to build:\n{message}")
            }
            Error::Stream(e) => write!(f, "{e}"),
            Error::Command { opcode, why } => write!(f, "{} failed: {why}", opcode.name()),
            Error::Trap(why) => write!(f, "the program trapped: {why}"),
            Error::Gpu(why) => write!(f, "GPU error: {why}"),
            Error::Lock(why) => write!(f, "GPU lock: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Manifest(e) => Some(e),
            Error::Stream(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StreamError> for Error {
    fn from(e: StreamError) -> Error {
        Error::Stream(e)
    }
}

impl From<ManifestError> for Error {
    fn from(e: ManifestError) -> Error {
        Error::Manifest(e)
    }
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
        Error::Io { path: path.into(), source }
    }

    pub(crate) fn command(opcode: Opcode, why: impl Into<String>) -> Error {
        Error::Command { opcode, why: why.into() }
    }
}
