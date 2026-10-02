//! The compiler's driver: loads a package, then checks and builds it through queries.

pub mod package;
pub mod query;

use package::{LayoutError, PackageFile};
use query::{Db, Input, Query};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use wrela_diag::{Diagnostic, FileId, SourceMap, Span, codes, sort_and_dedup};
use wrela_sema::{Checked, STD_SOURCES, SourceUnit};
use wrela_syntax::Parsed;

/// A value shared between queries; equal only to itself (so no early cutoff through it).
#[derive(Debug)]
pub struct Shared<T>(pub Rc<T>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<T> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Input: the package's files, or what's wrong with its layout.
pub struct PackageLayout;
impl Input for PackageLayout {
    type Key = ();
    type Value = Result<Vec<PackageFile>, Vec<LayoutError>>;
    const NAME: &'static str = "package_layout";
}

/// Input: a file's text, or why it couldn't be read.
pub struct FileText;
impl Input for FileText {
    type Key = PathBuf;
    type Value = Result<Rc<str>, String>;
    const NAME: &'static str = "file_text";
}

/// A source file the compiler reads: a std module or one of the package's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SourceKey {
    Std(usize),
    Package(PathBuf),
}

/// Query: one file, parsed. Its `FileId` is part of the key, so spans are stable.
pub struct ParseFile;
impl Query for ParseFile {
    type Key = (FileId, SourceKey);
    type Value = Shared<(Rc<str>, Parsed)>;
    const NAME: &'static str = "parse_file";
    fn compute(db: &Db, (file, key): &Self::Key) -> Self::Value {
        let text: Rc<str> = match key {
            SourceKey::Std(i) => STD_SOURCES[*i].1.into(),
            SourceKey::Package(p) => db.input::<FileText>(p).unwrap_or_else(|_| "".into()),
        };
        let parsed = wrela_syntax::parse(*file, &text);
        Shared(Rc::new((text, parsed)))
    }
}

/// The result of checking a package.
#[derive(Debug)]
pub struct CheckOutput {
    pub sources: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    /// `None` when the package couldn't be loaded or parsed.
    pub checked: Option<Checked>,
}

impl CheckOutput {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// Query: the whole package, checked (names, types, the memory model, GPU rules).
pub struct CheckPackage;
impl Query for CheckPackage {
    type Key = ();
    type Value = Shared<CheckOutput>;
    const NAME: &'static str = "check_package";
    fn compute(db: &Db, _: &()) -> Self::Value {
        Shared(Rc::new(check_package(db)))
    }
}

fn check_package(db: &Db) -> CheckOutput {
    let mut sources = SourceMap::new();
    let mut diags = Vec::new();
    let mut units = Vec::new();
    let mut parse_failed = false;
    for (i, (name, _)) in STD_SOURCES.iter().enumerate() {
        let file = FileId(i as u32);
        let parsed = db.get::<ParseFile>(&(file, SourceKey::Std(i)));
        let id = sources.add(format!("<{name}>"), parsed.0.0.to_string());
        debug_assert_eq!(id, file);
        diags.extend(parsed.0.1.diagnostics.iter().cloned());
        parse_failed |= syntax_failed(&parsed.0.1);
        units.push(SourceUnit {
            path: name.split("::").map(String::from).collect(),
            file,
            ast: Rc::new(parsed.0.1.file.clone()),
            is_std: true,
        });
    }
    let files = match db.input::<PackageLayout>(&()) {
        Ok(files) => files,
        Err(errors) => {
            for e in errors {
                let f = sources.add(e.path.clone(), "");
                let mut d = Diagnostic::new(codes::E0205, Span::new(f, 0, 0), e.message);
                if let Some(h) = e.help {
                    d = d.with_help(h);
                }
                diags.push(d);
            }
            return CheckOutput { sources, diagnostics: diags, checked: None };
        }
    };
    for pf in &files {
        let file = FileId(sources.len() as u32);
        match db.input::<FileText>(&pf.path) {
            Ok(_) => {}
            Err(why) => {
                let f = sources.add(pf.display.clone(), "");
                diags.push(Diagnostic::new(
                    codes::E0205,
                    Span::new(f, 0, 0),
                    format!("can't read `{}`: {why}", pf.display),
                ));
                continue;
            }
        }
        let parsed = db.get::<ParseFile>(&(file, SourceKey::Package(pf.path.clone())));
        let id = sources.add(pf.display.clone(), parsed.0.0.to_string());
        debug_assert_eq!(id, file);
        diags.extend(parsed.0.1.diagnostics.iter().cloned());
        parse_failed |= syntax_failed(&parsed.0.1);
        units.push(SourceUnit {
            path: pf.module.clone(),
            file,
            ast: Rc::new(parsed.0.1.file.clone()),
            is_std: false,
        });
    }
    if parse_failed {
        sort_and_dedup(&mut diags);
        return CheckOutput { sources, diagnostics: diags, checked: None };
    }
    let checked = wrela_sema::check_program(units, &mut diags);
    sort_and_dedup(&mut diags);
    CheckOutput { sources, diagnostics: diags, checked: Some(checked) }
}

/// Whether a file's syntax errors make its AST unusable. Lexical errors (E00xx) leave a usable
/// token stream, so checking goes on and reports more; parse errors (E01xx) stop it.
fn syntax_failed(p: &Parsed) -> bool {
    p.diagnostics.iter().any(|d| d.is_error() && d.code.as_str().starts_with("E01"))
}

/// A compiler session for one package.
pub struct Compiler {
    pub db: Db,
    pub root: PathBuf,
}

impl Compiler {
    /// Reads the package at `root` into a new session.
    pub fn new(root: &Path) -> Compiler {
        let c = Compiler { db: Db::new(), root: root.to_path_buf() };
        c.reload();
        c
    }

    /// Re-reads the package's layout and files; unchanged files don't invalidate anything.
    pub fn reload(&self) {
        let layout = package::find_files(&self.root);
        if let Ok(files) = &layout {
            for f in files {
                let text = std::fs::read(&f.path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| {
                        String::from_utf8(b)
                            .map_err(|_| "the file isn't valid UTF-8 (L1)".to_string())
                    })
                    .map(|s| Rc::from(s.as_str()));
                self.db.set_input::<FileText>(f.path.clone(), text);
            }
        }
        self.db.set_input::<PackageLayout>((), layout);
    }

    pub fn check(&self) -> Shared<CheckOutput> {
        self.db.get::<CheckPackage>(&())
    }
}
