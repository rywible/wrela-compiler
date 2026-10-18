//! `wrela.toml`, a package's manifest (language.md §3.6). A package needs one only to declare
//! dependencies or `unsafe`:
//!
//! ```text
//! [package]
//! name = "herd"             # how diagnostics name the package; defaults to its directory's
//! unsafe = true             # it uses `unsafe` (E0214 otherwise)
//!
//! [dependencies]
//! fieldkit = { path = "../fieldkit" }    # code names it `fieldkit::...`
//! geo = { workspace = true }             # the member of the workspace named `geo`
//! ```
//!
//! A manifest can also declare a workspace, whose members are packages found by name
//! ([`crate::workspace`]):
//!
//! ```text
//! [workspace]
//! members = ["geo", "content/*"]     # paths from this file; `/*` is each package in it
//! ```
//!
//! The file is read as the subset of TOML these need: `[section]` headers, `key = value` lines,
//! and values that are strings, `true`, `false`, an inline table of those, or an array (which
//! may run over several lines). Anything else is E0219, at its place in the file.

use std::collections::HashSet;
use wrela_diag::{Diagnostic, FileId, Span, codes};

/// What a manifest says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub name: Option<(String, Span)>,
    /// It has a `[package]` section. A manifest with `[workspace]` and no `[package]` is the
    /// workspace's alone, not a package's.
    pub package: bool,
    pub unsafe_ok: bool,
    /// Each dependency: its name, where it's found, and where it's declared.
    pub deps: Vec<Dependency>,
    /// Its `[workspace]`, if it declares one.
    pub workspace: Option<WorkspaceDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub source: Source,
    pub span: Span,
}

/// Where a dependency is found.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// `{ path = "..." }`: a directory, relative to the manifest's.
    Path(String),
    /// `{ workspace = true }`: the member of the package's workspace with the dependency's name.
    Workspace,
}

/// A manifest's `[workspace]`.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceDecl {
    /// The `[workspace]` header.
    pub span: Span,
    /// Each entry of `members`, a path relative to the manifest's directory or one ending `/*`,
    /// and its place.
    pub members: Vec<(String, Span)>,
}

/// A value in the subset of TOML a manifest uses.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    Bool(bool),
    Table(Vec<(String, Value, Span)>),
    Array(Vec<(Value, Span)>),
}

/// The name the manifest in `dir` gives its package, if it has one that reads.
pub fn name_in(dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("wrela.toml")).ok()?;
    parse(FileId(0), &text).ok()?.name.map(|(n, _)| n)
}

/// Whether the manifest `text` has a `[workspace]` header, read without the rest of it: how a
/// package finds its workspace above it, past manifests that may not read.
pub fn declares_workspace(text: &str) -> bool {
    text.lines().any(|l| {
        strip_comment(l)
            .trim()
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .is_some_and(|s| s.trim() == "workspace")
    })
}

/// Reads the manifest `text`, which is file `file` in the source map. Errors are E0219.
pub fn parse(file: FileId, text: &str) -> Result<Manifest, Vec<Diagnostic>> {
    // Comments become spaces, so a value that runs over lines reads as one text, at its place.
    let text = &blank_comments(text);
    let mut out = Manifest::default();
    let mut errors = Vec::new();
    let mut section = String::new();
    let mut keys = HashSet::new();
    let mut sections = HashSet::new();
    let mut offset = 0u32;
    // The end of a value that ran over lines: the lines before it are read.
    let mut skip_to = 0u32;
    let bad = |at: u32, len: usize, msg: String| {
        Diagnostic::new(codes::E0219, Span::new(file, at, at + len as u32), msg)
    };
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len() as u32;
        if start < skip_to {
            continue;
        }
        let content = line.trim_end();
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
            let span = Span::new(file, at, at + content.len() as u32);
            // As in TOML, a section is declared once: a second `[workspace]` would replace the
            // first's members.
            if !sections.insert(section.clone()) {
                errors.push(bad(
                    at,
                    content.len(),
                    format!("`[{section}]` is declared more than once"),
                ));
                continue;
            }
            match section.as_str() {
                "package" => out.package = true,
                "dependencies" => {}
                "workspace" => out.workspace = Some(WorkspaceDecl { span, members: Vec::new() }),
                _ => errors.push(
                    bad(
                        at,
                        content.len(),
                        format!("`[{section}]` isn't a section of `wrela.toml`"),
                    )
                    .with_note("a manifest has `[package]`, `[dependencies]` and `[workspace]` (language.md §3.6)"),
                ),
            }
            continue;
        }
        let Some((key, value)) = content.split_once('=') else {
            errors.push(bad(at, content.len(), "a line of `wrela.toml` is `key = value`".into()));
            continue;
        };
        let key = key.trim();
        if !keys.insert((section.clone(), key.to_string())) {
            errors.push(bad(
                at,
                key.len(),
                format!("`{key}` is declared more than once in `[{section}]`"),
            ));
            continue;
        }
        let vat = at
            + (content.len() - content[content.find('=').unwrap_or(0) + 1..].trim_start().len())
                as u32;
        // An array may run over lines: its text is to the end of the line it ends on.
        let value = if value.trim_start().starts_with('[') {
            let from = vat as usize;
            match array_end(&text[from..]) {
                Some(n) => {
                    let end = text[from + n..].find('\n').map_or(text.len(), |i| from + n + i);
                    skip_to = end as u32;
                    &text[from..end]
                }
                None => {
                    errors.push(bad(vat, value.trim().len(), "an array ends with `]`".into()));
                    break;
                }
            }
        } else {
            value
        };
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
                let mut workspace = None;
                let mut other = false;
                for (k, v, kspan) in fields {
                    match (k.as_str(), v) {
                        ("path", Value::Str(p)) if path.is_none() => path = Some(p),
                        ("workspace", Value::Bool(true)) if workspace.is_none() => workspace = Some(kspan),
                        ("path", Value::Str(_)) | ("workspace", Value::Bool(true)) => {
                            errors.push(Diagnostic::new(
                                codes::E0219,
                                kspan,
                                format!("a dependency's `{k}` is declared more than once"),
                            ));
                        }
                        ("workspace", _) => {
                            other = true;
                            errors.push(
                                Diagnostic::new(codes::E0219, kspan, "a dependency's `workspace` is `true`")
                                    .with_help("write `workspace = true` to find it in the workspace, or give it a `path` instead"),
                            );
                        }
                        (k, _) => {
                            other = true;
                            errors.push(
                            Diagnostic::new(codes::E0219, kspan, format!("a dependency has a `path` or `workspace = true`, not `{k}`"))
                                .with_note("dependencies are local packages; versions and registries come later (#31)"),
                            )
                        }
                    }
                }
                if !wrela_syntax::lexer::is_name(name) {
                    errors.push(bad(at, key.len(), format!("`{name}` isn't a wrela name, so code can't name the dependency")));
                    continue;
                }
                let source = match (path, workspace) {
                    (Some(path), None) => Source::Path(path),
                    (None, Some(_)) => Source::Workspace,
                    (Some(_), Some(kspan)) => {
                        errors.push(
                            Diagnostic::new(codes::E0219, kspan, format!("the dependency `{name}` has a `path` and `workspace = true`"))
                                .with_help("keep one: a dependency is found by its path or by its name in the workspace"),
                        );
                        continue;
                    }
                    // A key that isn't `path` or `workspace` is reported already.
                    (None, None) if other => continue,
                    (None, None) => {
                        errors.push(bad(at, content.len(), format!("the dependency `{name}` needs a `path`"))
                            .with_help(format!("write `{name} = {{ path = \"../{name}\" }}`, or `{name} = {{ workspace = true }}` for a member of the workspace")));
                        continue;
                    }
                };
                out.deps.push(Dependency { name: name.to_string(), source, span });
            }
            ("workspace", "members", Value::Array(items)) => {
                let mut members = Vec::new();
                for (v, vspan) in items {
                    match v {
                        Value::Str(s) if is_member_path(&s) => members.push((s, vspan)),
                        Value::Str(s) => errors.push(
                            Diagnostic::new(codes::E0219, vspan, format!("`{s}` isn't a member's path"))
                                .with_note("a member is a directory's path from this file, or a path ending `/*` for each directory in it that has a `wrela.toml`"),
                        ),
                        _ => errors.push(Diagnostic::new(codes::E0219, vspan, "a workspace's members are strings, each a path")),
                    }
                }
                if let Some(w) = &mut out.workspace {
                    w.members = members;
                }
            }
            ("", _, _) => errors.push(bad(at, content.len(), "a key outside a section".into())
                .with_help("put it under `[package]`, `[dependencies]` or `[workspace]`")),
            (sec, key, _) => errors.push(bad(at, content.len(), format!("`{key}` isn't a key of `[{sec}]`, or its value has the wrong type"))
                .with_note(match sec {
                    "package" => "`[package]` has `name = \"...\"` and `unsafe = true`",
                    "dependencies" => "`[dependencies]` has `name = { path = \"...\" }` and `name = { workspace = true }`",
                    _ => "`[workspace]` has `members = [\"...\", ...]`",
                })),
        }
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

/// Whether `s` is a member's path: not empty, with `*` only as its last part (`content/*`).
fn is_member_path(s: &str) -> bool {
    let dir = s.strip_suffix("/*").or(if s == "*" { Some("") } else { None }).unwrap_or(s);
    !s.is_empty() && !dir.contains('*')
}

/// `text` with each `#` comment (outside a string) made spaces, so every other byte keeps its
/// place.
fn blank_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let code = strip_comment(line);
        out.push_str(code);
        let rest = &line[code.len()..];
        let newline = rest.len() - rest.trim_end_matches(['\n', '\r']).len();
        out.extend(std::iter::repeat_n(' ', rest.len() - newline));
        out.push_str(&rest[rest.len() - newline..]);
    }
    out
}

/// The length of the array at the start of `s` (which starts with `[`), to its closing `]`
/// outside strings, if it has one.
fn array_end(s: &str) -> Option<usize> {
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for (i, c) in s.char_indices() {
        match c {
            '\\' if in_str && !escaped => {
                escaped = true;
                continue;
            }
            '"' if !escaped => in_str = !in_str,
            '[' if !in_str => depth += 1,
            ']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        escaped = false;
    }
    None
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
                '\n' => break,
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
    if let Some(mut rest) = s.strip_prefix('[') {
        let mut items = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(r) = rest.strip_prefix(']') {
                return Ok((Value::Array(items), r));
            }
            let vat = at + (s.len() - rest.len()) as u32;
            let (v, r) = read_value(rest, vat, file)?;
            items.push((v, Span::new(file, vat, at + (s.len() - r.len()) as u32)));
            rest = r.trim_start();
            if let Some(r) = rest.strip_prefix(',') {
                rest = r;
            } else if !rest.starts_with(']') {
                return Err(Box::new(bad("an array's values are separated by `,`")));
            }
        }
    }
    Err(Box::new(bad("a value is a string, `true`, `false`, `{ ... }` or `[ ... ]`")))
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
            "# a comment\n[package]\nname = \"herd\"  # its name\nunsafe = true\n\n[dependencies]\nfieldkit = { path = \"../fieldkit\" }\ngeo = { workspace = true }\n",
        )
        .expect("parses");
        assert_eq!(m.name.map(|n| n.0), Some("herd".to_string()));
        assert!(m.package && m.unsafe_ok && m.workspace.is_none());
        let deps: Vec<_> = m.deps.iter().map(|d| (d.name.as_str(), &d.source)).collect();
        assert_eq!(
            deps,
            [("fieldkit", &Source::Path("../fieldkit".into())), ("geo", &Source::Workspace)]
        );
    }

    #[test]
    fn reads_a_workspace() {
        let text = "[workspace]\nmembers = [\n    \"geo\",  # shapes\n    \"content/*\", # [not] a section\n]\n";
        let m = parse(file(), text).expect("parses");
        assert!(!m.package);
        let w = m.workspace.expect("a workspace");
        let members: Vec<_> = w
            .members
            .iter()
            .map(|(p, s)| (p.as_str(), &text[s.start as usize..s.end as usize]))
            .collect();
        assert_eq!(members, [("geo", "\"geo\""), ("content/*", "\"content/*\"")]);
        assert!(declares_workspace(text));
        assert!(!declares_workspace("[package]\n# [workspace]\nname = \"x\"\n"));
        let one =
            parse(file(), "[package]\nname = \"x\"\n[workspace]\nmembers = [\"a\", \"b/*\",]\n")
                .expect("parses");
        assert!(one.package);
        assert_eq!(one.workspace.expect("a workspace").members.len(), 2);
    }

    #[test]
    fn rejects_what_it_doesnt_know() {
        for text in [
            "[workspaces]\n",
            "[workspace]\ntoolchain = \"0.1.0\"\n",
            "[workspace]\nmembers = \"geo\"\n",
            "[workspace]\nmembers = [\"geo\"\n",
            "[workspace]\nmembers = [\"a\" \"b\"]\n",
            "[workspace]\nmembers = [true]\n",
            "[workspace]\nmembers = [\"\"]\n",
            "[workspace]\nmembers = [\"games/*/session\"]\n",
            "[workspace]\nmembers = [\"games/l*\"]\n",
            "[package]\nversion = \"1.0\"\n",
            "[dependencies]\nx = { git = \"...\" }\n",
            "[dependencies]\nx = { workspace = false }\n",
            "[dependencies]\nx = { workspace = \"yes\" }\n",
            "[dependencies]\nx = { path = \"../x\", workspace = true }\n",
            "[dependencies]\nx = { path = \"../x\" \n",
            "[dependencies]\nx = [\"../x\"]\n",
            "name = \"x\"\n",
            "[package]\nname = 3\n",
        ] {
            assert!(parse(file(), text).is_err(), "{text}");
        }
    }

    #[test]
    fn duplicate_manifest_keys_are_errors() {
        for text in [
            "[package]\nname = \"one\"\nname = \"two\"\n",
            "[dependencies]\nmath = { path = \"../one\" }\nmath = { path = \"../two\" }\n",
            "[dependencies]\nmath = { path = \"../one\", path = \"../two\" }\n",
            "[dependencies]\nmath = { workspace = true, workspace = true }\n",
            "[workspace]\nmembers = [\"a\"]\nmembers = [\"b\"]\n",
            "[workspace]\nmembers = [\"a\"]\n[workspace]\nmembers = [\"b\"]\n",
            "[workspace]\n[package]\nname = \"x\"\n[workspace]\n",
            "[dependencies]\na = { path = \"../a\" }\n[dependencies]\nb = { path = \"../b\" }\n",
        ] {
            assert!(parse(file(), text).is_err(), "accepted duplicate key in {text}");
        }
    }
}
