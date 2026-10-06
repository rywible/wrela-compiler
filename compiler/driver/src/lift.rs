//! Lifted builds (language.md §22): the table of literals a build lifts, with what tools need
//! to show and edit them, and the build's report of every float literal in the lifted packages,
//! lifted or not and why (`lift.json`).

use std::collections::BTreeSet;
use std::path::Path;
use wrela_diag::{FileId, SourceMap};
use wrela_lower::{LiftTable, LiftedFile, LiftedLiteral};
use wrela_sema::Checked;
use wrela_syntax::token::TokenKind;

/// FNV-1a 64 of `bytes`, as 16 hex digits: how tools tell a file has changed.
pub fn file_hash(bytes: &[u8]) -> String {
    let mut h = wrela_abi::hash::StateHash::new();
    h.update(bytes);
    h.hex()
}

/// The table of a build that lifts the files `files` (each with its path relative to the
/// program's package).
pub fn table(checked: &Checked, sources: &SourceMap, files: &[(FileId, String)]) -> LiftTable {
    let lifted: BTreeSet<FileId> = files.iter().map(|f| f.0).collect();
    // Only a number written as one: a vector's components that a call fills in (`vec3(y: 1.0)`
    // has an `x` and a `z` of 0) have the call's place, and no literal of their own.
    // A negative literal is one, its minus included (`-0.42`: the build folds the negation).
    let mut numbers = BTreeSet::new();
    for &f in &lifted {
        let toks = wrela_syntax::lexer::lex(f, &sources.file(f).text).tokens;
        for (i, tok) in toks.iter().enumerate() {
            if tok.kind.is_number() {
                numbers.insert((f, tok.span.start, tok.span.end));
                if i > 0 && toks[i - 1].kind == TokenKind::Minus {
                    numbers.insert((f, toks[i - 1].span.start, tok.span.end));
                }
            }
        }
    }
    let cands: Vec<_> = wrela_lower::candidates(checked, &|f| lifted.contains(&f))
        .into_iter()
        .filter(|c| numbers.contains(&(c.0, c.1, c.2)))
        .collect();
    let mut t = LiftTable::default();
    let mut file_index = std::collections::HashMap::new();
    for (file, start, end, value, negated) in cands {
        let fi = *file_index.entry(file).or_insert_with(|| {
            let src = sources.file(file);
            let path = files.iter().find(|f| f.0 == file).map(|f| f.1.clone()).unwrap_or_default();
            t.files.push(LiftedFile {
                id: file,
                name: src.name.clone(),
                path,
                text: src.text.clone(),
                hash: file_hash(src.text.as_bytes()),
            });
            t.files.len() as u32 - 1
        });
        let lc = sources.file(file).line_col(start);
        let i = t.literals.len() as u32;
        t.literals.push(LiftedLiteral {
            file: fi,
            start,
            end,
            line: lc.line,
            column: lc.column,
            value,
            negated,
        });
        t.index.insert((file, start, end), i);
    }
    t
}

/// Why a float literal of a lifted file isn't lifted: where it is decides.
fn why_not(checked: &Checked, file: FileId, start: u32, end: u32) -> &'static str {
    let p = &checked.program;
    let inside = |s: wrela_diag::Span| s.file == file && s.start <= start && end <= s.end;
    for (i, c) in p.consts.iter().enumerate() {
        if inside(c.value.span) {
            let id = wrela_sema::ty::ConstId(i as u32);
            return if wrela_lower::is_computed(checked, id) {
                "in a constant the build computes (§10), once: it's data in the build"
            } else {
                "in a constant nothing in the program reads"
            };
        }
    }
    for f in &p.fns {
        if inside(f.span) && f.attrs.test.is_some() {
            return "in a test, which isn't in a build";
        }
    }
    "not an `f32` where it's used: an `f64`, or a number a pattern or a type holds"
}

/// The report: each lifted literal, each lifted file (with its text when built, so a tool can
/// find a literal in the file as it is now), and every other float literal in them with why it
/// isn't lifted.
pub fn report(checked: &Checked, sources: &SourceMap, t: &LiftTable) -> String {
    use serde_json::json;
    let literals: Vec<_> = t
        .literals
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let f = &t.files[l.file as usize];
            json!({
                "index": i,
                "file": l.file,
                "start": l.start,
                "end": l.end,
                "line": l.line,
                "column": l.column,
                "text": &f.text[l.start as usize..l.end as usize],
                "value": l.value,
                "negated": l.negated,
            })
        })
        .collect();
    let mut not_lifted = Vec::new();
    for (fi, f) in t.files.iter().enumerate() {
        for tok in wrela_syntax::lexer::lex(f.id, &f.text).tokens {
            if !matches!(tok.kind, TokenKind::Float | TokenKind::Suffixed) {
                continue;
            }
            let (s, e) = (tok.span.start, tok.span.end);
            let negated_lifted = t.literals.iter().any(|l| l.file as usize == fi && l.end == e);
            if t.index.contains_key(&(f.id, s, e)) || negated_lifted {
                continue;
            }
            let lc = sources.file(f.id).line_col(s);
            not_lifted.push(json!({
                "file": fi,
                "start": s,
                "end": e,
                "line": lc.line,
                "column": lc.column,
                "text": &f.text[s as usize..e as usize],
                "why": why_not(checked, f.id, s, e),
            }));
        }
    }
    let files: Vec<_> = t
        .files
        .iter()
        .map(|f| json!({ "name": f.name, "path": f.path, "hash": f.hash, "text": f.text }))
        .collect();
    crate::pretty(
        &json!({ "version": 1, "files": files, "literals": literals, "not_lifted": not_lifted }),
    )
}

/// A lifted build's report as tools read it back: each lifted file's path and text when built,
/// and each literal's place in its file and value.
#[derive(serde::Deserialize)]
pub struct Report {
    pub files: Vec<ReportFile>,
    pub literals: Vec<ReportLiteral>,
}

#[derive(serde::Deserialize)]
pub struct ReportFile {
    /// Relative to the program's package.
    pub path: String,
    pub text: String,
}

#[derive(serde::Deserialize)]
pub struct ReportLiteral {
    /// The file's index in [`Report::files`].
    pub file: u32,
    pub start: u32,
    pub end: u32,
    pub value: f32,
}

/// `path` relative to `base`, both real paths: `../wolf/parts.wrela` for a dependency beside the
/// program's package.
pub fn relative(path: &Path, base: &Path) -> String {
    let (p, b) =
        (path.canonicalize().unwrap_or(path.into()), base.canonicalize().unwrap_or(base.into()));
    let pc: Vec<_> = p.components().collect();
    let bc: Vec<_> = b.components().collect();
    let common = pc.iter().zip(&bc).take_while(|(x, y)| x == y).count();
    let mut out: Vec<String> = vec!["..".into(); bc.len() - common];
    out.extend(pc[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    out.join("/")
}
