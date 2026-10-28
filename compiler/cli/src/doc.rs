//! `wrela doc <item> [<package-dir>]`: an item's signature, doc comment and examples, for every
//! public item in std and in the package (the package in the current directory if none is
//! given). An agent or a person finds what exists without an editor's help.
//!
//! `<item>` is a path, as a program names it (`std::arena::Arena`, `std::arena::Arena::push`,
//! `main::Grid`), or its last parts alone (`Arena::push`, `push`): one match is printed, several
//! are listed. A module (`std::arena`) lists its items; `std` lists std's modules. Only parsing
//! is done, no checking, so it's fast: well under 100 ms.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_diag::{FileId, Span};
use wrela_syntax::ast::{self, ImplMemberKind, ItemKind, TraitMemberKind};

/// One documented item.
pub(crate) struct Entry {
    /// Its module's path, then its name (then a method's name).
    pub(crate) path: Vec<String>,
    /// `fn`, `struct`, `enum`, `trait`, `const`, `type` or `method`.
    pub(crate) kind: &'static str,
    pub(crate) signature: String,
    pub(crate) doc: String,
    /// `file:line` of its declaration, as a diagnostic names the file.
    pub(crate) at: String,
    /// Its declaration's file in the repository (`compiler/std/io.wrela`) and line.
    pub(crate) source: String,
    pub(crate) line: usize,
    /// For a type or a trait: its methods' paths, and the traits it implements.
    pub(crate) methods: Vec<usize>,
    pub(crate) implements: Vec<String>,
}

/// A module: its path, its file's first comment line, and the whole comment its file opens with.
pub(crate) struct Module {
    pub(crate) path: Vec<String>,
    pub(crate) summary: String,
    pub(crate) intro: String,
}

pub(crate) struct Index {
    pub(crate) entries: Vec<Entry>,
    pub(crate) modules: Vec<Module>,
}

pub fn run(args: &[String]) -> ExitCode {
    let (query, dir) = match args {
        [q] => (q, None),
        [q, d] if !d.starts_with('-') => (q, Some(PathBuf::from(d))),
        _ => return crate::usage(),
    };
    let dir = dir.or_else(|| Path::new("main.wrela").is_file().then(|| PathBuf::from(".")));
    let index = match build_index(dir.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    // Another name of a built-in type is its page's: `u8` is on `u32`'s, `run` is `[T]`.
    let query = crate::builtin_types::resolve_alias(query).unwrap_or(query);
    let q: Vec<String> = query.split("::").map(String::from).collect();
    let exact: Vec<usize> =
        (0..index.entries.len()).filter(|&i| index.entries[i].path == q).collect();
    // A built-in function's name alone is the built-in, before any method of std's it ends.
    let builtins = wrela_driver::builtins();
    if exact.is_empty()
        && q.len() == 1
        && let Some((name, arity, _)) = builtins.iter().find(|b| b.0 == q[0])
    {
        println!(
            "{name}: a built-in function of {arity} argument{} (language.md §4; `wrela doc builtins` lists them all)",
            wrela_diag::plural(*arity)
        );
        return ExitCode::SUCCESS;
    }
    let matches: Vec<usize> = if exact.is_empty() {
        (0..index.entries.len()).filter(|&i| index.entries[i].path.ends_with(&q)).collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [one] => {
            print_entry(&index, *one);
            return ExitCode::SUCCESS;
        }
        [] => {}
        many => {
            println!("{} items are named `{query}`:", many.len());
            for &i in many {
                println!(
                    "  {:<40} {}",
                    index.entries[i].path.join("::"),
                    first_sentence(&index.entries[i].doc)
                );
            }
            return ExitCode::SUCCESS;
        }
    }
    // The built-in functions, all of them.
    if q == ["builtins"] {
        println!("the built-in functions (no import; most work on scalars and vectors alike):");
        for &(name, arity, method) in &builtins {
            println!("  {}", builtin_call(name, arity, method));
        }
        println!("\nthe built-in types (`wrela doc <type>` shows one):");
        for (name, summary) in crate::builtin_types::summaries() {
            println!("  {name:<10} {summary}");
        }
        return ExitCode::SUCCESS;
    }
    // A module, or std's list of them.
    if q == ["std"] {
        println!("std's modules:");
        for m in index.modules.iter().filter(|m| m.path[0] == "std") {
            println!("  {:<20} {}", m.path.join("::"), m.summary);
        }
        println!("  (and the built-in functions: `wrela doc builtins`)");
        return ExitCode::SUCCESS;
    }
    if let Some(m) = index.modules.iter().find(|m| m.path == q) {
        println!("module {}: {}\n", m.path.join("::"), m.summary);
        for e in index
            .entries
            .iter()
            .filter(|e| e.path.len() == q.len() + 1 && e.path.starts_with(&m.path))
        {
            println!("  {:<8} {:<28} {}", e.kind, e.path[q.len()], first_sentence(&e.doc));
        }
        return ExitCode::SUCCESS;
    }
    eprintln!("error: nothing public is named `{query}`");
    let last = q.last().map_or(String::new(), |l| l.to_lowercase());
    let near: Vec<&Entry> = index
        .entries
        .iter()
        .filter(|e| e.path.last().is_some_and(|n| n.to_lowercase().contains(&last)))
        .take(10)
        .collect();
    if !near.is_empty() {
        eprintln!("  = help: these have names like it:");
        for e in near {
            eprintln!("      {}", e.path.join("::"));
        }
    }
    ExitCode::from(1)
}

/// A built-in function as a call: `name(x, y)`, or a method's `x.name(y)`.
pub(crate) fn builtin_call(name: &str, arity: usize, method: bool) -> String {
    if method {
        format!("x.{name}({})", ["", "y", "y, z"][arity.saturating_sub(1).min(2)])
    } else {
        format!("{name}({})", ["x", "y", "z"][..arity.min(3)].join(", "))
    }
}

pub(crate) fn first_sentence(doc: &str) -> String {
    let lines: Vec<&str> = doc.lines().map(str::trim).take_while(|l| !l.is_empty()).collect();
    let text = lines.join(" ");
    match text.find(". ") {
        Some(i) => text[..=i].to_string(),
        None => text,
    }
}

fn print_entry(index: &Index, i: usize) {
    let e = &index.entries[i];
    println!("{} ({}, {})\n", e.path.join("::"), e.kind, e.at);
    println!("{}\n", e.signature);
    if !e.doc.is_empty() {
        println!("{}\n", e.doc);
    }
    if !e.implements.is_empty() {
        println!("implements: {}\n", e.implements.join(", "));
    }
    if !e.methods.is_empty() {
        println!("methods:");
        for &m in &e.methods {
            let m = &index.entries[m];
            println!("  {}", one_line(&m.signature));
            if !m.doc.is_empty() {
                println!("      {}", first_sentence(&m.doc));
            }
        }
    }
}

/// A parsed file: its module path, display name, file in the repository, text, items and doc
/// comments.
struct Source {
    module: Vec<String>,
    name: String,
    file: String,
    text: String,
    parsed: wrela_syntax::Parsed,
}

/// A file to index: its module path, display name, file in the repository and text.
pub(crate) type SourceFile = (Vec<String>, String, String, String);

/// std's files, as the compiler holds them.
pub(crate) fn std_sources() -> Vec<SourceFile> {
    wrela_driver::STD_SOURCES
        .iter()
        .map(|(name, text)| {
            let module: Vec<String> = name.split("::").map(String::from).collect();
            // `std::tool::files` is std/tool/files.wrela.
            let path = module[1..].join("/");
            let display = format!("std/{path}.wrela");
            let file = format!("compiler/std/{path}.wrela");
            (module, display, file, text.to_string())
        })
        .collect()
}

/// The files of the package at `dir`, each module under `prefix` (`engine::creature`) if one is
/// given, and named as `[prefix] file` then.
pub(crate) fn package_sources(dir: &Path, prefix: Option<&str>) -> Result<Vec<SourceFile>, String> {
    let here = std::env::current_dir().unwrap_or_default();
    package_sources_in(&wrela_driver::reach::Reach::anywhere(), &here, dir, prefix)
}

/// [`package_sources`], read through `reach`, each file named by its path from `base`.
pub(crate) fn package_sources_in(
    reach: &wrela_driver::reach::Reach,
    base: &Path,
    dir: &Path,
    prefix: Option<&str>,
) -> Result<Vec<SourceFile>, String> {
    let files = wrela_driver::package::find_files_in(reach, dir).map_err(|errs| {
        errs.iter().map(|e| format!("{}: {}", e.path, e.message)).collect::<Vec<_>>().join("; ")
    })?;
    let mut sources = Vec::new();
    for f in files {
        let text = reach
            .read_to_string(&f.path)
            .map_err(|e| format!("can't read `{}`: {e}", f.path.display()))?;
        let file = repository_path(base, &f.path);
        match prefix {
            Some(p) => {
                let mut module = vec![p.to_string()];
                module.extend(f.module);
                sources.push((module, format!("[{p}] {}", f.display), file, text));
            }
            None => sources.push((f.module, f.display, file, text)),
        }
    }
    Ok(sources)
}

/// `path` as the repository names it: relative to `base` (the current directory, for `wrela
/// doc`), `/`-separated.
fn repository_path(base: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    let parts: Vec<String> = relative
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

fn build_index(dir: Option<&Path>) -> Result<Index, String> {
    let mut sources = std_sources();
    if let Some(dir) = dir {
        sources.extend(package_sources(dir, None)?);
        // The packages it depends on, each module under its package's name (`engine::creature`).
        if let Ok(deps) = wrela_driver::workspace::dependency_dirs(dir) {
            for (name, dep) in deps {
                if let Ok(files) = package_sources(&dep, Some(&name)) {
                    sources.extend(files);
                }
            }
        }
    }
    Ok(index_of(sources))
}

/// The documented items of `sources`.
pub(crate) fn index_of(sources: Vec<SourceFile>) -> Index {
    let sources: Vec<Source> = sources
        .into_iter()
        .enumerate()
        .map(|(i, (module, name, file, text))| {
            let parsed = wrela_syntax::parse(FileId(i as u32), &text);
            Source { module, name, file, text, parsed }
        })
        .collect();
    let mut index = Index { entries: Vec::new(), modules: Vec::new() };
    for s in &sources {
        let intro = intro(&s.text);
        index.modules.push(Module {
            path: s.module.clone(),
            summary: first_sentence(&intro),
            intro,
        });
        for item in &s.parsed.file.items {
            add_item(&mut index, s, item);
        }
    }
    crate::builtin_types::add(&mut index);
    // Each type's and trait's methods, and the traits a type implements, wherever the impl is.
    for s in &sources {
        for item in &s.parsed.file.items {
            let ItemKind::Impl(imp) = &item.kind else { continue };
            let Some(ty) = type_name(&imp.self_ty) else { continue };
            let Some(owner) = owner_of(&index, &ty, &s.module) else { continue };
            match &imp.trait_ {
                Some(t) => {
                    let t = text(s, t.span);
                    if !index.entries[owner].implements.contains(&t) {
                        index.entries[owner].implements.push(t);
                    }
                }
                None => {
                    for m in &imp.members {
                        if !documented(m.vis.as_ref(), s) {
                            continue;
                        }
                        // A method, or an associated constant as it's written.
                        let (name, kind, signature) = match &m.kind {
                            ImplMemberKind::Fn(f) => (
                                &f.name.name,
                                "method",
                                fn_signature(s, &m.attrs, m.vis.as_ref(), f),
                            ),
                            ImplMemberKind::Const(c) => (&c.name.name, "const", text(s, m.span)),
                            ImplMemberKind::Type { .. } => continue,
                        };
                        let mut path = index.entries[owner].path.clone();
                        path.push(name.clone());
                        let e = entry(s, path, kind, signature, m.span);
                        index.entries.push(e);
                        let id = index.entries.len() - 1;
                        index.entries[owner].methods.push(id);
                    }
                }
            }
        }
    }
    index
}

/// A type's name as an impl names it: `Arena<T>` is `Arena`; std's impls of a run and of an
/// array are of the built-in types `[T]` and `[T; N]` (`crate::builtin_types`).
fn type_name(t: &ast::TypeExpr) -> Option<String> {
    match &t.kind {
        ast::TypeExprKind::Path(p) => p.segments.last().map(|s| s.ident.name.clone()),
        ast::TypeExprKind::Array(_, None) => Some("[T]".into()),
        ast::TypeExprKind::Array(_, Some(_)) => Some("[T; N]".into()),
        _ => None,
    }
}

/// The type or trait entry an impl in `module` is for: one in the same module, else the only
/// one with that name.
fn owner_of(index: &Index, name: &str, module: &[String]) -> Option<usize> {
    let is_type = |e: &Entry| matches!(e.kind, "struct" | "enum" | "trait" | "type");
    let named: Vec<usize> = (0..index.entries.len())
        .filter(|&i| {
            is_type(&index.entries[i]) && index.entries[i].path.last().is_some_and(|n| n == name)
        })
        .collect();
    named
        .iter()
        .copied()
        .find(|&i| index.entries[i].path[..index.entries[i].path.len() - 1] == *module)
        .or(if named.len() == 1 { Some(named[0]) } else { None })
}

/// Whether an item with visibility `vis` in `s` gets a page: `pub` ones, and `pub(package)`
/// ones of the package being documented (std's are its own business).
fn documented(vis: Option<&ast::Vis>, s: &Source) -> bool {
    vis.is_some_and(|v| !v.package || s.module.first().is_none_or(|m| m != "std"))
}

fn add_item(index: &mut Index, s: &Source, item: &ast::Item) {
    if !documented(item.vis.as_ref(), s) {
        return;
    }
    let Some(name) = item.kind.name() else { return };
    let mut path = s.module.clone();
    path.push(name.name.clone());
    let (kind, signature) = match &item.kind {
        ItemKind::Fn(f) => ("fn", fn_signature(s, &item.attrs, item.vis.as_ref(), f)),
        ItemKind::Struct(st) => ("struct", struct_signature(s, item, st)),
        ItemKind::Enum(_) => ("enum", text(s, item.span)),
        ItemKind::TypeAlias(_) => ("type", text(s, item.span)),
        ItemKind::TraitSet(_) => ("trait set", text(s, item.span)),
        ItemKind::Const(_) => {
            let t = text(s, item.span);
            let short = match t.lines().count() {
                0 | 1 => t,
                _ => format!("{} ...", t.lines().next().unwrap_or("")),
            };
            ("const", short)
        }
        ItemKind::Trait(t) => {
            // The trait with its default methods' bodies left out.
            let holes: Vec<Span> = t
                .members
                .iter()
                .filter_map(|m| match &m.kind {
                    TraitMemberKind::Fn(f) => f.body.as_ref().map(|b| b.span),
                    TraitMemberKind::Type { .. } | TraitMemberKind::Const(_) => None,
                })
                .collect();
            ("trait", elide(s, item.span, &holes))
        }
        ItemKind::Impl(_) | ItemKind::Use(_) | ItemKind::Error(_) => return,
    };
    let e = entry(s, path.clone(), kind, signature, item.span);
    index.entries.push(e);
    // A trait's methods are documented under it.
    if let ItemKind::Trait(t) = &item.kind {
        let owner = index.entries.len() - 1;
        for m in &t.members {
            let TraitMemberKind::Fn(f) = &m.kind else { continue };
            let mut p = path.clone();
            p.push(f.name.name.clone());
            let signature = fn_signature(s, &m.attrs, None, f);
            index.entries.push(entry(s, p, "method", signature, m.span));
            let id = index.entries.len() - 1;
            index.entries[owner].methods.push(id);
        }
    }
}

fn entry(
    s: &Source,
    path: Vec<String>,
    kind: &'static str,
    signature: String,
    span: Span,
) -> Entry {
    let line = 1 + s.text[..span.start as usize].matches('\n').count();
    Entry {
        path,
        kind,
        signature,
        doc: doc_before(s, span.start),
        at: format!("{}:{line}", s.name),
        source: s.file.clone(),
        line,
        methods: Vec::new(),
        implements: Vec::new(),
    }
}

/// A struct's declaration with its public fields only: the others are how it's built.
fn struct_signature(s: &Source, item: &ast::Item, st: &ast::StructDecl) -> String {
    let all = text(s, item.span);
    let Some(open) = all.find('{') else { return all };
    let mut out = all[..open].trim_end().to_string();
    // Each public field with the doc comment above it.
    let public: Vec<(Vec<&str>, String)> = st
        .fields
        .iter()
        .filter(|f| f.vis.is_some())
        .map(|f| (doc_comments(s, f.span.start), text(s, f.span)))
        .collect();
    let hidden = st.fields.len() - public.len();
    if public.is_empty() && hidden == 0 {
        return all;
    }
    out.push_str(" {\n");
    for (doc, f) in &public {
        for d in doc {
            out.push_str(&format!("    {}\n", d.trim_end()));
        }
        out.push_str(&format!("    {f},\n"));
    }
    if hidden > 0 {
        out.push_str("    // and private fields\n");
    }
    out.push('}');
    out
}

/// A signature on one line.
pub(crate) fn one_line(sig: &str) -> String {
    let joined: Vec<&str> = sig.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    joined.join(" ").replace("( ", "(").replace(", )", ")")
}

fn text(s: &Source, span: Span) -> String {
    s.text[span.start as usize..span.end as usize].to_string()
}

/// A function's attributes, `pub`, and its signature: what calls it need.
fn fn_signature(
    s: &Source,
    attrs: &[ast::Attribute],
    vis: Option<&ast::Vis>,
    f: &ast::FnDecl,
) -> String {
    let mut out = String::new();
    for a in attrs {
        out.push_str(&text(s, a.span));
        out.push('\n');
    }
    if let Some(v) = vis {
        out.push_str(&text(s, v.span));
        out.push(' ');
    }
    out.push_str(&text(s, f.sig_span));
    out
}

/// The text of `span` with each of `holes` (a body) written `{ ... }`.
fn elide(s: &Source, span: Span, holes: &[Span]) -> String {
    let mut out = String::new();
    let mut pos = span.start as usize;
    for h in holes {
        out.push_str(&s.text[pos..h.start as usize]);
        out.push_str("{ ... }");
        pos = h.end as usize;
    }
    out.push_str(&s.text[pos..span.end as usize]);
    out
}

/// The `///` comments just above `start` (attributes may come between), as they're written.
fn doc_comments(s: &Source, start: u32) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut end = start as usize;
    let before = s.parsed.comments.partition_point(|c| c.span.end <= start);
    for c in s.parsed.comments[..before].iter().rev() {
        let between = &s.text[c.span.end as usize..end];
        let gap_ok = between.trim().is_empty() && between.matches('\n').count() <= 1;
        if !c.doc || !c.own_line || !gap_ok {
            break;
        }
        lines.push(c.text.as_str());
        end = c.span.start as usize;
    }
    lines.reverse();
    lines
}

/// The `///` lines just above `start` (attributes may come between), without their `///`.
fn doc_before(s: &Source, start: u32) -> String {
    let lines: Vec<&str> = doc_comments(s, start)
        .into_iter()
        .map(|c| {
            let t = c.trim_start_matches("///");
            t.strip_prefix(' ').unwrap_or(t)
        })
        .collect();
    lines.join("\n")
}

/// The `//` comment a file opens with, line for line, as Markdown: what the module is for, at
/// length. Its lists and indented lines stay as written.
fn intro(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let Some(l) = line.strip_prefix("//") else { break };
        if l.starts_with('/') {
            break;
        }
        let l = l.strip_prefix(' ').unwrap_or(l);
        if l.trim().is_empty() {
            out.push_str("\n\n");
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(l.trim_end());
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_keep_only_adjacent_preceding_comments() {
        let text = concat!(
            "pub fn before() {}\n\n",
            "/// First line.\n/// Second line.\npub fn documented() {}\n\n",
            "/// On an attribute.\n@test\npub fn attributed() {}\n\n",
            "/// Detached.\n\npub fn detached() {}\n\n",
            "/// Interrupted.\n// An ordinary comment.\npub fn interrupted() {}\n\n",
            "/// Later.\npub fn later() {}\n",
        );
        let s = Source {
            module: vec!["test".into()],
            name: "test.wrela".into(),
            file: "test.wrela".into(),
            text: text.into(),
            parsed: wrela_syntax::parse(FileId(0), text),
        };
        assert!(!wrela_diag::has_errors(&s.parsed.diagnostics));
        for (at, expected) in [
            ("pub fn before", ""),
            ("pub fn documented", "First line.\nSecond line."),
            ("@test", "On an attribute."),
            ("pub fn detached", ""),
            ("pub fn interrupted", ""),
            ("pub fn later", "Later."),
        ] {
            assert_eq!(doc_before(&s, text.find(at).expect("item") as u32), expected, "{at}");
        }
    }

    #[test]
    fn an_intro_keeps_its_lines_and_a_summary_is_its_first_sentence() {
        let text = "// What it is: one thing. And more\n// on two lines.\n//\n// - a list\n// - of two\n//   indented\n\nfn x() {}\n";
        assert_eq!(
            intro(text),
            "What it is: one thing. And more\non two lines.\n\n- a list\n- of two\n  indented"
        );
        assert_eq!(first_sentence(&intro(text)), "What it is: one thing.");
        assert_eq!(first_sentence("One line\nthat wraps.\n\nNext."), "One line that wraps.");
    }
}
