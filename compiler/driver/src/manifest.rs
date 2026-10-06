//! `wrela.toml`, a package's manifest (language.md §3). A package needs one only to declare
//! dependencies or `unsafe`:
//!
//! ```text
//! [package]
//! name = "herd"             # how diagnostics name the package; defaults to its directory's
//! unsafe = true             # it uses `unsafe` (E0214 otherwise)
//!
//! [dependencies]
//! fieldkit = { path = "../fieldkit" }    # code names it `fieldkit::...`
//! ```
//!
//! The file is read as the subset of TOML these need: `[section]` headers, `key = value` lines,
//! and values that are strings, `true`, `false`, or an inline table of those. Anything else is
//! E0219, at its place in the file.

use wrela_diag::{Diagnostic, FileId, Span, codes};

/// What a manifest says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub name: Option<(String, Span)>,
    pub unsafe_ok: bool,
    /// Each dependency: its name, its path (relative to the manifest's directory), and where
    /// it's declared.
    pub deps: Vec<Dependency>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub path: String,
    pub span: Span,
}

/// A value in the subset of TOML a manifest uses.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    Bool(bool),
    Table(Vec<(String, Value, Span)>),
}

/// The name the manifest in `dir` gives its package, if it has one that reads.
pub fn name_in(dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("wrela.toml")).ok()?;
    parse(FileId(0), &text).ok()?.name.map(|(n, _)| n)
}

/// Reads the manifest `text`, which is file `file` in the source map. Errors are E0219.
pub fn parse(file: FileId, text: &str) -> Result<Manifest, Vec<Diagnostic>> {
    let mut out = Manifest::default();
    let mut errors = Vec::new();
    let mut section = String::new();
    let mut offset = 0u32;
    let bad = |at: u32, len: usize, msg: String| {
        Diagnostic::new(codes::E0219, Span::new(file, at, at + len as u32), msg)
    };
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len() as u32;
        let content = strip_comment(line).trim_end();
        let lead = content.len() - content.trim_start().len();
        let content = content.trim_start();
        let at = start + lead as u32;
        if content.is_empty() {
            continue;
        }
        if let Some(rest) = content.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                errors.push(bad(at, content.len(), "a section header ends with `]`".into()));
                continue;
            };
            section = name.trim().to_string();
            if section != "package" && section != "dependencies" {
                errors.push(
                    bad(
                        at,
                        content.len(),
                        format!("`[{section}]` isn't a section of `wrela.toml`"),
                    )
                    .with_note("a manifest has `[package]` and `[dependencies]` (language.md §3)"),
                );
            }
            continue;
        }
        let Some((key, value)) = content.split_once('=') else {
            errors.push(bad(at, content.len(), "a line of `wrela.toml` is `key = value`".into()));
            continue;
        };
        let key = key.trim();
        let vat = at
            + (content.len() - content[content.find('=').unwrap_or(0) + 1..].trim_start().len())
                as u32;
        let value = match read_value(value.trim(), vat, file) {
            Ok((v, rest)) if rest.trim().is_empty() => v,
            Ok(_) => {
                errors.push(bad(vat, value.trim().len(), "unexpected text after the value".into()));
                continue;
            }
            Err(d) => {
                errors.push(*d);
                continue;
            }
        };
        let span = Span::new(file, at, at + content.len() as u32);
        match (section.as_str(), key, value) {
            ("package", "name", Value::Str(s)) => {
                if !wrela_syntax::lexer::is_name(&s) {
                    errors.push(bad(at, content.len(), format!("`{s}` isn't a wrela name, so it can't name a package")));
                }
                out.name = Some((s, span));
            }
            ("package", "unsafe", Value::Bool(b)) => out.unsafe_ok = b,
            ("dependencies", name, Value::Table(fields)) => {
                let mut path = None;
                let mut other = false;
                for (k, v, kspan) in fields {
                    match (k.as_str(), v) {
                        ("path", Value::Str(p)) => path = Some(p),
                        (k, _) => {
                            other = true;
                            errors.push(
                            Diagnostic::new(codes::E0219, kspan, format!("a dependency has a `path`, not `{k}`"))
                                .with_note("dependencies are local paths; versions and registries come later (#31)"),
                            )
                        }
                    }
                }
                if !wrela_syntax::lexer::is_name(name) {
                    errors.push(bad(at, key.len(), format!("`{name}` isn't a wrela name, so code can't name the dependency")));
                    continue;
                }
                match path {
                    Some(path) => out.deps.push(Dependency { name: name.to_string(), path, span }),
                    // A key that isn't `path` is reported already.
                    None if other => {}
                    None => errors.push(bad(at, content.len(), format!("the dependency `{name}` needs a `path`"))
                        .with_help(format!("write `{name} = {{ path = \"../{name}\" }}`"))),
                }
            }
            ("", _, _) => errors.push(bad(at, content.len(), "a key outside a section".into())
                .with_help("put it under `[package]` or `[dependencies]`")),
            (sec, key, _) => errors.push(bad(at, content.len(), format!("`{key}` isn't a key of `[{sec}]`, or its value has the wrong type"))
                .with_note("`[package]` has `name = \"...\"` and `unsafe = true`; `[dependencies]` has `name = { path = \"...\" }`")),
        }
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

/// The line without a `#` comment (outside a string).
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            '\\' if in_str && !escaped => {
                escaped = true;
                continue;
            }
            '"' if !escaped => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
        escaped = false;
    }
    line
}

/// Reads a value at the start of `s` (at offset `at` in the file): it and the text after it.
fn read_value(s: &str, at: u32, file: FileId) -> Result<(Value, &str), Box<Diagnostic>> {
    let bad = |msg: &str| {
        Diagnostic::new(
            codes::E0219,
            Span::new(file, at, at + s.len().max(1) as u32),
            msg.to_string(),
        )
    };
    if let Some(rest) = s.strip_prefix("true") {
        return Ok((Value::Bool(true), rest));
    }
    if let Some(rest) = s.strip_prefix("false") {
        return Ok((Value::Bool(false), rest));
    }
    if let Some(rest) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return Ok((Value::Str(out), &rest[i + 1..])),
                '\\' => match chars.next() {
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, 'n')) => out.push('\n'),
                    _ => return Err(Box::new(bad("a string's escape is `\\\"`, `\\\\` or `\\n`"))),
                },
                c => out.push(c),
            }
        }
        return Err(Box::new(bad("a string ends with `\"`")));
    }
    if let Some(mut rest) = s.strip_prefix('{') {
        let mut fields = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(r) = rest.strip_prefix('}') {
                return Ok((Value::Table(fields), r));
            }
            let Some((key, after)) = rest.split_once('=') else {
                return Err(Box::new(bad("an inline table is `{ key = value, ... }`")));
            };
            let key = key.trim().to_string();
            let off = at + (s.len() - rest.len()) as u32;
            let kspan = Span::new(file, off, off + key.len() as u32);
            let vat = at + (s.len() - after.trim_start().len()) as u32;
            let (v, r) = read_value(after.trim_start(), vat, file)?;
            fields.push((key, v, kspan));
            rest = r.trim_start();
            if let Some(r) = rest.strip_prefix(',') {
                rest = r;
            } else if !rest.starts_with('}') {
                return Err(Box::new(bad("an inline table's values are separated by `,`")));
            }
        }
    }
    Err(Box::new(bad("a value is a string, `true`, `false` or `{ ... }`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> FileId {
        FileId(0)
    }

    #[test]
    fn reads_a_manifest() {
        let m = parse(
            file(),
            "# a comment\n[package]\nname = \"herd\"  # its name\nunsafe = true\n\n[dependencies]\nfieldkit = { path = \"../fieldkit\" }\n",
        )
        .expect("parses");
        assert_eq!(m.name.map(|n| n.0), Some("herd".to_string()));
        assert!(m.unsafe_ok);
        assert_eq!(m.deps.len(), 1);
        assert_eq!((m.deps[0].name.as_str(), m.deps[0].path.as_str()), ("fieldkit", "../fieldkit"));
    }

    #[test]
    fn rejects_what_it_doesnt_know() {
        for text in [
            "[workspace]\n",
            "[package]\nversion = \"1.0\"\n",
            "[dependencies]\nx = { git = \"...\" }\n",
            "[dependencies]\nx = { path = \"../x\" \n",
            "name = \"x\"\n",
            "[package]\nname = 3\n",
        ] {
            assert!(parse(file(), text).is_err(), "{text}");
        }
    }
}
