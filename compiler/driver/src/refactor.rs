//! `wrela refactor` (#39, the owner's additions): changes to a program's code that keep what it
//! means, planned as text edits, checked before anything is written, and refused when a file
//! has changed since the plan was made.
//!
//! - **rename `<item>` `<new-name>`**: a function, a method, a trait's method (with its impls'),
//!   a struct, an enum or a trait of the program's own package. The references are what the
//!   compiler resolves to the item: the definition is renamed, and each error that leaves (the
//!   old name, not found where it named the item) is a reference, renamed in turn, until the
//!   program checks. A function's calls are counted in the compiler's call graph before and
//!   after, so a call that would quietly go to another function is refused, and so is a name
//!   the item's module (or its type's methods) already has.
//! - **move `<item>` `<module>`**: a top-level item (a struct or an enum with its impls in its
//!   file) to another module's file. Each module that names it imports it, the moved code
//!   imports what it named from its old module, an import only it needed goes, and a private
//!   item another module now names becomes `pub(package)`.
//! - **add-param `<fn>` `"<name>: <type>"`** with `--value <expr>` (each call passes it, by name)
//!   or `--default <expr>` (the calls don't change).
//! - **change-mode `<fn>` `<param>` `<borrow|mut|take>`**: the parameter's mode, and each call's
//!   argument marked to match (`mut x`; `take x` where it passed a place of a type that isn't
//!   `Copy`).
//!
//! - **extract-const**, **inline** and **extract-fn** (`extract.rs`): an expression of
//!   literals and constants made a constant; a one-expression function's calls made its body;
//!   whole lines of statements made a function and a call of it.
//!
//! A plan is each file's hash before, its new text and its hunks. It's checked with the new
//! texts in place of the old: if the program wouldn't check, nothing is written and the errors
//! say why. A file the formatter owned stays formatted. A plan made earlier (`apply`) is refused
//! if a file it changes no longer has the hash it had.

use crate::query::{Index, Item};
use crate::{Overlay, SourceMap};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wrela_diag::{Diagnostic, SourceFile, Span};
use wrela_sema::Checked;
use wrela_sema::defs::{FnOwner, Lang, Mode, PackageKind, Res};
use wrela_sema::mir::{Arg, Callee, Rvalue};

/// How many times the program is checked to find what a change leaves to do.
const ROUNDS: usize = 12;

/// One file a plan changes.
#[derive(Clone, Debug)]
pub struct FileChange {
    /// Relative to the package.
    pub file: String,
    pub old: String,
    pub new: String,
}

#[derive(Clone, Debug)]
pub struct Plan {
    /// What the plan does, in a line.
    pub op: String,
    pub files: Vec<FileChange>,
    /// What else a person should know: visibility widened, imports added.
    pub notes: Vec<String>,
}

/// Why a change was refused, and the errors the program would have had, if that's why.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub why: String,
    pub errors: Option<String>,
}

impl From<String> for Refusal {
    fn from(why: String) -> Refusal {
        Refusal { why, errors: None }
    }
}

impl From<&str> for Refusal {
    fn from(why: &str) -> Refusal {
        Refusal { why: why.to_string(), errors: None }
    }
}

mod extract;
pub use extract::{extract_const, extract_fn, inline};

fn refuse<T>(why: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::from(why.into()))
}

/// The files being changed: each one's text now, by its path in the package.
#[derive(Default)]
struct Texts {
    root: PathBuf,
    now: BTreeMap<String, String>,
    before: BTreeMap<String, String>,
}

impl Texts {
    fn new(root: &Path) -> Texts {
        Texts { root: root.to_path_buf(), ..Texts::default() }
    }

    fn get(&mut self, file: &str) -> Result<&mut String, Refusal> {
        if !self.now.contains_key(file) {
            let text = std::fs::read_to_string(self.root.join(file))
                .map_err(|e| format!("can't read {file}: {e}"))?;
            self.before.insert(file.to_string(), text.clone());
            self.now.insert(file.to_string(), text);
        }
        Ok(self.now.get_mut(file).expect("read"))
    }

    /// Replaces each `(start, end)` of `file` (offsets in its text now) with its text.
    fn replace(&mut self, file: &str, mut edits: Vec<(u32, u32, String)>) -> Result<(), Refusal> {
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        edits.dedup_by_key(|e| e.0);
        let text = self.get(file)?;
        for (s, e, new) in edits {
            text.replace_range(s as usize..e as usize, &new);
        }
        Ok(())
    }

    /// [`Texts::replace`] for edits of several files, each `(file, start, end, text)`.
    fn replace_all(&mut self, edits: Vec<(String, u32, u32, String)>) -> Result<(), Refusal> {
        let mut by_file: BTreeMap<String, Vec<(u32, u32, String)>> = BTreeMap::new();
        for (file, s, e, new) in edits {
            by_file.entry(file).or_default().push((s, e, new));
        }
        for (file, edits) in by_file {
            self.replace(&file, edits)?;
        }
        Ok(())
    }

    fn overlay(&self) -> Overlay {
        self.now
            .iter()
            .filter_map(|(f, t)| Some((std::fs::canonicalize(self.root.join(f)).ok()?, t.clone())))
            .collect()
    }

    /// The plan: the files that changed, formatted where they were.
    fn plan(self, op: String, notes: Vec<String>) -> Result<Plan, Refusal> {
        let mut files = Vec::new();
        let mut before = self.before;
        for (file, new) in self.now {
            let old = before.remove(&file).expect("original text");
            if old == new {
                continue;
            }
            let new = crate::on_compiler_thread(|| {
                let formatted = if crate::edit::formatted(&old) {
                    crate::edit::format_text(&new)
                } else {
                    None
                };
                formatted.unwrap_or(new)
            });
            files.push(FileChange { file, old, new });
        }
        if files.is_empty() {
            return refuse("nothing would change");
        }
        Ok(Plan { op, files, notes })
    }
}

/// The program package's file a span is in, by its path in the package: `None` for std's and
/// a dependency's (their names start with `<` and `[`).
fn program_file(sources: &SourceMap, span: Span) -> Option<String> {
    let name = &sources.file(span.file).name;
    (!name.starts_with('<') && !name.starts_with('[') && name.ends_with(".wrela"))
        .then(|| name.clone())
}

/// Refused, with `why` and the errors rendered, if the check `out` found errors.
fn clean(out: crate::Output, why: &str) -> Result<(), Refusal> {
    if !out.has_errors() {
        return Ok(());
    }
    let errors: Vec<Diagnostic> =
        out.diagnostics.into_iter().filter(Diagnostic::is_error).collect();
    let errors = Some(wrela_diag::render::render_all(&out.sources, &errors));
    Err(Refusal { why: why.into(), errors })
}

/// Checks the program with `texts` in place: refused, with `why` and the errors, if it has any.
fn check_clean(root: &Path, texts: &Texts, why: &str) -> Result<(), Refusal> {
    clean(crate::check_with(root, &texts.overlay()), why)
}

/// Checks the program with `texts` in place: the errors in its own files, by file, each the
/// identifier its primary span names (the span itself, or the one `name`-like token in it).
type Found = Vec<(String, u32, u32, String)>;

fn check_round(root: &Path, texts: &Texts) -> (crate::Output, Found) {
    let out = crate::check_with(root, &texts.overlay());
    let mut found = Vec::new();
    for d in out.diagnostics.iter().filter(|d| d.is_error()) {
        let Some(span) = d.span() else { continue };
        let Some(file) = program_file(&out.sources, span) else { continue };
        let text = &out.sources.file(span.file).text;
        let Some(at) = text.get(span.range()) else { continue };
        if wrela_syntax::lexer::is_name(at) {
            found.push((file, span.start, span.end, at.to_string()));
        }
    }
    (out, found)
}

/// Each identifier token `name` in `text`.
fn tokens_named(text: &str, name: &str) -> Vec<(u32, u32)> {
    let lexed = wrela_syntax::lexer::lex(wrela_diag::FileId(0), text);
    lexed
        .tokens
        .iter()
        .filter(|t| {
            t.kind == wrela_syntax::token::TokenKind::Ident
                && text.get(t.span.range()) == Some(name)
        })
        .map(|t| (t.span.start, t.span.end))
        .collect()
}

/// [`crate::with_checked_in`]: what `f` gives, or refused with `why` and the errors if the
/// program, with `overlay`'s texts in place, doesn't check.
fn checked_or<T: Send>(
    root: &Path,
    overlay: &Overlay,
    why: &str,
    f: impl FnOnce(&Checked, &SourceMap) -> T + Send,
) -> Result<T, Refusal> {
    crate::with_checked_in(root, overlay, f)
        .map_err(|errors| Refusal { why: why.into(), errors: Some(errors) })
}

/// Checks the program loads, and gives `f` what it found.
fn inspect<T: Send>(
    root: &Path,
    f: impl FnOnce(&Checked, &SourceMap) -> Result<T, Refusal> + Send,
) -> Result<T, Refusal> {
    let why = "the program doesn't check as it is: fix its errors first";
    checked_or(root, &Overlay::new(), why, f)?
}

/// How many uses the compiler records of the function at `path` (after a change, in `texts`).
fn uses_now(root: &Path, texts: &Texts, path: &str) -> Result<usize, Refusal> {
    checked_or(root, &texts.overlay(), "the changed program doesn't check", |checked, sources| {
        let ix = Index::new(checked, sources);
        match ix.one(path) {
            Ok((Item::Fn(f), _)) => Ok(ix.uses_of(f).len()),
            _ => refuse(format!("after the change, `{path}` isn't one function")),
        }
    })?
}

/// Repeats checks until the program has no errors: each round, `fix` gets what the errors name
/// and makes its edits (false: it can't fix them).
fn converge(
    root: &Path,
    texts: &mut Texts,
    mut fix: impl FnMut(&mut Texts, &Found) -> Result<bool, Refusal>,
) -> Result<(), Refusal> {
    for _ in 0..ROUNDS {
        let (out, found) = check_round(root, texts);
        if !out.has_errors() {
            return Ok(());
        }
        if found.is_empty() || !fix(texts, &found)? {
            return clean(out, "the program wouldn't check after the change");
        }
    }
    refuse(format!("the program still had errors after {ROUNDS} rounds of fixes"))
}

// ---- rename ------------------------------------------------------------------------------------

struct RenameInfo {
    old: String,
    /// The definitions' names: file and offsets.
    defs: Vec<(String, u32, u32)>,
    /// For a function: its path after the rename, and its uses before.
    calls: Option<(String, usize)>,
    path: String,
}

pub fn rename(root: &Path, item: &str, new: &str) -> Result<Plan, Refusal> {
    if !wrela_syntax::lexer::is_name(new) {
        return refuse(format!("`{new}` isn't a name"));
    }
    let info = inspect(root, |checked, sources| {
        let ix = Index::new(checked, sources);
        let p = ix.p;
        let (it, path) = ix.one(item)?;
        let own = |span: Span| {
            program_file(sources, span).ok_or_else(|| {
                format!("`{path}` isn't the program's own: rename it in its package")
            })
        };
        let module = it.decl(p).module;
        if p.package_of(module).kind != PackageKind::Program {
            return refuse(format!("`{path}` isn't the program's own: rename it in its package"));
        }
        let mut defs = Vec::new();
        let old: String;
        let mut calls = None;
        match it {
            Item::Fn(f) => {
                let def = p.func(f);
                old = def.name.clone();
                defs.push((own(def.name_span)?, def.name_span.start, def.name_span.end));
                // The methods beside it that it would clash with (a free function's names are
                // its module's, checked below).
                let siblings: Vec<String> = match def.owner {
                    FnOwner::Free => Vec::new(),
                    FnOwner::Impl(im) => {
                        if let Some(tr) = &p.impl_(im).trait_ref {
                            return refuse(format!(
                                "`{path}` implements `{}`'s method: rename the trait's method",
                                p.trait_(tr.trait_).name
                            ));
                        }
                        let self_ty = p.impl_(im).self_ty;
                        p.impls
                            .iter()
                            .filter(|i| crate::query::same_head(p, i.self_ty, self_ty))
                            .flat_map(|i| i.methods.iter().map(|&m| p.func(m).name.clone()))
                            .collect()
                    }
                    FnOwner::Trait(t) => {
                        for m in ix.with_impls(f).into_iter().skip(1) {
                            let s = p.func(m).name_span;
                            defs.push((own(s)?, s.start, s.end));
                        }
                        p.trait_(t).methods.iter().map(|&m| p.func(m).name.clone()).collect()
                    }
                    FnOwner::Const(_) => {
                        return refuse("a constant's function has no name to rename");
                    }
                };
                if siblings.iter().any(|s| s == new) {
                    return refuse(format!("`{new}` is already a method there"));
                }
                let mut segments: Vec<&str> = path.split("::").collect();
                segments.pop();
                segments.push(new);
                calls = Some((segments.join("::"), ix.uses_of(f).len()));
            }
            Item::Adt(a) => {
                let def = p.adt(a);
                old = def.name.clone();
                defs.push((own(def.name_span)?, def.name_span.start, def.name_span.end));
            }
            Item::Trait(t) => {
                let def = p.trait_(t);
                old = def.name.clone();
                let file = own(def.span)?;
                let text = p.text(def.span).unwrap_or("");
                let Some(&(s, e)) = tokens_named(text, &old).first() else {
                    return refuse(format!("can't find `{old}` in its declaration"));
                };
                defs.push((file, def.span.start + s, def.span.start + e));
            }
        }
        // A free function, a type or a trait: a name of its module.
        let free = !matches!(it, Item::Fn(f) if p.func(f).owner != FnOwner::Free);
        if free && wrela_sema::resolve::lookup_name(p, module, new).is_some() {
            return refuse(format!(
                "`{new}` is already a name in `{}`",
                p.module(module).path.join("::")
            ));
        }
        // A type or trait with a name the prelude or a built-in has would leave references
        // that resolve to that one instead.
        if !matches!(it, Item::Fn(_)) && wrela_sema::resolve::lookup_global(p, &old).is_some() {
            return refuse(format!(
                "`{old}` is also a built-in or prelude name, so its references can't be told apart"
            ));
        }
        Ok(RenameInfo { old, defs, calls, path })
    })?;
    if info.old == new {
        return refuse("that's its name already");
    }
    let mut texts = Texts::new(root);
    texts
        .replace_all(info.defs.iter().map(|(f, s, e)| (f.clone(), *s, *e, new.into())).collect())?;
    converge(root, &mut texts, |texts, found| {
        let edits: Vec<_> = found
            .iter()
            .filter(|(.., name)| *name == info.old)
            .map(|(f, s, e, _)| (f.clone(), *s, *e, new.to_string()))
            .collect();
        if edits.is_empty() {
            return Ok(false);
        }
        texts.replace_all(edits)?;
        Ok(true)
    })?;
    if let Some((after, before)) = &info.calls {
        let now = uses_now(root, &texts, after)?;
        if now != *before {
            let times = |n: usize| if n == 1 { "once".to_string() } else { format!("{n} times") };
            return refuse(format!(
                "`{}` is used {} and would be used {}: a call would go to another function",
                info.path,
                times(*before),
                times(now)
            ));
        }
    }
    texts.plan(format!("rename `{}` to `{new}`", info.path), Vec::new())
}

// ---- move --------------------------------------------------------------------------------------

struct MoveInfo {
    name: String,
    path: String,
    from: String,
    from_module: String,
    /// The byte ranges to move, in order: the item, then its impls.
    ranges: Vec<(u32, u32)>,
    public: bool,
    /// Where `pub(package) ` would go, in the moved text's first range: the item's keyword.
    keyword: u32,
    /// Each name the old module sees (but the item's), and the path to import it by.
    imports: BTreeMap<String, String>,
    /// The original call graph, with the moved functions named at their new paths.
    calls: BTreeMap<(String, String), usize>,
}

/// Each caller and target, with their use counts. Paths remain stable when a new check gives
/// definitions new IDs; `renames` accounts for the item and methods the move relocates.
fn call_targets(
    checked: &Checked,
    sources: &SourceMap,
    renames: &BTreeMap<String, String>,
) -> BTreeMap<(String, String), usize> {
    let ix = Index::new(checked, sources);
    let name = |f| {
        let path = ix.fn_path(f);
        renames.get(&path).cloned().unwrap_or(path)
    };
    let mut calls = BTreeMap::new();
    for (&f, body) in &checked.mir {
        let caller = name(f);
        for (u, _) in crate::query::uses(ix.p, body) {
            *calls.entry((caller.clone(), name(u.func()))).or_default() += 1;
        }
    }
    calls
}

/// Includes the item's doc comments, attributes and trailing comment, without taking another
/// item that shares its first or last line.
fn whole_lines(f: &SourceFile, span: Span) -> (u32, u32) {
    let text = &f.text;
    let line = f.line_index(span.start);
    // The item's span starts at its keyword, after `pub` where present. A semicolon before
    // it separates a previous item; otherwise its whole opening line belongs to it.
    let line_start = f.line_start(line);
    let prefix = &text[line_start as usize..span.start as usize];
    let start = crate::edit::lex(prefix)
        .filter(|t| t.kind == wrela_syntax::token::TokenKind::Semi)
        .last()
        .map_or_else(
            || f.line_start(crate::query::doc_top(f, line)),
            |t| {
                let rest = &prefix[t.span.end as usize..];
                line_start + t.span.end + (rest.len() - rest.trim_start().len()) as u32
            },
        );
    let line_end =
        text[span.end as usize..].find('\n').map_or(text.len(), |i| span.end as usize + i + 1);
    let tail = &text[span.end as usize..line_end];
    let rest = tail.trim_start();
    let after_separator = rest.strip_prefix(';').unwrap_or(rest).trim_start();
    let end = if after_separator.is_empty() || after_separator.starts_with("//") {
        line_end
    } else {
        span.end as usize + (tail.len() - rest.len()) + usize::from(rest.starts_with(';'))
    };
    (start, end as u32)
}

/// The byte ranges of `text`'s top-level `use` statements, in order: each a line that starts
/// with `use `, and the lines after it up to its `}` if it opens a `{` there.
fn use_statements(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let end = offset + line.len();
        match open {
            Some(start) if line.contains('}') => {
                out.push((start, end));
                open = None;
            }
            Some(_) => {}
            None if line.starts_with("use ") => {
                if line.contains('{') && !line.contains('}') {
                    open = Some(offset);
                } else {
                    out.push((offset, end));
                }
            }
            None => {}
        }
        offset = end;
    }
    out.extend(open.map(|start| (start, text.len())));
    out
}

/// Where a new import goes: after the file's last top-level `use` (its end, for one over
/// several lines), else after its first comment block.
fn import_at(text: &str) -> usize {
    use_statements(text).last().map_or_else(
        || text.split_inclusive('\n').take_while(|l| l.starts_with("//")).map(str::len).sum(),
        |&(_, end)| end,
    )
}

/// `text` with `name` taken out of the top-level `use` that imports it (the whole `use`, if
/// it's its only name): `None` if no `use` of the simple forms (`use a::name`,
/// `use a::{.., name, ..}`) imports it.
fn without_import(text: &str, name: &str) -> Option<String> {
    use_statements(text).into_iter().find_map(|(start, end)| {
        let body = text[start..end].trim_end().trim_start_matches("use ");
        let replacement = if let Some((head, list)) = body.split_once('{') {
            let names: Vec<&str> = list
                .trim_end_matches('}')
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .collect();
            if !names.contains(&name) {
                return None;
            }
            let rest: Vec<&str> = names.into_iter().filter(|n| *n != name).collect();
            if rest.is_empty() {
                String::new()
            } else {
                format!("use {head}{{{}}}\n", rest.join(", "))
            }
        } else if body.rsplit("::").next() == Some(name) {
            String::new()
        } else {
            return None;
        };
        Some(format!("{}{replacement}{}", &text[..start], &text[end..]))
    })
}

pub fn move_item(root: &Path, item: &str, module: &str) -> Result<Plan, Refusal> {
    let to = format!("{}.wrela", module.replace("::", "/"));
    if !root.join(&to).is_file() {
        return refuse(format!("there's no module `{module}` ({to}): create the file first"));
    }
    let info = inspect(root, |checked, sources| {
        let ix = Index::new(checked, sources);
        let p = ix.p;
        let (it, path) = ix.one(item)?;
        if let Item::Fn(f) = it
            && p.func(f).owner != FnOwner::Free
        {
            return refuse(format!("`{path}` is a method: move its type"));
        }
        let crate::query::Decl { module: module_id, name, span, name_span, public } = it.decl(p);
        let Some(from) = program_file(sources, span) else {
            return refuse(format!("`{path}` isn't the program's own"));
        };
        let from_module = p.module(module_id).path.join("::");
        if from_module == module {
            return refuse(format!("`{path}` is in `{module}` already"));
        }
        let is_export = |m: &str| m == "main";
        if matches!(it, Item::Fn(_)) && public && (is_export(&from_module) || is_export(module)) {
            return refuse(
                "main.wrela's public functions are the program's exports: moving one in or out changes them",
            );
        }
        let file = sources.file(span.file);
        let text = &file.text;
        let mut ranges = vec![whole_lines(file, span)];
        if let Item::Adt(_) = it {
            for im in ix.impl_ids(it).into_iter().map(|i| &p.impls[i]) {
                if im.span.file == span.file && !im.from_opt_in {
                    ranges.push(whole_lines(file, im.span));
                }
            }
        }
        // The keyword: the first `fn`, `struct`, `enum` or `trait` token of the item's span.
        let lexed = wrela_syntax::lexer::lex(
            wrela_diag::FileId(0),
            &text[span.start as usize..name_span.end as usize],
        );
        let keyword = lexed
            .tokens
            .iter()
            .find(|t| {
                matches!(
                    text.get(
                        (span.start + t.span.start) as usize..(span.start + t.span.end) as usize
                    ),
                    Some("fn" | "struct" | "enum" | "trait")
                )
            })
            .map_or(span.start, |t| span.start + t.span.start);
        let mut imports = BTreeMap::new();
        let target_module = p.modules.iter().position(|m| m.path.join("::") == module);
        for (n, b) in &p.module(module_id).scope {
            if *n == name {
                continue;
            }
            let at = |m: wrela_sema::ty::ModuleId| p.module(m).path.join("::");
            let path = match b.res {
                Res::Adt(a) => format!("{}::{}", at(p.adt(a).module), p.adt(a).name),
                Res::Trait(t) => format!("{}::{}", at(p.trait_(t).module), p.trait_(t).name),
                Res::Fn(f) if p.func(f).owner == FnOwner::Free => {
                    format!("{}::{}", at(p.func(f).module), p.func(f).name)
                }
                Res::Const(c) => format!("{}::{}", at(p.const_(c).module), p.const_(c).name),
                Res::Module(m) => at(m),
                _ => continue,
            };
            // Scalar constants are folded into MIR, so a call-graph comparison cannot see
            // their binding change. Refuse a conflicting bare name before moving it; an
            // explicit path remains safe in either module.
            if matches!(b.res, Res::Const(_))
                && target_module.is_some_and(|target| {
                    wrela_sema::resolve::lookup_name(p, wrela_sema::ty::ModuleId(target as u32), n)
                        .is_some_and(|res| res != b.res)
                })
                && ranges.iter().any(|&(s, e)| {
                    let moved = &text[s as usize..e as usize];
                    let tokens: Vec<_> = crate::edit::lex(moved).collect();
                    tokens.iter().enumerate().any(|(i, token)| {
                        token.kind == wrela_syntax::token::TokenKind::Ident
                            && &moved[token.span.range()] == n
                            && (i == 0
                                || tokens[i - 1].kind != wrela_syntax::token::TokenKind::ColonColon)
                    })
                })
            {
                return refuse(format!(
                    "`{n}` names another item in `{module}`: qualify it as `{path}` before moving"
                ));
            }
            imports.insert(n.clone(), path);
        }
        let mut renames = BTreeMap::new();
        let prefix = format!("{from_module}::");
        for (i, f) in p.fns.iter().enumerate() {
            if f.span.file == span.file
                && ranges.iter().any(|&(s, e)| s <= f.span.start && f.span.end <= e)
            {
                let path = ix.fn_path(wrela_sema::ty::FnId(i as u32));
                if let Some(rest) = path.strip_prefix(&prefix) {
                    renames.insert(path.clone(), format!("{module}::{rest}"));
                }
            }
        }
        let calls = call_targets(checked, sources, &renames);
        Ok(MoveInfo {
            name: name.to_string(),
            path,
            from,
            from_module,
            ranges,
            public,
            keyword,
            imports,
            calls,
        })
    })?;
    let mut texts = Texts::new(root);
    let mut notes = Vec::new();
    // Cut the item (and its impls) out, and put it at the end of the module it goes to.
    let source = texts.get(&info.from)?.clone();
    let mut moved: Vec<String> = Vec::new();
    // The item as written: made visible to the module it leaves, which may name it, and put
    // back as it was below if no other module does.
    let written = source[info.ranges[0].0 as usize..info.ranges[0].1 as usize].to_string();
    for &(s, e) in &info.ranges {
        let mut t = source[s as usize..e as usize].to_string();
        if (s, e) == info.ranges[0] && !info.public {
            t.insert_str((info.keyword - s) as usize, "pub(package) ");
        }
        moved.push(t);
    }
    let cut: Vec<(u32, u32, String)> =
        info.ranges.iter().map(|&(s, e)| (s, e, String::new())).collect();
    texts.replace(&info.from, cut)?;
    {
        let from = texts.get(&info.from)?;
        while from.contains("\n\n\n") {
            *from = from.replace("\n\n\n", "\n\n");
        }
    }
    {
        let target = texts.get(&to)?;
        if !target.ends_with('\n') && !target.is_empty() {
            target.push('\n');
        }
        for t in &moved {
            target.push('\n');
            target.push_str(t);
        }
    }
    // The module it goes to may have imported it: it's that module's own now.
    if let Some(t) = without_import(texts.get(&to)?, &info.name) {
        *texts.get(&to)? = t;
    }
    let mut imported: Vec<(String, String)> = Vec::new();
    converge(root, &mut texts, |texts, found| {
        let mut changed = false;
        for (file, start, _, n) in found {
            let path = if *n == info.name && *file != to {
                // An import of it from where it was goes; the new one replaces it.
                let text = texts.get(file)?;
                let line = text[..*start as usize].rfind('\n').map_or(0, |i| i + 1);
                if text[line..].starts_with("use ")
                    && let Some(t) = without_import(text, &info.name)
                {
                    *text = t;
                    changed = true;
                }
                format!("{module}::{}", info.name)
            } else if *file == to {
                match info.imports.get(n) {
                    Some(p) => p.clone(),
                    None => continue,
                }
            } else {
                continue;
            };
            if imported.iter().any(|(f, p)| f == file && *p == path) {
                continue;
            }
            let text = texts.get(file)?;
            let at = import_at(text);
            text.insert_str(at, &format!("use {path}\n"));
            imported.push((file.clone(), path));
            changed = true;
        }
        Ok(changed)
    })?;
    // Imports the old module no longer needs: names the moved code took with it.
    for (file, path) in imported.clone() {
        if file != to {
            continue;
        }
        let n = path.rsplit("::").next().unwrap_or("").to_string();
        let from = texts.get(&info.from)?.clone();
        let still: usize = tokens_named(&from, &n).len();
        let in_imports = from
            .lines()
            .filter(|l| l.starts_with("use ") && tokens_named(l, &n).len() == 1)
            .count();
        if still == in_imports
            && let Some(t) = without_import(&from, &n)
        {
            *texts.get(&info.from)? = t;
            if crate::check_with(root, &texts.overlay()).has_errors() {
                *texts.get(&info.from)? = from;
            }
        }
    }
    // `pub(package)` only if another module names it.
    if !info.public {
        let others =
            imported.iter().any(|(f, p)| *f != to && p.ends_with(&format!("::{}", info.name)));
        if others {
            notes.push(format!(
                "`{}` is now `pub(package)`: `{}` names it",
                info.name, info.from_module
            ));
        } else {
            let target = texts.get(&to)?;
            *target = target.replacen(&moved[0], &written, 1);
        }
    }
    for (f, p) in &imported {
        notes.push(format!("{f} imports `{p}`"));
    }
    let why = "the program wouldn't check after the move";
    let calls = checked_or(root, &texts.overlay(), why, |checked, sources| {
        call_targets(checked, sources, &BTreeMap::new())
    })?;
    if calls != info.calls {
        return refuse("moving the item would make a call go to another function");
    }
    texts.plan(format!("move `{}` to `{module}`", info.path), notes)
}

// ---- add-param and change-mode -----------------------------------------------------------------

/// A function's definitions (its impls' too, for a trait's method) and every call of it.
struct Calls {
    path: String,
    /// Each definition: its file, its parameters' spans and modes, and where its parameter list
    /// opens and closes.
    defs: Vec<Def>,
    /// Each call: its file, the call's span, and each argument's span and whether a `take`
    /// parameter would need it marked `take` (a place, not taken, whose type isn't `Copy`).
    calls: Vec<CallSite>,
    /// Where it's used as a value: a change to its parameters changes its type there.
    values: Vec<String>,
}

struct Def {
    /// An impl's method for the trait's method being changed: it takes the trait's defaults.
    implements: bool,
    file: String,
    params: Vec<(String, Span, Mode, bool)>,
    open: u32,
    close: u32,
}

struct CallSite {
    file: String,
    span: Span,
    args: Vec<(Span, bool)>,
}

/// The offsets of the `(` and `)` of the parameter list after a function's name.
fn param_list(text: &str, after: usize) -> Option<(u32, u32)> {
    use wrela_syntax::token::TokenKind;
    let mut angle = 0usize;
    let mut depth = 0usize;
    let mut open = None;
    for token in crate::edit::lex(text.get(after..)?) {
        if open.is_none() {
            match token.kind {
                TokenKind::Lt => angle += 1,
                TokenKind::Gt => angle = angle.saturating_sub(1),
                TokenKind::Shr => angle = angle.saturating_sub(2),
                TokenKind::LParen if angle == 0 => {
                    open = Some(after as u32 + token.span.start);
                    depth = 1;
                }
                _ => {}
            }
        } else {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((open?, after as u32 + token.span.start));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// The definitions and calls of the function at `func` in the checked program.
fn calls_in(checked: &Checked, sources: &SourceMap, func: &str) -> Result<Calls, Refusal> {
    let ix = Index::new(checked, sources);
    let p = ix.p;
    let (it, path) = ix.one(func)?;
    let Item::Fn(f) = it else { return refuse(format!("`{path}` isn't a function")) };
    if let FnOwner::Impl(im) = p.func(f).owner
        && p.impl_(im).trait_ref.is_some()
    {
        return refuse(format!("`{path}` implements a trait's method: change the trait's"));
    }
    let fns = ix.with_impls(f);
    let mut defs = Vec::new();
    for &g in &fns {
        let d = p.func(g);
        let file = program_file(sources, d.span)
            .ok_or_else(|| format!("`{}` isn't the program's own", ix.fn_path(g)))?;
        let text = &sources.file(d.span.file).text;
        let (open, close) = param_list(text, d.name_span.end as usize)
            .ok_or_else(|| format!("can't find `{}`'s parameters", ix.fn_path(g)))?;
        let params = d
            .params
            .iter()
            .map(|x| (x.name.clone(), x.span, x.mode, x.default.is_some()))
            .collect();
        defs.push(Def { implements: g != f, file, params, open, close });
    }
    let mut calls = Vec::new();
    let mut values = Vec::new();
    for body in checked.mir.values() {
        for (s, r) in wrela_sema::mir::rvalues(body) {
            match r {
                Rvalue::Call(c) => {
                    let hit = match &c.callee {
                        Callee::Fn { func, .. } => fns.contains(func),
                        Callee::TraitMethod { method, .. } => *method == f,
                        _ => false,
                    };
                    if !hit {
                        continue;
                    }
                    let Some(file) = program_file(sources, c.span) else { continue };
                    let args = c
                        .args
                        .iter()
                        .map(|a| {
                            let needs_take = match a {
                                Arg::Borrow(pl, _) | Arg::Mut(pl, _) => {
                                    let ty = wrela_sema::mir::place_ty(p, &body.locals, pl);
                                    !wrela_sema::traits::implements_builtin(p, ty, Lang::Copy)
                                }
                                Arg::Take(_) => false,
                            };
                            (a.span(), needs_take)
                        })
                        .collect();
                    calls.push(CallSite { file, span: c.span, args });
                }
                Rvalue::FnRef(g, _) if fns.contains(g) => values.push(ix.at(s.span)),
                _ => {}
            }
        }
    }
    Ok(Calls { path, defs, calls, values })
}

/// The `(` that the `)` at `close` closes.
fn opening(text: &str, close: u32) -> u32 {
    use wrela_syntax::token::TokenKind;
    let mut depth = 0;
    let tokens: Vec<_> = crate::edit::lex(text).take_while(|t| t.span.start <= close).collect();
    for token in tokens.into_iter().rev() {
        match token.kind {
            TokenKind::RParen => depth += 1,
            TokenKind::LParen if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return token.span.start;
                }
            }
            _ => {}
        }
    }
    close
}

/// Where to add `item` (an argument or a parameter) before the `)` at `close`: after a
/// trailing comma, or with a comma after what's there.
fn before_close(text: &str, open: u32, close: u32, item: &str) -> (u32, u32, String) {
    let inner = &text[open as usize + 1..close as usize];
    let Some(last) = crate::edit::lex(inner).last() else {
        return (open + 1, open + 1, item.to_string());
    };
    let at = open + 1 + last.span.end;
    if last.kind == wrela_syntax::token::TokenKind::Comma {
        (at, at, format!(" {item},"))
    } else {
        (at, at, format!(", {item}"))
    }
}

pub fn add_param(
    root: &Path,
    func: &str,
    param: &str,
    value: Option<&str>,
    default: Option<&str>,
) -> Result<Plan, Refusal> {
    let Some((name, ty)) = param.split_once(':') else {
        return refuse(format!("a parameter is `<name>: <type>`, not `{param}`"));
    };
    let (name, ty) = (name.trim(), ty.trim());
    if !wrela_syntax::lexer::is_name(name) || ty.is_empty() {
        return refuse(format!("a parameter is `<name>: <type>`, not `{param}`"));
    }
    if value.is_some() == default.is_some() {
        return refuse(
            "give `--value <expr>` (each call passes it) or `--default <expr>` (the parameter has it)",
        );
    }
    let c = inspect(root, |checked, sources| calls_in(checked, sources, func))?;
    if !c.values.is_empty() && value.is_some() {
        return refuse(format!(
            "`{}` is used as a value at {}: a new parameter changes its type",
            c.path,
            c.values.join(", ")
        ));
    }
    let mut texts = Texts::new(root);
    // Every edit in the files' offsets as they are, applied at once.
    let mut edits = Vec::new();
    for d in &c.defs {
        if d.params.iter().any(|(n, ..)| n == name) {
            return refuse(format!("`{}` has a parameter `{name}` already", c.path));
        }
        let text: &str = texts.get(&d.file)?;
        let decl = match default {
            Some(e) if !d.implements => format!("{name}: {ty} = {e}"),
            _ => format!("{name}: {ty}"),
        };
        // A parameter without a default goes before the first that has one.
        let edit = match d.params.iter().find(|x| x.3) {
            Some(first) if default.is_none() => (first.1.start, first.1.start, format!("{decl}, ")),
            _ => before_close(text, d.open, d.close, &decl),
        };
        edits.push((d.file.clone(), edit.0, edit.1, edit.2));
    }
    if let Some(v) = value {
        // Each call passes it by name, before its `)`.
        for call in &c.calls {
            let text: &str = texts.get(&call.file)?;
            if !text[call.span.range()].ends_with(')') {
                return refuse(format!(
                    "can't find the end of the call at {}:{}",
                    call.file, call.span.start
                ));
            }
            let close = call.span.end - 1;
            let call_text = &text[call.span.range()];
            let open = call.span.start + opening(call_text, call.span.end - call.span.start - 1);
            let (s, e, new) = before_close(text, open, close, &format!("{name}: {v}"));
            edits.push((call.file.clone(), s, e, new));
        }
    }
    texts.replace_all(edits)?;
    check_clean(root, &texts, "the program wouldn't check after the change")?;
    let what = match (value, default) {
        (Some(v), _) => format!("add `{name}: {ty}` to `{}`, each call passing `{v}`", c.path),
        (_, Some(d)) => format!("add `{name}: {ty} = {d}` to `{}`", c.path),
        _ => unreachable!(),
    };
    texts.plan(what, Vec::new())
}

pub fn change_mode(root: &Path, func: &str, param: &str, mode: &str) -> Result<Plan, Refusal> {
    let new_mode = match mode {
        "borrow" => Mode::Borrow,
        "mut" => Mode::Mut,
        "take" => Mode::Take,
        _ => return refuse(format!("a mode is `borrow`, `mut` or `take`, not `{mode}`")),
    };
    if param == "self" {
        return refuse("`self`'s mode is the method's: change it by hand");
    }
    let c = inspect(root, |checked, sources| calls_in(checked, sources, func))?;
    if !c.values.is_empty() {
        return refuse(format!(
            "`{}` is used as a value at {}: its parameter's mode is part of its type",
            c.path,
            c.values.join(", ")
        ));
    }
    let mut edits = Vec::new();
    let mut index = None;
    let mut texts = Texts::new(root);
    for d in &c.defs {
        let Some(i) = d.params.iter().position(|x| x.0 == param) else {
            return refuse(format!("`{}` has no parameter `{param}`", c.path));
        };
        index = Some(i);
        let (_, span, old, _) = &d.params[i];
        if *old == new_mode {
            return refuse(format!("`{param}` is `{mode}` already"));
        }
        let text: &str = texts.get(&d.file)?;
        let decl = &text[span.range()];
        let Some(colon) = decl.find(':') else { return refuse(format!("can't read `{decl}`")) };
        let ty = decl[colon + 1..].trim_start();
        let ty = ["mut ", "take ", "borrow "].iter().find_map(|m| ty.strip_prefix(m)).unwrap_or(ty);
        let prefix = match new_mode {
            Mode::Borrow => "",
            Mode::Mut => "mut ",
            Mode::Take => "take ",
        };
        let new = format!("{}: {prefix}{ty}", &decl[..colon]);
        edits.push((d.file.clone(), span.start, span.end, new));
    }
    let i = index.expect("a definition");
    for call in &c.calls {
        let Some(&(span, needs_take)) = call.args.get(i) else { continue };
        // An argument the call leaves to its default is the definition's: nothing to mark here.
        let within = call.span.file == span.file
            && call.span.start <= span.start
            && span.end <= call.span.end;
        if !within {
            continue;
        }
        let text: &str = texts.get(&call.file)?;
        let arg = &text[span.range()];
        let bare = ["mut ", "take "].iter().find_map(|m| arg.strip_prefix(m)).unwrap_or(arg);
        let new = match new_mode {
            Mode::Mut => format!("mut {bare}"),
            Mode::Take if needs_take => format!("take {bare}"),
            _ => bare.to_string(),
        };
        if new != arg {
            edits.push((call.file.clone(), span.start, span.end, new));
        }
    }
    texts.replace_all(edits)?;
    check_clean(root, &texts, "the program wouldn't check after the change")?;
    texts.plan(format!("make `{param}` of `{}` `{mode}`", c.path), Vec::new())
}

// ---- plans -------------------------------------------------------------------------------------

/// A run of changed lines: where they start (from 1) in each text, and what they are.
#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub old_start: usize,
    pub old: Vec<String>,
    pub new_start: usize,
    pub new: Vec<String>,
}

/// The hunks that turn `old` into `new`: a longest common subsequence of their lines, between
/// the lines they start and end with alike (one hunk for all of it, past 4 million pairs).
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    if ma.is_empty() && mb.is_empty() {
        return Vec::new();
    }
    // The pairs of lines kept, in order.
    let mut keep: Vec<(usize, usize)> = Vec::new();
    if ma.len() * mb.len() <= 4_000_000 {
        let (n, m) = (ma.len(), mb.len());
        let mut lcs = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * (m + 1) + j] = if ma[i] == mb[j] {
                    lcs[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if ma[i] == mb[j] {
                keep.push((i, j));
                i += 1;
                j += 1;
            } else if lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    keep.push((ma.len(), mb.len()));
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ki, kj) in keep {
        if ki > i || kj > j {
            out.push(Hunk {
                old_start: pre + i + 1,
                old: ma[i..ki].iter().map(|s| s.to_string()).collect(),
                new_start: pre + j + 1,
                new: mb[j..kj].iter().map(|s| s.to_string()).collect(),
            });
        }
        i = ki + 1;
        j = kj + 1;
    }
    out
}

/// A unified diff of the plan.
pub fn diff(plan: &Plan) -> String {
    let mut s = String::new();
    for f in &plan.files {
        s.push_str(&format!("--- {}\n+++ {}\n", f.file, f.file));
        for h in hunks(&f.old, &f.new) {
            s.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                h.old_start,
                h.old.len(),
                h.new_start,
                h.new.len()
            ));
            for l in &h.old {
                s.push_str(&format!("-{l}\n"));
            }
            for l in &h.new {
                s.push_str(&format!("+{l}\n"));
            }
        }
    }
    s
}

/// What `wrela refactor --json` prints: the plan (its files' hashes, hunks and new texts, which
/// `apply` takes), and whether it was written.
pub fn to_json(plan: &Plan, written: bool) -> String {
    let files: Vec<Value> = plan
        .files
        .iter()
        .map(|f| {
            let hunks: Vec<Value> = hunks(&f.old, &f.new)
                .into_iter()
                .map(|h| json!({ "old_start": h.old_start, "old": h.old, "new_start": h.new_start, "new": h.new }))
                .collect();
            json!({
                "file": f.file,
                "old_hash": crate::lift::file_hash(f.old.as_bytes()),
                "new_hash": crate::lift::file_hash(f.new.as_bytes()),
                "hunks": hunks,
                "text": f.new,
            })
        })
        .collect();
    crate::pretty(
        &json!({ "version": 1, "op": plan.op, "written": written, "notes": plan.notes, "files": files, "diff": diff(plan) }),
    )
}

/// Refusal as JSON.
pub fn refusal_json(r: &Refusal) -> String {
    crate::pretty(&json!({ "version": 1, "refused": r.why, "errors": r.errors }))
}

/// The package's source modules are the only files a refactor plan may change. A plan can be
/// read from JSON, so a relative path alone does not establish that it belongs to the program.
fn source_files(root: &Path) -> Result<BTreeSet<String>, Refusal> {
    let files = crate::package::find_files(root)
        .map_err(|_| Refusal::from("can't find the package's source files".to_string()))?;
    Ok(files.into_iter().map(|f| f.display).collect())
}

fn source_path(root: &Path, file: &str, files: &BTreeSet<String>) -> Result<PathBuf, Refusal> {
    if wrela_abi::check::path_problem(file).is_some() || !files.contains(file) {
        return refuse(format!("{file} isn't a source file in the package"));
    }
    let path = root.join(file);
    crate::ensure_no_symlink_path(root, &path)
        .map_err(|e| Refusal::from(format!("{file} isn't a plain source path: {e}")))?;
    Ok(path)
}

/// Reads a plan `to_json` printed, and checks it against the files now: each must still hash
/// as it did, and the program must check with the plan's texts.
pub fn read_plan(root: &Path, text: &str) -> Result<Plan, Refusal> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("the plan isn't JSON: {e}"))?;
    let bad = |what: &str| Refusal::from(format!("the plan has no {what}"));
    let op = v["op"].as_str().ok_or_else(|| bad("`op`"))?.to_string();
    let files = source_files(root)?;
    let mut texts = Texts::new(root);
    for f in v["files"].as_array().ok_or_else(|| bad("`files`"))? {
        let file = f["file"].as_str().ok_or_else(|| bad("file name"))?;
        let (old_hash, new_hash) =
            (f["old_hash"].as_str().unwrap_or(""), f["new_hash"].as_str().unwrap_or(""));
        let new = f["text"].as_str().ok_or_else(|| bad("file text"))?;
        source_path(root, file, &files)?;
        let now = texts.get(file)?;
        let hash = crate::lift::file_hash(now.as_bytes());
        if hash != old_hash {
            return refuse(format!(
                "{file} has changed since the plan was made (hash {hash}, not {old_hash}): make the plan again"
            ));
        }
        if crate::lift::file_hash(new.as_bytes()) != new_hash {
            return refuse(format!("{file}'s new text doesn't hash as the plan says"));
        }
        *now = new.to_string();
    }
    check_clean(root, &texts, "the program wouldn't check with the plan")?;
    let notes = v["notes"]
        .as_array()
        .map(|n| n.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let files = texts
        .now
        .iter()
        .map(|(f, new)| FileChange {
            file: f.clone(),
            old: texts.before[f].clone(),
            new: new.clone(),
        })
        .collect();
    Ok(Plan { op, files, notes })
}

/// Writes the plan's files, each whole (a temporary file renamed over it), after checking once
/// more that each is as the plan found it.
pub fn write(root: &Path, plan: &Plan) -> Result<(), Refusal> {
    // Keep the package directory open while checking and writing. A later replacement of a
    // source directory with a link cannot redirect the write outside this handle.
    let dir = Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|e| Refusal::from(format!("can't open the package: {e}")))?;
    let files = source_files(root)?;
    // Check every destination before writing any of them.
    for f in &plan.files {
        source_path(root, &f.file, &files)?;
    }
    for f in &plan.files {
        let cant =
            |e: std::io::Error| format!("can't read {} before writing the plan: {e}", f.file);
        let Some(bytes) = crate::reach::read_regular(&dir, Path::new(&f.file)).map_err(cant)?
        else {
            return refuse(format!("{} isn't a regular source file", f.file));
        };
        let now = crate::reach::utf8(bytes).map_err(cant)?;
        if now != f.old {
            return refuse(format!(
                "{} changed while the plan was made: nothing is written",
                f.file
            ));
        }
    }
    for f in &plan.files {
        crate::write_atomic_in(&dir, Path::new(&f.file), f.new.as_bytes())
            .map_err(|e| format!("can't write {}: {e}", f.file))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_changes_only_plain_source_modules() {
        let dir =
            std::env::temp_dir().join(format!("wrela-refactor-plan-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let source = "pub fn frame(time: f32, width: u32, height: u32) {}\n";
        std::fs::write(dir.join("main.wrela"), source).expect("source");
        std::fs::write(dir.join("wrela.toml"), "[package]\nname = \"game\"\n").expect("manifest");
        std::fs::write(dir.join(".hidden.wrela"), source).expect("hidden file");

        for file in ["wrela.toml", ".hidden.wrela"] {
            let old = std::fs::read_to_string(dir.join(file)).expect("file");
            let forged = Plan {
                op: "change an unrelated file".into(),
                files: vec![FileChange {
                    file: file.into(),
                    old: old.clone(),
                    new: "changed\n".into(),
                }],
                notes: Vec::new(),
            };
            assert!(read_plan(&dir, &to_json(&forged, false)).is_err(), "{file}");
            assert!(write(&dir, &forged).is_err(), "{file}");
            assert_eq!(std::fs::read_to_string(dir.join(file)).expect("unchanged"), old);
        }

        let valid = Plan {
            op: "change source".into(),
            files: vec![FileChange {
                file: "main.wrela".into(),
                old: source.into(),
                new: "pub fn frame(time: f32, width: u32, height: u32) { }\n".into(),
            }],
            notes: Vec::new(),
        };
        let read = read_plan(&dir, &to_json(&valid, false));
        assert!(read.is_ok(), "{read:?}");
        write(&dir, &valid).expect("plain source module");
        assert_eq!(
            std::fs::read_to_string(dir.join("main.wrela")).expect("source"),
            valid.files[0].new
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_plan_refuses_a_linked_source_file() {
        let dir =
            std::env::temp_dir().join(format!("wrela-refactor-plan-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let outside = dir.with_extension("wrela");
        std::fs::write(&outside, "fn main() {}\n").expect("outside source");
        std::os::unix::fs::symlink(&outside, dir.join("main.wrela")).expect("link");
        let forged = Plan {
            op: "change a linked source".into(),
            files: vec![FileChange {
                file: "main.wrela".into(),
                old: "fn main() {}\n".into(),
                new: "fn main() { }\n".into(),
            }],
            notes: Vec::new(),
        };
        assert!(read_plan(&dir, &to_json(&forged, false)).is_err());
        assert!(write(&dir, &forged).is_err());
        assert_eq!(std::fs::read_to_string(&outside).expect("outside source"), forged.files[0].old);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_source_directory_cannot_redirect_a_write() {
        let root = std::env::temp_dir()
            .join(format!("wrela-refactor-replaced-dir-{}", std::process::id()));
        let outside = root.with_extension("outside");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(root.join("module")).expect("source directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        std::fs::write(outside.join("a.wrela"), b"outside").expect("outside source");
        let held = Dir::open_ambient_dir(&root, ambient_authority()).expect("package handle");
        std::fs::rename(root.join("module"), root.join("old-module"))
            .expect("replace source directory");
        std::os::unix::fs::symlink(&outside, root.join("module")).expect("replacement link");
        assert!(crate::write_atomic_in(&held, Path::new("module/a.wrela"), b"changed").is_err());
        assert_eq!(std::fs::read(outside.join("a.wrela")).expect("outside source"), b"outside");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn a_move_refuses_to_change_call_targets() {
        let dir =
            std::env::temp_dir().join(format!("wrela-refactor-move-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let source = "pub fn helper() -> i32 { 1 }\npub fn moved() -> i32 { helper() }\n";
        std::fs::write(dir.join("a.wrela"), source).expect("source module");
        std::fs::write(dir.join("b.wrela"), "fn helper() -> i32 { 2 }\n").expect("target module");
        let result = move_item(&dir, "a::moved", "b");
        assert!(result.is_err_and(|r| r.why.contains("another function")));
        assert_eq!(std::fs::read_to_string(dir.join("a.wrela")).expect("source"), source);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_move_keeps_constant_bindings_and_allows_generics_and_closures() {
        let dir = std::env::temp_dir()
            .join(format!("wrela-refactor-move-bindings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        std::fs::write(
            dir.join("a.wrela"),
            "pub const VALUE: i32 = 1\npub fn moved() -> i32 { VALUE }\n",
        )
        .expect("source module");
        std::fs::write(dir.join("b.wrela"), "const VALUE: i32 = 2\n").expect("target module");
        assert!(
            move_item(&dir, "a::moved", "b")
                .is_err_and(|r| r.why.contains("qualify it as `a::VALUE`"))
        );
        for source in [
            "pub const VALUE: i32 = 1\npub fn moved() -> i32 { a::VALUE }\n",
            "pub fn moved<T: Copy>(x: T) -> T { x }\npub fn caller() -> i32 { moved(3) }\n",
            "pub fn helper(f: fn() -> i32) -> i32 { f() }\npub fn moved() -> i32 { helper(|| 3) }\npub fn caller() -> i32 { moved() }\n",
        ] {
            std::fs::write(dir.join("a.wrela"), source).expect("source module");
            move_item(&dir, "a::moved", "b").expect("move keeps its bindings");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn moving_an_item_leaves_other_items_on_its_line() {
        let dir =
            std::env::temp_dir().join(format!("wrela-refactor-shared-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let moved = "pub fn moved() -> i32 { 1 }";
        let kept = "pub fn kept() -> i32 { 2 }";
        for source in [format!("{moved}; {kept}\n"), format!("{kept}; {moved}\n")] {
            std::fs::write(dir.join("a.wrela"), source).expect("source module");
            std::fs::write(dir.join("b.wrela"), "").expect("target module");
            let plan = move_item(&dir, "a::moved", "b").expect("move plan");
            let old_module = plan.files.iter().find(|f| f.file == "a.wrela").expect("old module");
            let new_module = plan.files.iter().find(|f| f.file == "b.wrela").expect("new module");
            assert!(old_module.new.contains(kept), "{}", old_module.new);
            assert!(!old_module.new.contains(moved), "{}", old_module.new);
            assert!(new_module.new.contains("fn moved"), "{}", new_module.new);
            assert!(!new_module.new.contains("fn kept"), "{}", new_module.new);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// What code after the lines uses and changes is found in its nested blocks too.
    #[test]
    fn extracted_lines_return_what_nested_code_after_them_uses_and_changes() {
        let dir = std::env::temp_dir()
            .join(format!("wrela-refactor-extract-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        let source = "pub fn frame(time: f32, width: u32, height: u32) {}\n\nfn total(n: u32) -> u32 {\n    var t = n\n    t += 1\n    var r: u32 = 0\n    if n > 2 {\n        r = t\n        t += 1\n    }\n    r\n}\n";
        std::fs::write(dir.join("main.wrela"), source).expect("source");
        let plan = extract_fn(&dir, "main.wrela:4-5", "start").expect("extract plan");
        let text = &plan.files[0].new;
        assert!(text.contains("var t = start(n)"), "{text}");
        assert!(text.contains("fn start(n: u32) -> u32 {"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parameter_delimiters_ignore_strings_and_comments() {
        for text in [
            "fn f(s: String = \"(\") {}",
            "fn f(s: String = \")\") {}",
            "fn f(// )\n s: String) {}",
        ] {
            let open = text.find('(').expect("parameter list");
            let close = text.rfind(')').expect("closing delimiter");
            assert_eq!(param_list(text, 4), Some((open as u32, close as u32)), "{text}");
            assert_eq!(opening(text, close as u32), open as u32, "{text}");
        }
    }

    #[test]
    fn a_deleted_empty_file_makes_a_plan_stale() {
        let dir =
            std::env::temp_dir().join(format!("wrela-refactor-deleted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("directory");
        std::fs::write(dir.join("empty.wrela"), "").expect("empty module");
        let plan = Plan {
            op: "move an item into an empty module".into(),
            files: vec![FileChange {
                file: "empty.wrela".into(),
                old: String::new(),
                new: "fn f() {}\n".into(),
            }],
            notes: Vec::new(),
        };
        std::fs::remove_file(dir.join("empty.wrela")).expect("concurrent deletion");
        assert!(
            write(&dir, &plan).is_err(),
            "a deleted module cannot be recreated by a stale plan"
        );
        assert!(!dir.join("empty.wrela").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hunks_are_the_lines_that_changed() {
        let h = hunks("a\nb\nc\nd\n", "a\nB\nc\nd\ne\n");
        assert_eq!(
            h,
            [
                Hunk { old_start: 2, old: vec!["b".into()], new_start: 2, new: vec!["B".into()] },
                Hunk { old_start: 5, old: vec![], new_start: 5, new: vec!["e".into()] },
            ]
        );
        assert!(hunks("same\n", "same\n").is_empty());
    }

    #[test]
    fn imports_go_after_the_last_and_come_out_whole_or_by_name() {
        let text = "// A module.\n\nuse a::b\nuse c::{d,\n    e}\n\nfn f() {}\n";
        assert_eq!(&text[..import_at(text)], "// A module.\n\nuse a::b\nuse c::{d,\n    e}\n");
        assert_eq!(import_at("// Only a comment.\nfn f() {}\n"), 19);
        assert_eq!(without_import("use a::b\nfn f() {}\n", "b").as_deref(), Some("fn f() {}\n"));
        assert_eq!(without_import("use c::{d, e}\n", "d").as_deref(), Some("use c::{e}\n"));
        assert_eq!(without_import("use c::{d}\n", "d").as_deref(), Some(""));
        assert_eq!(without_import("use c::{d}\n", "x"), None);
    }

    #[test]
    fn a_new_item_goes_before_the_close_with_a_comma_where_needed() {
        assert_eq!(before_close("f()", 1, 2, "x: 1"), (2, 2, "x: 1".into()));
        assert_eq!(before_close("f(a)", 1, 3, "x: 1"), (3, 3, ", x: 1".into()));
        assert_eq!(before_close("f(\n    a,\n)", 1, 10, "x: 1"), (9, 9, " x: 1,".into()));
    }
}
