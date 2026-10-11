//! `wrela fix <package-dir> [--json]`: makes every fix a tool can make, and reports each one.
//!
//! A diagnostic with exactly one fix gets it, unless the fix is the author's choice: the others
//! are what the compiler would do. One with several fixes offers choices (`borrow`, `.clone()`
//! or `take`), and so does a fix that changes what the program owns or costs (`.clone()` copies
//! heap memory, and an index, `remove` or a parameter that borrows may be what was meant): only
//! the author can make those, so they're listed and left. Fixes go in rounds, since one fix can
//! let checking reach code it didn't before: each round checks the package and makes the fixes
//! it can, and the rounds stop when one makes none. Then each changed file is formatted, and
//! the report comes from a check of the files as they are now, so every place it names is in
//! them. Only the package's own files change, and running it again changes nothing.
//!
//! A diagnostic's one fix isn't made when it edits std's files or a dependency's (reported:
//! that package's authors fix it), when it replaces code that has another error with no fix of
//! its own (reported: the fix was made from code with an error, such as tokens the parser
//! skipped), when its edits overlap each other or change nothing (reported: a compiler bug),
//! or when its edits overlap those of a fix made earlier in the round (the next round has it).
//!
//! With `--json`, the report is the diagnostics' JSON (wrela_diag::json) with two keys more:
//! `"fixed"`, each fix made, as `{"code", "message", "file", "line", "column"}` (where its
//! diagnostic was, in the file as it is now), and `"changed"`, the names of the files changed.
//!
//! Exit status: 0 when the package has no errors after, 1 when it has (or the fixes didn't
//! settle, an internal error), 2 when a file can't be written.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use wrela_diag::{Diagnostic, Edit, FileId, Fix, SourceMap};
use wrela_driver::Output;

/// At most this many rounds: a fix removes its diagnostic, so more would mean one doesn't.
const ROUNDS: usize = 16;

/// On a thread with the compiler's stack, for the formatter (see `fmt::run`).
pub fn run(args: &[String]) -> ExitCode {
    wrela_driver::on_compiler_thread(|| fix(args))
}

/// A fix made: its diagnostic's code, its message, and where it is in its file's text as it
/// is now (each later edit and the formatting move it).
struct Made {
    path: PathBuf,
    at: u32,
    code: &'static str,
    message: String,
}

/// Why a diagnostic's one fix isn't made.
enum Skip<'a> {
    /// It edits this file, which is std's or a dependency's.
    Elsewhere(FileId),
    /// It replaces code that has this error, which has no fix.
    Covers(&'a Diagnostic),
    /// Its edits overlap each other, are out of the text, or change nothing.
    Broken,
    /// Its edits overlap those of a fix the round makes already.
    Clash,
}

fn fix(args: &[String]) -> ExitCode {
    let args = match crate::package_args(args, false) {
        Ok(a) => a,
        Err(status) => return status,
    };
    // Each file changed, with its text now.
    let mut changed: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut made: Vec<Made> = Vec::new();
    // The check of the files as they are, once a round makes no fix.
    let mut last = None;
    let mut write_failed = false;
    for _ in 0..ROUNDS {
        let out = wrela_driver::check(&args.dir);
        let paths: BTreeMap<FileId, PathBuf> = out.program_files.iter().cloned().collect();
        let fixes = plan(&out, &paths).make;
        if fixes.is_empty() {
            last = Some(out);
            break;
        }
        let mut edits: BTreeMap<FileId, Vec<&Edit>> = BTreeMap::new();
        let mut round = Vec::new();
        for (d, fix) in fixes {
            for e in &fix.edits {
                edits.entry(e.span.file).or_default().push(e);
            }
            // Where the diagnostic is, or failing that (it's in another file), the fix.
            let at = d.span().filter(|s| paths.contains_key(&s.file)).unwrap_or(fix.edits[0].span);
            let path = paths[&at.file].clone();
            round.push(Made {
                path,
                at: at.start,
                code: d.code.as_str(),
                message: fix.message.clone(),
            });
        }
        for (file, mut es) in edits {
            let path = &paths[&file];
            es.sort_by_key(|e| (e.span.start, e.span.end));
            let next = wrela_diag::apply_edits(&out.sources.file(file).text, es.iter().copied());
            if let Err(e) = std::fs::write(path, &next) {
                eprintln!("error: can't write `{}`: {e}", path.display());
                write_failed = true;
                // This round's fixes in it aren't made; the earlier rounds' still are.
                round.retain(|m| &m.path != path);
                continue;
            }
            for m in made.iter_mut().chain(&mut round).filter(|m| &m.path == path) {
                m.at = after_edits(&es, m.at);
            }
            changed.insert(path.clone(), next);
        }
        made.extend(round);
        if write_failed {
            break;
        }
    }
    let settled = last.is_some() || write_failed;
    // The changed files, formatted (one that doesn't parse is left as the fixes made it).
    for (path, text) in &mut changed {
        let Some(formatted) = crate::fmt::format_text(text).filter(|f| f != text) else {
            continue;
        };
        if let Err(e) = std::fs::write(path, &formatted) {
            eprintln!("error: can't write `{}`: {e}", path.display());
            write_failed = true;
            continue;
        }
        let mut at: Vec<u32> = made.iter().filter(|m| &m.path == path).map(|m| m.at).collect();
        wrela_syntax::fmt::move_offsets(text, &formatted, &mut at);
        for (m, a) in made.iter_mut().filter(|m| &m.path == path).zip(at) {
            m.at = a;
        }
        *text = formatted;
        last = None;
    }
    let mut out = last.unwrap_or_else(|| wrela_driver::check(&args.dir));
    if !settled {
        out.diagnostics.push(Diagnostic::internal(format!(
            "the fixes didn't settle in {ROUNDS} rounds: a fix doesn't remove its diagnostic, so \
             running `wrela fix` again changes the files again"
        )));
    }
    report(&out, &made, &changed, args.json);
    if write_failed {
        ExitCode::from(2)
    } else if out.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// What a round does with the diagnostics that have one fix.
struct Plan<'a> {
    /// The fixes it makes, each with its diagnostic.
    make: Vec<(&'a Diagnostic, &'a Fix)>,
    /// The diagnostics whose fix it doesn't make, with why.
    skip: Vec<(&'a Diagnostic, Skip<'a>)>,
}

fn plan<'a>(out: &'a Output, paths: &BTreeMap<FileId, PathBuf>) -> Plan<'a> {
    let mut taken: BTreeMap<FileId, Vec<&Edit>> = BTreeMap::new();
    let mut plan = Plan { make: Vec::new(), skip: Vec::new() };
    for d in &out.diagnostics {
        let [fix] = d.fixes.as_slice() else { continue };
        if fix.choice {
            continue;
        }
        let skip =
            if let Some(e) = fix.edits.iter().find(|e| !paths.contains_key(&e.span.file)) {
                Some(Skip::Elsewhere(e.span.file))
            } else if broken(fix, &out.sources) {
                Some(Skip::Broken)
            } else if let Some(o) = covered(&out.diagnostics, d, fix) {
                Some(Skip::Covers(o))
            } else if fix.edits.iter().any(|e| {
                taken.get(&e.span.file).is_some_and(|es| es.iter().any(|o| overlaps(o, e)))
            }) {
                Some(Skip::Clash)
            } else {
                None
            };
        match skip {
            Some(s) => plan.skip.push((d, s)),
            None => {
                for e in &fix.edits {
                    taken.entry(e.span.file).or_default().push(e);
                }
                plan.make.push((d, fix));
            }
        }
    }
    plan
}

/// Whether `apply_edits` can't make the fix, or making it would change nothing: no edits,
/// an edit out of its file's text or inside a character, edits that overlap, or edits that
/// each give back the text they replace.
fn broken(fix: &Fix, sources: &SourceMap) -> bool {
    let outside = fix.edits.iter().any(|e| {
        let text = &sources.file(e.span.file).text;
        let (s, t) = (e.span.start as usize, e.span.end as usize);
        t > text.len() || !text.is_char_boundary(s) || !text.is_char_boundary(t)
    });
    if fix.edits.is_empty() || outside {
        return true;
    }
    let mut spans: Vec<_> = fix.edits.iter().map(|e| e.span).collect();
    spans.sort_by_key(|s| (s.file, s.start, s.end));
    let overlap = spans.windows(2).any(|w| w[0].file == w[1].file && w[1].start < w[0].end);
    let no_change = fix.edits.iter().all(|e| {
        let text = &sources.file(e.span.file).text;
        text[e.span.start as usize..e.span.end as usize] == e.replacement
    });
    overlap || no_change
}

/// Another error that has no fix, in code the fix replaces: the fix was made from code with an
/// error, such as tokens the parser skipped to recover.
fn covered<'a>(all: &'a [Diagnostic], d: &Diagnostic, fix: &Fix) -> Option<&'a Diagnostic> {
    all.iter().find(|o| {
        !std::ptr::eq(*o, d)
            && o.is_error()
            && o.fixes.is_empty()
            && o.span().is_some_and(|s| {
                fix.edits
                    .iter()
                    .any(|e| e.span.file == s.file && e.span.start < s.end && s.start < e.span.end)
            })
    })
}

fn overlaps(a: &Edit, b: &Edit) -> bool {
    // Two insertions at one place overlap too: their order would be a guess.
    a.span.start < b.span.end && b.span.start < a.span.end || a.span.start == b.span.start
}

/// Where offset `at` of a text is once `edits` (sorted, none overlapping) are made: in text an
/// edit replaces, at the start of the replacement; after an insertion, after it.
fn after_edits(edits: &[&Edit], at: u32) -> u32 {
    let mut shift = 0i64;
    for e in edits {
        let (s, t) = (e.span.start, e.span.end);
        if at < s {
            break;
        }
        if at < t {
            return (i64::from(s) + shift) as u32;
        }
        shift += e.replacement.len() as i64 - i64::from(t - s);
    }
    (i64::from(at) + shift) as u32
}

/// Prints what was fixed, what's left to choose, the fixes not made, and the diagnostics left:
/// for people, or as JSON.
fn report(out: &Output, made: &[Made], changed: &BTreeMap<PathBuf, String>, json: bool) {
    let paths: BTreeMap<FileId, PathBuf> = out.program_files.iter().cloned().collect();
    let ids: BTreeMap<&PathBuf, FileId> = paths.iter().map(|(id, p)| (p, *id)).collect();
    let name = |path: &PathBuf| match ids.get(path) {
        Some(&id) => out.sources.file(id).name.clone(),
        None => path.display().to_string(),
    };
    let mut made: Vec<&Made> = made.iter().collect();
    made.sort_by(|a, b| (&a.path, a.at).cmp(&(&b.path, b.at)));
    if json {
        let fixed: Vec<serde_json::Value> = made
            .iter()
            .map(|m| {
                let (line, column) = match ids.get(&m.path) {
                    Some(&id) => {
                        let lc = out.sources.file(id).line_col(m.at);
                        (lc.line, lc.column)
                    }
                    None => (0, 0),
                };
                serde_json::json!({
                    "code": m.code, "message": m.message,
                    "file": name(&m.path), "line": line, "column": column,
                })
            })
            .collect();
        let mut v = wrela_diag::json::to_json(&out.sources, &out.diagnostics);
        v["fixed"] = fixed.into();
        v["changed"] = changed.keys().map(name).collect::<Vec<_>>().into();
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return;
    }
    for m in &made {
        let at = match ids.get(&m.path) {
            Some(&id) => out.sources.file(id).location(m.at),
            None => m.path.display().to_string(),
        };
        println!("{at}: fixed {}: {}", m.code, m.message);
    }
    for d in out.diagnostics.iter() {
        match d.fixes.as_slice() {
            [f] if f.choice => println!(
                "{}: {}'s fix is yours to choose, since it changes what the program owns or costs:",
                where_(out, d),
                d.code
            ),
            [] | [_] => continue,
            _ => println!("{}: {} has fixes to choose from:", where_(out, d), d.code),
        }
        for f in &d.fixes {
            println!("  - {}", f.message);
        }
    }
    let skipped = plan(out, &paths).skip;
    let mut unmade = 0;
    for (d, skip) in &skipped {
        let why = match skip {
            Skip::Elsewhere(f) => {
                format!("it edits `{}`, which isn't this package's", out.sources.file(*f).name)
            }
            Skip::Covers(o) => {
                format!("it replaces code with another error ({} at {})", o.code, where_(out, o))
            }
            Skip::Broken => "its edits overlap or change nothing, which is a compiler bug".into(),
            Skip::Clash => continue,
        };
        println!("{}: {}'s fix wasn't made: {why}", where_(out, d), d.code);
        unmade += 1;
    }
    let errors = out.diagnostics.iter().filter(|d| d.is_error()).count();
    let head = match (made.len(), unmade, errors) {
        (0, 0, 0) => "nothing to fix".to_string(),
        (0, 0, _) => "nothing a tool can fix".to_string(),
        (0, _, _) => "no fix made".to_string(),
        (n, _, _) => {
            let files = crate::count(changed.len(), "file", "files");
            format!("{} in {files}", crate::count(n, "fix", "fixes"))
        }
    };
    if errors > 0 {
        println!("{head}; {} left", crate::count(errors, "error", "errors"));
    } else if made.is_empty() {
        println!("{head}");
    } else {
        println!("{head}; the package has no errors");
    }
    eprint!("{}", wrela_diag::render::render_all(&out.sources, &out.diagnostics));
}

/// `file:line:column` of a diagnostic's primary span.
fn where_(out: &Output, d: &Diagnostic) -> String {
    match d.span() {
        Some(s) => out.sources.file(s.file).location(s.start),
        None => "(no location)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wrela_diag::{Span, codes};

    fn edit(file: FileId, start: u32, end: u32, replacement: &str) -> Edit {
        Edit { span: Span::new(file, start, end), replacement: replacement.into() }
    }

    fn fix(edits: Vec<Edit>) -> Fix {
        Fix { message: "fix it".into(), edits, choice: false }
    }

    /// A fix that `apply_edits` would panic on, or that would change nothing in every round,
    /// is broken: it isn't made.
    #[test]
    fn broken_fixes_are_found() {
        let mut map = SourceMap::new();
        let f = map.add("main.wrela", "let x = 1\n");
        assert!(!broken(&fix(vec![edit(f, 4, 5, "y"), edit(f, 8, 9, "2")]), &map));
        assert!(broken(&fix(vec![]), &map));
        assert!(broken(&fix(vec![edit(f, 4, 5, "x")]), &map), "a change to nothing");
        assert!(broken(&fix(vec![edit(f, 0, 6, "a"), edit(f, 4, 9, "b")]), &map), "overlap");
        assert!(broken(&fix(vec![edit(f, 8, 40, "2")]), &map), "past the end");
    }

    /// Text an edit replaces goes to the start of its replacement; text after an insertion or
    /// a replacement moves by the change in length.
    #[test]
    fn places_move_with_edits() {
        let f = FileId(0);
        let (a, b) = (edit(f, 0, 0, "use x\n"), edit(f, 10, 14, "y"));
        let edits = [&a, &b];
        assert_eq!(after_edits(&edits, 0), 6);
        assert_eq!(after_edits(&edits, 5), 11);
        assert_eq!(after_edits(&edits, 12), 16);
        assert_eq!(after_edits(&edits, 14), 17);
    }

    /// A fix that replaces another error with no fix of its own was made from code with an
    /// error: it waits. One beside it, or an insertion at the error's start, doesn't.
    #[test]
    fn fixes_over_other_errors_wait() {
        let f = FileId(0);
        let other = Diagnostic::new(codes::E0100, Span::new(f, 10, 11), "expected `,`");
        let d = Diagnostic::new(codes::E0115, Span::new(f, 4, 8), "a macro");
        let all = [d.clone(), other];
        assert!(covered(&all, &all[0], &fix(vec![edit(f, 4, 14, "v")])).is_some());
        assert!(covered(&all, &all[0], &fix(vec![edit(f, 4, 10, "v")])).is_none());
        assert!(covered(&all, &all[0], &fix(vec![edit(f, 10, 10, "v")])).is_none());
    }
}
