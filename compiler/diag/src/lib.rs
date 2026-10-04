//! Diagnostics for the wrela compiler: a registry of codes, source files and spans, and
//! rendering for people (`render`) and for tools (`json`, D-021).

pub mod codes;
mod diagnostic;
pub mod json;
pub mod render;
mod source;

pub use codes::Code;
pub use diagnostic::{
    Diagnostic, Edit, Fix, Label, Severity, apply_edits, has_errors, sort_and_dedup,
};
pub use source::{FileId, LineCol, SourceFile, SourceMap, Span};

/// The plural ending of a noun counting `n` things: `"s"`, or `""` when `n` is 1.
pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
