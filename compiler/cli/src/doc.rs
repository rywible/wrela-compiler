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
struct Entry {
    /// Its module's path, then its name (then a method's name).
    path: Vec<String>,
    /// `fn`, `struct`, `enum`, `trait`, `const`, `type` or `method`.
    kind: &'static str,
    signature: String,
    doc: String,
    /// `file:line` of its declaration.
    at: String,
    /// For a type or a trait: its methods' paths, and the traits it implements.
    methods: Vec<usize>,
    implements: Vec<String>,
}

/// A module: its path, its file's first comment line, and its items.
struct Module {
    path: Vec<String>,
    summary: String,
}

struct Index {
    entries: Vec<Entry>,
    modules: Vec<Module>,
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
    let q: Vec<&str> = query.split("::").collect();
    let exact: Vec<usize> =
        (0..index.entries.len()).filter(|&i| index.entries[i].path == q).collect();
    let matches: Vec<usize> = if exact.is_empty() {
        (0..index.entries.len())
            .filter(|&i| index.entries[i].path.ends_with(&q_owned(&q)))
            .collect()
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
                    first_line(&index.entries[i].doc)
                );
            }
            return ExitCode::SUCCESS;
        }
    }
    // The built-in functions: all of them, or one.
    let builtins = wrela_driver::builtins();
    if q == ["builtins"] {
        println!("the built-in functions (no import; most work on scalars and vectors alike):");
        for (name, arity, method) in &builtins {
            let args = ["x", "y", "z"][..(*arity).min(3)].join(", ");
            if *method {
                println!("  x.{name}({})", ["", "y", "y, z"][arity.saturating_sub(1).min(2)]);
            } else {
                println!("  {name}({args})");
            }
        }
        return ExitCode::SUCCESS;
    }
    if q.len() == 1
        && let Some((name, arity, _)) = builtins.iter().find(|b| b.0 == q[0])
    {
        println!(
            "{name}: a built-in function of {arity} argument{} (language.md §4; `wrela doc builtins` lists them all)",
            wrela_diag::plural(*arity)
        );
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
            println!("  {:<8} {:<28} {}", e.kind, e.path[q.len()], first_line(&e.doc));
        }
        return ExitCode::SUCCESS;
    }
    eprintln!("error: nothing public is named `{query}`");
    let last = q.last().copied().unwrap_or_default().to_lowercase();
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

fn q_owned(q: &[&str]) -> Vec<String> {
    q.iter().map(|s| s.to_string()).collect()
}

fn first_line(doc: &str) -> &str {
    doc.lines().next().unwrap_or("")
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
                println!("      {}", first_line(&m.doc));
            }
        }
    }
}

/// A parsed file: its module path, display name, text, items and doc comments.
struct Source {
    module: Vec<String>,
    name: String,
    text: String,
    parsed: wrela_syntax::Parsed,
}

fn build_index(dir: Option<&Path>) -> Result<Index, String> {
    let mut sources = Vec::new();
    for (name, text) in wrela_driver::STD_SOURCES {
        let module: Vec<String> = name.split("::").map(String::from).collect();
        let file = format!("std/{}.wrela", module[1]);
        sources.push((module, file, text.to_string()));
    }
    if let Some(dir) = dir {
        let files = wrela_driver::package::find_files(dir).map_err(|errs| {
            errs.iter().map(|e| format!("{}: {}", e.path, e.message)).collect::<Vec<_>>().join("; ")
        })?;
        for f in files {
            let text = std::fs::read_to_string(&f.path)
                .map_err(|e| format!("can't read `{}`: {e}", f.path.display()))?;
            sources.push((f.module, f.display, text));
        }
        // The packages it depends on, each module under its package's name (`engine::creature`).
        if let Ok(text) = std::fs::read_to_string(dir.join("wrela.toml"))
            && let Ok(m) = wrela_driver::manifest::parse(FileId(0), &text)
        {
            for dep in m.deps {
                let Ok(files) = wrela_driver::package::find_files(&dir.join(&dep.path)) else {
                    continue;
                };
                for f in files {
                    let Ok(text) = std::fs::read_to_string(&f.path) else { continue };
                    let mut module = vec![dep.name.clone()];
                    module.extend(f.module);
                    sources.push((module, format!("[{}] {}", dep.name, f.display), text));
                }
            }
        }
    }
    let sources: Vec<Source> = sources
        .into_iter()
        .enumerate()
        .map(|(i, (module, name, text))| {
            let parsed = wrela_syntax::parse(FileId(i as u32), &text);
            Source { module, name, text, parsed }
        })
        .collect();
    let mut index = Index { entries: Vec::new(), modules: Vec::new() };
    for s in &sources {
        index.modules.push(Module { path: s.module.clone(), summary: summary(&s.text) });
        for item in &s.parsed.file.items {
            add_item(&mut index, s, item);
        }
    }
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
                        let ImplMemberKind::Fn(f) = &m.kind else { continue };
                        if !documented(m.vis.as_ref(), s) {
                            continue;
                        }
                        let mut path = index.entries[owner].path.clone();
                        path.push(f.name.name.clone());
                        let signature = fn_signature(s, &m.attrs, m.vis.as_ref(), f);
                        let e = entry(s, path, "method", signature, m.span);
                        index.entries.push(e);
                        let id = index.entries.len() - 1;
                        index.entries[owner].methods.push(id);
                    }
                }
            }
        }
    }
    Ok(index)
}

/// A type's name as an impl names it: `Arena<T>` is `Arena`.
fn type_name(t: &ast::TypeExpr) -> Option<String> {
    match &t.kind {
        ast::TypeExprKind::Path(p) => p.segments.last().map(|s| s.ident.name.clone()),
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
                    TraitMemberKind::Type { .. } => None,
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
fn one_line(sig: &str) -> String {
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
    let before = s.parsed.comments.iter().rev().filter(|c| (c.span.end as usize) <= start as usize);
    for c in before {
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

/// A file's first `//` comment line: what the module is for.
fn summary(text: &str) -> String {
    text.lines()
        .next()
        .and_then(|l| l.strip_prefix("//"))
        .map(|l| l.trim().to_string())
        .unwrap_or_default()
}
