//! Lifted builds (language.md §23): the table of literals a build lifts, with what tools need
//! to show and edit them, and the build's report of every float literal in the lifted packages,
//! lifted or not and why (`lift.json`).

use crate::live::Files;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    for (file, path) in files {
        let src = sources.file(*file);
        file_index.insert(*file, t.files.len() as u32);
        t.files.push(LiftedFile {
            id: *file,
            name: src.name.clone(),
            path: path.clone(),
            text: src.text.clone(),
            hash: file_hash(src.text.as_bytes()),
        });
    }
    for (file, start, end, value, negated) in cands {
        let fi = file_index[&file];
        let lc = sources.file(file).line_col(start);
        let i = t.literals.len() as u32;
        t.literals.push(LiftedLiteral {
            file: fi,
            start,
            end,
            line: lc.line,
            column: lc.column,
            unit: crate::edit::unit_of(&sources.file(file).text[start as usize..end as usize]),
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
                "in a constant the build computes (§13), once: it's data in the build"
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
    let lifted_ends: std::collections::HashSet<(u32, u32)> =
        t.literals.iter().map(|l| (l.file, l.end)).collect();
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
                "unit": l.unit,
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
            let negated_lifted = lifted_ends.contains(&(fi as u32, e));
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

// ---- a lifted build's literals in the files as they are now ---------------------------------------

/// A lifted build's report (`lift.json`) in the build's directory `dir`.
pub fn read_report(dir: &Path) -> Result<Report, String> {
    let text = std::fs::read_to_string(dir.join("lift.json"))
        .map_err(|e| format!("the build has no lift.json: {e}"))?;
    parse_report(&text)
}

/// A lifted build's report, from its text.
pub fn parse_report(text: &str) -> Result<Report, String> {
    serde_json::from_str(text).map_err(|e| format!("lift.json doesn't read: {e}"))
}

/// A file's text, lexed once for finding literals in it: its number tokens, in order, as byte
/// ranges (a minus before one, taken into a lifted literal, isn't a separate token here), and
/// what an edit of literals keeps of its tokens (its shape).
pub struct Numbers {
    text: Arc<str>,
    tokens: Vec<(u32, u32)>,
    /// Its tokens but its line breaks and its end, each number a `Float` with no text, and the
    /// minus signs before a number (an edit of a literal may add, drop or move one) left out.
    /// A minus before anything else is the code's.
    shape: Vec<(TokenKind, u32, u32)>,
}

impl Numbers {
    pub fn new(text: impl Into<Arc<str>>) -> Numbers {
        let text = text.into();
        let lexed: Vec<_> = crate::edit::lex(&text).collect();
        let mut tokens = Vec::new();
        let mut shape = Vec::with_capacity(lexed.len());
        let mut i = 0;
        while let Some(t) = lexed.get(i) {
            if t.kind == TokenKind::Minus {
                let start = i;
                while lexed.get(i).is_some_and(|t| t.kind == TokenKind::Minus) {
                    i += 1;
                }
                if !lexed.get(i).is_some_and(|t| t.kind.is_number()) {
                    for t in &lexed[start..i] {
                        shape.push((t.kind, t.span.start, t.span.end));
                    }
                }
                continue;
            }
            if t.kind.is_number() {
                tokens.push((t.span.start, t.span.end));
                shape.push((TokenKind::Float, 0, 0));
            } else {
                shape.push((t.kind, t.span.start, t.span.end));
            }
            i += 1;
        }
        Numbers { text, tokens, shape }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether `other` lexes to the same tokens but for numbers, and the minus signs before a
    /// number: what an edit of literals changes, and nothing else.
    pub(crate) fn same_but_numbers(&self, other: &Numbers) -> bool {
        self.shape.len() == other.shape.len()
            && self.shape.iter().zip(&other.shape).all(|(&(k, s, e), &(l, t, u))| {
                k == l && self.text[s as usize..e as usize] == other.text[t as usize..u as usize]
            })
    }

    /// The tokens outside the literal ranges; unlike `shape`, this keeps unlifted numbers
    /// and operators, so those changes require a new build.
    fn outside_literals(&self, mut ranges: Vec<(u32, u32)>) -> Vec<(TokenKind, &str)> {
        ranges.sort_unstable();
        let mut next = 0;
        crate::edit::lex(&self.text)
            .filter_map(|token| {
                while next < ranges.len() && ranges[next].1 <= token.span.start {
                    next += 1;
                }
                if ranges
                    .get(next)
                    .is_some_and(|&(s, e)| s <= token.span.start && token.span.end <= e)
                {
                    return None;
                }
                Some((token.kind, &self.text[token.span.range()]))
            })
            .collect()
    }
}

/// Where literal `start..end` of the build's text (`then`) is in the file's text `now`: the
/// same number token by its position among the file's numbers, which an edit of literals keeps.
/// A literal's place takes the unary minus before its number now if it started at its minus
/// (an edit may have dropped it since), or if an edit has put one there since (the build's
/// text had none). `None` if the token isn't there.
pub fn moved(then: &Numbers, now: &Numbers, start: u32, end: u32) -> Option<(u32, u32)> {
    use crate::edit::minus_before;
    let (old, new) = (then.text.as_bytes(), now.text.as_bytes());
    let minus = old.get(start as usize) == Some(&b'-');
    let number_start =
        if minus { crate::edit::blanks_after(old, start as usize + 1) as u32 } else { start };
    let k = then.tokens.binary_search_by_key(&number_start, |t| t.0).ok()?;
    if then.tokens[k].1 != end {
        return None;
    }
    let (s, e) = *now.tokens.get(k)?;
    let had = minus || minus_before(old, start as usize).is_some();
    match minus_before(new, s as usize) {
        Some(m) if minus || !had => Some((m as u32, e)),
        _ => Some((s, e)),
    }
}

/// Each file of a lifted build's report `r` (its paths relative to `base`) as a watcher found
/// it among `files`, with its text when built, lexed: `None` if one isn't among them.
pub(crate) fn lifted_files(
    base: &Path,
    r: &Report,
    files: &Files,
) -> Option<Vec<(PathBuf, Numbers)>> {
    // The files by their canonical paths, as the report names them from `base`.
    let canonical: BTreeMap<PathBuf, &PathBuf> =
        files.keys().filter_map(|p| Some((p.canonicalize().ok()?, p))).collect();
    r.files
        .iter()
        .map(|f| {
            let path = canonical.get(&base.join(&f.path).canonicalize().ok()?)?;
            Some((path.to_path_buf(), Numbers::new(f.text.as_str())))
        })
        .collect()
}

/// The build's literals' values (`literals`) in the files as they are (`now`), if what changed
/// since the build is literals alone: each file it lifted (`lifted`, from [`lifted_files`]) has
/// the build's tokens but for numbers, and every other file is as the build saw it (`built`).
pub(crate) fn literal_values(
    lifted: &[(PathBuf, Numbers)],
    literals: &[ReportLiteral],
    now: &Files,
    built: &Files,
) -> Option<Vec<f32>> {
    // A file added or deleted since the build changes the program, lifted or not.
    let other = |p: &PathBuf| !lifted.iter().any(|(l, _)| l == p);
    if !now.keys().eq(built.keys()) || now.iter().any(|(p, s)| other(p) && !s.same_text(&built[p]))
    {
        return None;
    }
    let then: Vec<&Numbers> = lifted.iter().map(|(_, n)| n).collect();
    let current = lifted.iter().map(|(p, _)| Numbers::new(Arc::clone(&now[p].text))).collect();
    values_in(&then, current, literals)
}

/// The values of a lifted build's literals (`literals`) in its lifted files as they are now,
/// each read by `read` by its path in the report (relative to the program's package): `None`
/// if a file has changed past its literals since the build, which then needs a new build. This
/// looks at the lifted files alone; a tool that watches other files compares those itself.
pub fn values_now(
    report: &Report,
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Option<Vec<f32>>, String> {
    let then: Vec<Numbers> = report.files.iter().map(|f| Numbers::new(f.text.as_str())).collect();
    let mut now = Vec::with_capacity(report.files.len());
    for f in &report.files {
        now.push(Numbers::new(read(&f.path)?));
    }
    Ok(values_in(&then.iter().collect::<Vec<_>>(), now, &report.literals))
}

/// The literals' values in the lifted files' texts `now`, each file then (`then`) as the build
/// saw it, if each has changed in its literals alone.
fn values_in(then: &[&Numbers], now: Vec<Numbers>, literals: &[ReportLiteral]) -> Option<Vec<f32>> {
    if then.len() != now.len() {
        return None;
    }
    for (file, (then, current)) in then.iter().zip(&now).enumerate() {
        if !then.same_but_numbers(current) {
            return None;
        }
        let mut before_ranges = Vec::new();
        let mut now_ranges = Vec::new();
        for l in literals.iter().filter(|l| l.file as usize == file) {
            let (start, end) = moved(then, current, l.start, l.end)?;
            // `moved` already includes the literal's own sign. Another minus before it is
            // an operator in the code and must remain unchanged for a table-only update.
            before_ranges.push((l.start, l.end));
            now_ranges.push((start, end));
        }
        if then.outside_literals(before_ranges) != current.outside_literals(now_ranges) {
            return None;
        }
    }
    let mut values = Vec::with_capacity(literals.len());
    for l in literals {
        let current = now.get(l.file as usize)?;
        let (s, e) = moved(then[l.file as usize], current, l.start, l.end)?;
        let value = crate::edit::signed_value(&current.text()[s as usize..e as usize])?;
        if !value.is_finite() {
            return None;
        }
        values.push(value);
    }
    Some(values)
}

/// Edits of a lifted build's literals, each by its index in the build (`report`) and its new
/// value, planned against the files as they are now, each read by `read` by its path in the
/// report: what `wrela edit` would write (nothing is written here).
pub fn plan_by_index(
    report: &Report,
    read: &dyn Fn(&str) -> Result<String, String>,
    edits: &[(u32, f32)],
) -> Result<Vec<crate::edit::FileEdit>, crate::edit::Refused> {
    let refused = |why: String| crate::edit::Refused { file: String::new(), why };
    let changed = |file: &str| crate::edit::Refused {
        file: file.to_string(),
        why: "it has changed past its literals since it was built: rebuild".into(),
    };
    // Each file an edit is in, by its index in the build: its text then and now, each lexed once.
    let mut lexed: BTreeMap<u32, (Numbers, Numbers)> = BTreeMap::new();
    let mut out = Vec::new();
    for &(i, value) in edits {
        let Some(l) = report.literals.get(i as usize) else {
            return Err(refused(format!("the build has no literal {i}")));
        };
        let Some(f) = report.files.get(l.file as usize) else {
            return Err(refused(format!("the build's literal {i} has no file")));
        };
        let rel = &f.path;
        let (then, now) = match lexed.entry(l.file) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                let now = read(rel).map_err(|e| crate::edit::Refused {
                    file: rel.clone(),
                    why: format!("can't read it: {e}"),
                })?;
                let (then, now) = (Numbers::new(f.text.as_str()), Numbers::new(now));
                // A literal is found by its place among the file's numbers, which only an edit
                // of literals keeps: any other change since the build and it could be another.
                if !then.same_but_numbers(&now) {
                    return Err(changed(rel));
                }
                v.insert((then, now))
            }
        };
        let Some((s, e)) = moved(then, now, l.start, l.end) else { return Err(changed(rel)) };
        // The value is the text's, minus and all: what the code reads (as the table holds it).
        out.push(crate::edit::LiteralEdit {
            file: rel.clone(),
            start: s,
            end: e,
            text: now.text[s as usize..e as usize].to_string(),
            hash: file_hash(now.text.as_bytes()),
            value,
        });
    }
    crate::edit::plan_with(&|file| read(file).map_err(|e| format!("can't read it: {e}")), &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::Files;

    #[test]
    fn a_file_without_lifted_candidates_still_reports_its_float_literals() {
        let root = std::env::temp_dir().join(format!("wrela-lift-unlifted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("package directory");
        std::fs::write(
            root.join("main.wrela"),
            "pub fn frame(time: f32, width: u32, height: u32) {}\n",
        )
        .expect("program");
        std::fs::write(root.join("wide.wrela"), "pub fn value() -> f64 { 0.125 }\n")
            .expect("non-lifted function");
        crate::with_checked(&root, |checked, sources| {
            let files: Vec<_> = sources
                .files()
                .filter(|(_, f)| !f.name.starts_with('<'))
                .map(|(id, f)| (id, f.name.clone()))
                .collect();
            let table = table(checked, sources, &files);
            let report: serde_json::Value =
                serde_json::from_str(&report(checked, sources, &table)).expect("report");
            assert!(table.literals.is_empty());
            assert_eq!(report["not_lifted"].as_array().expect("not lifted").len(), 1);
            assert_eq!(report["not_lifted"][0]["text"], "0.125");
        })
        .expect("checked");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn overflowing_literals_require_a_rebuild() {
        let dir = std::env::temp_dir().join(format!("wrela-lift-overflow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let text =
            "pub fn value() -> f32 { 0.5 }\npub fn frame(time: f32, width: u32, height: u32) {}\n";
        let file = dir.join("main.wrela");
        std::fs::write(&file, text).expect("source");
        assert!(!crate::check(&dir).has_errors(), "the initial program checks");
        let built = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
        let lifted = vec![(file.clone(), Numbers::new(text))];
        let start = text.find("0.5").expect("literal") as u32;
        let literals = vec![ReportLiteral { file: 0, start, end: start + 3, value: 0.5 }];
        for value in ["1e99", "-1e99", "1e99m", "16777217", "18446744073709551615"] {
            std::fs::write(&file, text.replace("0.5", value)).expect("changed source");
            assert!(
                crate::check(&dir).diagnostics.iter().any(|d| d.code == wrela_diag::codes::E0006),
                "the compiler rejects {value} as unrepresentable"
            );
            let now = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
            assert!(
                literal_values(&lifted, &literals, &now, &built).is_none(),
                "{value} must rebuild to report its compiler error"
            );
        }
        for value in ["16777216", "16777217.0"] {
            std::fs::write(&file, text.replace("0.5", value)).expect("valid changed source");
            assert!(!crate::check(&dir).has_errors(), "the compiler accepts {value}");
            let now = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
            assert_eq!(literal_values(&lifted, &literals, &now, &built), Some(vec![16777216.0]));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unlifted_numbers_and_binary_minus_changes_require_a_rebuild() {
        let dir = std::env::temp_dir().join(format!("wrela-lift-rebuild-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let text = "fn f() { let n: u32 = 2; let x = n - 0.5 }\n";
        let file = dir.join("main.wrela");
        std::fs::write(&file, text).expect("source");
        let built = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
        let lifted = vec![(file.clone(), Numbers::new(text))];
        let start = text.find("0.5").expect("lifted literal") as u32;
        let literals = vec![ReportLiteral { file: 0, start, end: start + 3, value: 0.5 }];
        for changed in [text.replace("= 2", "= 3"), text.replace("n - 0.5", "n 0.5")] {
            std::fs::write(&file, changed).expect("changed source");
            let now = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
            assert!(
                literal_values(&lifted, &literals, &now, &built).is_none(),
                "a non-lifted change cannot update only the table"
            );
        }
        std::fs::write(&file, text.replace("0.5", "-0.25")).expect("lifted edit");
        let now = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
        assert_eq!(literal_values(&lifted, &literals, &now, &built), Some(vec![-0.25]));
        std::fs::write(&file, text.replace("0.5", "--0.25")).expect("two new negations");
        let now = crate::live::scan(std::slice::from_ref(&dir), &Files::new());
        assert!(
            literal_values(&lifted, &literals, &now, &built).is_none(),
            "an extra unary operator cannot be lost from a literal-only update"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn same_but_numbers(a: &str, b: &str) -> bool {
        Numbers::new(a).same_but_numbers(&Numbers::new(b))
    }

    #[test]
    fn only_numbers_and_the_minus_signs_before_them_may_change() {
        // What an edit of literals changes: a number, a minus moved with one, a second minus.
        assert!(same_but_numbers("let a = x - 0.5 * y", "let a = x - 0.25 * y"));
        assert!(same_but_numbers("f(-0.5, 2.0)", "f(0.2, 2.5)"));
        assert!(same_but_numbers("x - 0.5", "x - -0.2"));
        // What only the code's changes do: a minus before a name, an operator, a new number.
        assert!(!same_but_numbers("a - b", "-a - b"));
        assert!(!same_but_numbers("x * y", "x * -y"));
        assert!(!same_but_numbers("a - 0.5", "a + 0.5"));
        assert!(!same_but_numbers("f(1.0)", "f(1.0, 2.0)"));
    }

    #[test]
    fn perf_chains_of_minus_signs_keep_their_numbers_and_code_in_order() {
        let chain = "-".repeat(4096);
        assert!(same_but_numbers(&format!("a {chain} 1.0 + 2.0"), "a 3.0 + -4.0"));
        assert!(!same_but_numbers(&format!("a {chain} b + 2.0"), "a -b + 2.0"));
        let numbers = Numbers::new("1.0 - -2.0 + 3.0");
        assert_eq!(numbers.tokens, [(0, 3), (7, 10), (13, 16)]);
    }
}
