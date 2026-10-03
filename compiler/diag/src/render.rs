//! Human-readable rendering, in the familiar rustc shape:
//!
//! ```text
//! error[E0500]: `log` was moved into `archive` on line 5
//!  --> main.wrela:6:1
//!   |
//! 6 | log.push(edit)
//!   | ^^^ used here after the move
//!   = help: write `archive(log.clone())` there if you still need `log`
//! ```

use crate::diagnostic::{Diagnostic, Label};
use crate::plural;
use crate::source::SourceMap;
use std::fmt::Write;

pub fn render(map: &SourceMap, d: &Diagnostic) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}[{}]: {}", d.severity.as_str(), d.code, d.message);
    let Some(primary) = &d.primary else {
        // An internal error: no place in the program to show.
        for note in &d.notes {
            let _ = writeln!(out, "  = note: {note}");
        }
        return out;
    };
    let file = map.file(primary.span.file);
    // Each label in this file, with the 0-based line it starts on.
    let labels: Vec<(&Label, usize)> = std::iter::once(primary)
        .chain(d.secondary.iter().filter(|l| l.span.file == primary.span.file))
        .map(|l| (l, file.line_index(l.span.start)))
        .collect();
    let width = labels.iter().map(|(_, line)| (line + 1).to_string().len()).max().unwrap_or(1);
    let pad = " ".repeat(width);
    let _ = writeln!(out, "{pad}--> {}", file.location(primary.span.start));
    let _ = writeln!(out, "{pad} |");
    let mut lines: Vec<usize> = labels.iter().map(|&(_, line)| line).collect();
    lines.sort_unstable();
    lines.dedup();
    for line in lines {
        let text = file.line_text(line);
        let _ = writeln!(out, "{:>width$} | {}", line + 1, text.replace('\t', "    "));
        for (i, &(label, at)) in labels.iter().enumerate() {
            if at != line {
                continue;
            }
            let start = file.line_col(label.span.start).column as usize - 1;
            let end_offset = label.span.end.min(file.line_end(line));
            let end = (file.line_col(end_offset).column as usize - 1).max(start + 1);
            let prefix: String =
                text.chars().take(start).map(|c| if c == '\t' { "    " } else { " " }).collect();
            let mark = if i == 0 { "^" } else { "-" };
            let marks = mark.repeat(end - start);
            match &label.message {
                Some(m) => {
                    let _ = writeln!(out, "{pad} | {prefix}{marks} {m}");
                }
                None => {
                    let _ = writeln!(out, "{pad} | {prefix}{marks}");
                }
            }
        }
    }
    for label in d.secondary.iter().filter(|l| l.span.file != primary.span.file) {
        let at = map.file(label.span.file).location(label.span.start);
        let msg = label.message.as_deref().unwrap_or("");
        let _ = writeln!(out, "{pad} = note: {at}: {msg}");
    }
    for note in &d.notes {
        let _ = writeln!(out, "{pad} = note: {note}");
    }
    for help in &d.help {
        let _ = writeln!(out, "{pad} = help: {help}");
    }
    for fix in &d.fixes {
        let _ = writeln!(out, "{pad} = fix: {}", fix.message);
    }
    out
}

/// Renders every diagnostic, then a summary line when there are errors.
pub fn render_all(map: &SourceMap, diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diags {
        out.push_str(&render(map, d));
        out.push('\n');
    }
    let errors = diags.iter().filter(|d| d.is_error()).count();
    let warnings = diags.len() - errors;
    if errors > 0 {
        let _ = writeln!(
            out,
            "{} error{}{}",
            errors,
            plural(errors),
            if warnings > 0 {
                format!(", {} warning{}", warnings, plural(warnings))
            } else {
                String::new()
            }
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes;
    use crate::source::Span;

    #[test]
    fn renders_a_caret_under_the_span() {
        let mut map = SourceMap::new();
        let f = map.add("main.wrela", "fn main() {\n    log.push(edit)\n}\n");
        let d = Diagnostic::new(codes::E0500, Span::new(f, 16, 19), "`log` was moved")
            .with_label("used here after the move")
            .with_help("write `log.clone()`");
        let text = render(&map, &d);
        assert_eq!(
            text,
            "error[E0500]: `log` was moved\n --> main.wrela:2:5\n  |\n2 |     log.push(edit)\n  |     ^^^ used here after the move\n  = help: write `log.clone()`\n"
        );
    }
}
