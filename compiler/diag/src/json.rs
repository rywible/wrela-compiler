//! Machine-readable diagnostics (D-021).
//!
//! The format is versioned by [`JSON_VERSION`]. Consumers must ignore keys they don't know.
//! Adding a key is compatible; removing or renaming one, or changing what one means, bumps the
//! version. Offsets are bytes into the file's UTF-8 text; lines and columns are 1-based, and
//! columns count characters.
//!
//! ```json
//! {"version": 1, "diagnostics": [{
//!   "code": "E0500", "severity": "error", "title": "a use of a moved value",
//!   "message": "...", "file": "main.wrela",
//!   "span": {"start": 16, "end": 19, "line": 2, "column": 5, "end_line": 2, "end_column": 8},
//!   "label": "used here after the move",
//!   "labels": [{"file": "...", "span": {...}, "message": "..."}],
//!   "notes": ["..."], "help": ["..."],
//!   "fixes": [{"message": "...", "edits": [{"file": "...", "start": 16, "end": 19, "replacement": "..."}]}]
//! }]}
//! ```

use crate::diagnostic::Diagnostic;
use crate::source::{SourceMap, Span};
use serde_json::{Value, json};

/// The JSON format's version.
pub const JSON_VERSION: u32 = 1;

fn span_json(map: &SourceMap, span: Span) -> Value {
    let file = map.file(span.file);
    let a = file.line_col(span.start);
    let b = file.line_col(span.end);
    json!({
        "start": span.start, "end": span.end,
        "line": a.line, "column": a.column,
        "end_line": b.line, "end_column": b.column,
    })
}

pub fn diagnostic_json(map: &SourceMap, d: &Diagnostic) -> Value {
    let labels: Vec<Value> = d
        .secondary
        .iter()
        .map(|l| {
            json!({
                "file": map.file(l.span.file).name,
                "span": span_json(map, l.span),
                "message": l.message,
            })
        })
        .collect();
    let fixes: Vec<Value> = d
        .fixes
        .iter()
        .map(|f| {
            json!({
                "message": f.message,
                "edits": f.edits.iter().map(|e| json!({
                    "file": map.file(e.span.file).name,
                    "start": e.span.start, "end": e.span.end,
                    "replacement": e.replacement,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "code": d.code.as_str(),
        "severity": d.severity.as_str(),
        "title": d.code.title(),
        "message": d.message,
        "file": map.file(d.primary.span.file).name,
        "span": span_json(map, d.primary.span),
        "label": d.primary.message,
        "labels": labels,
        "notes": d.notes,
        "help": d.help,
        "fixes": fixes,
    })
}

/// The whole report, as one JSON document.
pub fn to_json(map: &SourceMap, diags: &[Diagnostic]) -> Value {
    json!({
        "version": JSON_VERSION,
        "diagnostics": diags.iter().map(|d| diagnostic_json(map, d)).collect::<Vec<_>>(),
    })
}

pub fn to_json_string(map: &SourceMap, diags: &[Diagnostic]) -> String {
    let mut s = serde_json::to_string_pretty(&to_json(map, diags)).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes;

    #[test]
    fn json_has_version_and_positions() {
        let mut map = SourceMap::new();
        let f = map.add("m.wrela", "let x = 1\nfoo\n");
        let d = Diagnostic::new(codes::E0200, Span::new(f, 10, 13), "no `foo` here").with_fix(
            "rename",
            Span::new(f, 10, 13),
            "x",
        );
        let v = to_json(&map, &[d]);
        assert_eq!(v["version"], 1);
        let d = &v["diagnostics"][0];
        assert_eq!(d["code"], "E0200");
        assert_eq!(d["span"]["line"], 2);
        assert_eq!(d["span"]["column"], 1);
        assert_eq!(d["fixes"][0]["edits"][0]["replacement"], "x");
    }
}
