//! Diagnostics for the wrela compiler: a registry of codes, source files and spans, and
//! rendering for people (`render`) and for tools (`json`, D-021).

pub mod codes;
mod diagnostic;
pub mod json;
pub mod render;
mod source;

pub use codes::Code;
pub use diagnostic::{Diagnostic, Edit, Fix, Label, Severity, sort_and_dedup};
pub use source::{FileId, LineCol, SourceFile, SourceMap, Span};

/// Applies every edit of the first fix of each diagnostic to `text` (all in one file). Edits
/// that overlap an earlier one are skipped. Used by tests that check a fix removes its
/// diagnostic.
pub fn apply_fixes(text: &str, file: FileId, diags: &[Diagnostic]) -> String {
    let mut edits: Vec<&Edit> = diags
        .iter()
        .filter_map(|d| d.fixes.first())
        .flat_map(|f| f.edits.iter())
        .filter(|e| e.span.file == file)
        .collect();
    edits.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    for e in edits {
        let (s, t) = (e.span.start as usize, e.span.end as usize);
        if s < pos {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str(&e.replacement);
        pos = t;
    }
    out.push_str(&text[pos..]);
    out
}
