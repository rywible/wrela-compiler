//! The compiler's driver: loads a package, then checks it (names, types, the memory model, the
//! GPU rules, and the per-instantiation effects found while lowering) or builds it.
//!
//! Each call compiles the package from its files; nothing is cached between calls. Checking the
//! examples takes tens of milliseconds, and the LSP (milestone 4) will decide what incremental
//! reuse has to look like.
//!
//! The compiler recurses over syntax trees, so it runs on its own thread with a stack sized for
//! the deepest tree the parser accepts (`wrela_syntax::MAX_NESTING`, `MAX_EXPR_DEPTH`).

pub mod build;
pub mod package;

use std::path::Path;
use std::rc::Rc;
use wrela_diag::{Diagnostic, FileId, SourceMap, Span, codes, sort_and_dedup};
use wrela_sema::{STD_SOURCES, SourceUnit};
use wrela_syntax::Parsed;

/// What compiling a package produced.
#[derive(Debug)]
pub struct Output {
    /// Every source file read: std's, then the package's.
    pub sources: SourceMap,
    /// Every diagnostic, sorted.
    pub diagnostics: Vec<Diagnostic>,
    /// The build's files by path relative to the output directory, in a fixed order; empty
    /// unless building found no errors.
    pub files: Vec<(String, Vec<u8>)>,
}

impl Output {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// The compiler thread's stack. Generous: it's reserved, not committed, and the deepest trees
/// the parser accepts need far less.
const STACK_SIZE: usize = 256 << 20;

/// Checks the package at `root`: every diagnostic, including those lowering finds.
pub fn check(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, false))
}

/// Builds the package at `root`: its diagnostics, and its files when there are no errors.
pub fn build(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, true))
}

fn on_compiler_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name("wrela".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(s, f)
            .expect("can't start the compiler's thread");
        worker.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn compile(root: &Path, emit: bool) -> Output {
    let (sources, mut diagnostics, units) = load(root);
    let Some(units) = units else {
        return Output { sources, diagnostics, files: Vec::new() };
    };
    let checked = wrela_sema::check_program(units, &mut diagnostics);
    let mut files = Vec::new();
    if !diagnostics.iter().any(|d| d.is_error()) {
        let roots = wrela_lower::Roots::of(&checked);
        let (lowered, d) = wrela_lower::lower(&checked, &roots);
        let ok = !d.iter().any(|x| x.is_error());
        diagnostics.extend(d);
        if ok && emit {
            let out = build::emit(&lowered);
            diagnostics.extend(out.diagnostics);
            files = out.files;
        }
    }
    sort_and_dedup(&mut diagnostics);
    if diagnostics.iter().any(|d| d.is_error()) {
        files.clear();
    }
    Output { sources, diagnostics, files }
}

/// Reads and parses std and the package. The units are `None` when the package can't be
/// loaded or a file has syntax errors that leave no usable tree.
fn load(root: &Path) -> (SourceMap, Vec<Diagnostic>, Option<Vec<SourceUnit>>) {
    let mut sources = SourceMap::new();
    let mut diags = Vec::new();
    let mut units = Vec::new();
    let mut parse_failed = false;
    for (i, (name, text)) in STD_SOURCES.iter().enumerate() {
        let file = FileId(i as u32);
        let parsed = wrela_syntax::parse(file, text);
        let id = sources.add(format!("<{name}>"), text.to_string());
        debug_assert_eq!(id, file);
        parse_failed |= syntax_failed(&parsed);
        diags.extend(parsed.diagnostics);
        units.push(SourceUnit {
            path: name.split("::").map(String::from).collect(),
            file,
            ast: Rc::new(parsed.file),
            is_std: true,
        });
    }
    let files = match package::find_files(root) {
        Ok(files) => files,
        Err(errors) => {
            for e in errors {
                let f = sources.add(e.path.clone(), "");
                let code = if e.symlink { codes::E0208 } else { codes::E0205 };
                let mut d = Diagnostic::new(code, Span::new(f, 0, 0), e.message);
                if let Some(h) = e.help {
                    d = d.with_help(h);
                }
                diags.push(d);
            }
            sort_and_dedup(&mut diags);
            return (sources, diags, None);
        }
    };
    for pf in &files {
        let text = std::fs::read(&pf.path).map_err(|e| e.to_string()).and_then(|b| {
            String::from_utf8(b).map_err(|_| "the file isn't valid UTF-8 (L1)".to_string())
        });
        let text = match text {
            Ok(t) => t,
            Err(why) => {
                let f = sources.add(pf.display.clone(), "");
                diags.push(Diagnostic::new(
                    codes::E0205,
                    Span::new(f, 0, 0),
                    format!("can't read `{}`: {why}", pf.display),
                ));
                continue;
            }
        };
        let file = FileId(sources.len() as u32);
        let parsed = wrela_syntax::parse(file, &text);
        let id = sources.add(pf.display.clone(), text);
        debug_assert_eq!(id, file);
        parse_failed |= syntax_failed(&parsed);
        diags.extend(parsed.diagnostics);
        units.push(SourceUnit {
            path: pf.module.clone(),
            file,
            ast: Rc::new(parsed.file),
            is_std: false,
        });
    }
    if parse_failed {
        sort_and_dedup(&mut diags);
        return (sources, diags, None);
    }
    (sources, diags, Some(units))
}

/// Whether a file's syntax errors make its AST unusable. Lexical errors (E00xx) leave a usable
/// token stream, so checking goes on and reports more; parse errors (E01xx) stop it.
fn syntax_failed(p: &Parsed) -> bool {
    p.diagnostics.iter().any(|d| d.is_error() && d.code.as_str().starts_with("E01"))
}
