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
//! A plan is each file's hash before, its new text and its hunks. It's checked with the new
//! texts in place of the old: if the program wouldn't check, nothing is written and the errors
//! say why. A file the formatter owned stays formatted. A plan made earlier (`apply`) is refused
//! if a file it changes no longer has the hash it had.

use crate::query::{Index, Item};
use crate::{Overlay, SourceMap};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
        for (file, new) in self.now {
            let old = self.before[&file].clone();
            if old == new {
                continue;
            }
            let new = crate::on_compiler_thread(|| {
                let formatted = if crate::edit::formatted(&old) {
                    crate::edit::format_text(&new)
                } else {
                    None
                };
                formatted.unwrap_or_else(|| new.clone())
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

/// Checks the program loads, and gives `f` what it found.
fn inspect<T: Send>(
    root: &Path,
    f: impl FnOnce(&Checked, &SourceMap) -> Result<T, Refusal> + Send,
) -> Result<T, Refusal> {
    crate::with_checked(root, f).map_err(|errors| Refusal {
        why: "the program doesn't check as it is: fix its errors first".into(),
        errors: Some(errors),
    })?
}

/// How many uses the compiler records of the function at `path` (after a change, in `texts`).
fn uses_now(root: &Path, texts: &Texts, path: &str) -> Result<usize, Refusal> {
    crate::with_checked_in(root, &texts.overlay(), |checked, sources| {
        let ix = Index::new(checked, sources);
        match ix.one(path) {
            Ok((Item::Fn(f), _)) => Ok(ix.uses_of(f).len()),
            _ => refuse(format!("after the change, `{path}` isn't one function")),
        }
    })
    .map_err(|errors| Refusal {
        why: "the changed program doesn't check".into(),
        errors: Some(errors),
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
}

/// From the first doc comment or attribute line above `span` to the end of its last line.
fn whole_lines(f: &SourceFile, span: Span) -> (u32, u32) {
    let start = f.line_start(crate::query::doc_top(f, f.line_index(span.start)));
    let text = &f.text;
    let end =
        text[span.end as usize..].find('\n').map_or(text.len(), |i| span.end as usize + i + 1);
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
            imports.insert(n.clone(), path);
        }
        Ok(MoveInfo {
            name: name.to_string(),
            path,
            from,
            from_module,
            ranges,
            public,
            keyword,
            imports,
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
    let bytes = text.as_bytes();
    let mut i = after;
    let mut angle = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => angle += 1,
            b'>' => angle -= 1,
            b'(' if angle == 0 => break,
            _ => {}
        }
        i += 1;
    }
    let open = i;
    let mut depth = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open as u32, i as u32));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn calls_of(root: &Path, func: &str) -> Result<Calls, Refusal> {
    inspect(root, |checked, sources| {
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
            for (s, r) in crate::query::rvalues(body) {
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
    })
}

/// The `(` that the `)` at `close` closes.
fn opening(text: &str, close: u32) -> u32 {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut i = close as usize;
    loop {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return i as u32;
                }
            }
            _ => {}
        }
        if i == 0 {
            return close;
        }
        i -= 1;
    }
}

/// Where to add `item` (an argument or a parameter) before the `)` at `close`: after a
/// trailing comma, or with a comma after what's there.
fn before_close(text: &str, open: u32, close: u32, item: &str) -> (u32, u32, String) {
    let inner = &text[open as usize + 1..close as usize];
    let trimmed = inner.trim_end();
    let at = open + 1 + trimmed.len() as u32;
    if trimmed.trim().is_empty() {
        (open + 1, open + 1, item.to_string())
    } else if trimmed.ends_with(',') {
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
    let c = calls_of(root, func)?;
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
            let (s, e, new) =
                before_close(text, opening(text, close), close, &format!("{name}: {v}"));
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
    let c = calls_of(root, func)?;
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

/// Reads a plan `to_json` printed, and checks it against the files now: each must still hash
/// as it did, and the program must check with the plan's texts.
pub fn read_plan(root: &Path, text: &str) -> Result<Plan, Refusal> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("the plan isn't JSON: {e}"))?;
    let bad = |what: &str| Refusal::from(format!("the plan has no {what}"));
    let op = v["op"].as_str().ok_or_else(|| bad("`op`"))?.to_string();
    let mut texts = Texts::new(root);
    for f in v["files"].as_array().ok_or_else(|| bad("`files`"))? {
        let file = f["file"].as_str().ok_or_else(|| bad("file name"))?;
        let (old_hash, new_hash) =
            (f["old_hash"].as_str().unwrap_or(""), f["new_hash"].as_str().unwrap_or(""));
        let new = f["text"].as_str().ok_or_else(|| bad("file text"))?;
        if wrela_abi::check::path_problem(file).is_some() {
            return refuse(format!("{file} isn't in the package"));
        }
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
    for f in &plan.files {
        let now = std::fs::read_to_string(root.join(&f.file)).unwrap_or_default();
        if now != f.old {
            return refuse(format!(
                "{} changed while the plan was made: nothing is written",
                f.file
            ));
        }
    }
    for f in &plan.files {
        crate::write_atomic(&root.join(&f.file), f.new.as_bytes())
            .map_err(|e| format!("can't write {}: {e}", f.file))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
