//! `wrela doc --export <dir> [--commit <sha>] [<package-dir>...]`: the reference docs as files
//! that people and agents read alike. The language's spec, split by section, and a page per
//! module of std and of each package given, in Markdown; `index.json`, every item and every
//! section of prose with its page, anchor and source; `llms.txt`, a map of the pages, and
//! `llms-full.txt`, all of them in one file, with each group's pages in one file of the group's
//! (`spec/llms-full.txt`, `std/llms-full.txt`), small enough for an agent to read whole. The same
//! input gives the same bytes.
//!
//! The items come from the index `wrela doc` reads (`doc.rs`), so a page says what a lookup
//! says. The spec is the one this compiler was built with. A package's items link to their
//! source in the repository given, at its commit; std's link to the toolchain's, which for a
//! release is its own repository at the revision it was built from (`index.json`'s `toolchain`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::doc::{self, Entry, Index};

pub(crate) const LANGUAGE: &str = include_str!("../../../spec/language.md");
const LEXICAL: &str = include_str!("../../../spec/lexical.md");
const GRAMMAR: &str = include_str!("../../../spec/grammar.ebnf");
const REPOSITORY: &str = "https://github.com/rywible/wrela";

/// A page: where it goes, its title, its one-line summary, what it is and its Markdown.
struct Page {
    path: String,
    title: String,
    summary: String,
    kind: &'static str,
    text: String,
}

/// A section of prose: its page, its anchor there, its headings from the page's down, and its
/// text.
struct Section {
    page: String,
    anchor: String,
    headings: Vec<String>,
    text: String,
}

pub fn run(args: &[String]) -> ExitCode {
    let mut out: Option<PathBuf> = None;
    let mut commit: Option<String> = None;
    let mut packages: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--export" if i + 1 < args.len() => {
                out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--commit" if i + 1 < args.len() => {
                commit = Some(args[i + 1].clone());
                i += 2;
            }
            a if !a.starts_with('-') => {
                packages.push(PathBuf::from(a));
                i += 1;
            }
            _ => return crate::usage(),
        }
    }
    let Some(out) = out else { return crate::usage() };
    if let Some(c) = &commit
        && !(c.len() >= 7 && c.len() <= 40 && c.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        eprintln!("error: `--commit` is a commit's hexadecimal name, not `{c}`");
        return ExitCode::from(2);
    }
    match export(&out, commit.as_deref(), &packages) {
        Ok(n) => {
            println!(
                "wrote {n} pages, index.json, llms.txt and llms-full.txt (whole and by group) to {}",
                out.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Writes the docs of std and `packages` into `out`, which must be empty or missing. Returns how
/// many pages.
fn export(out: &Path, commit: Option<&str>, packages: &[PathBuf]) -> Result<usize, String> {
    if out.exists() && out.read_dir().map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("`{}` isn't empty: export into a new directory", out.display()));
    }
    let here = std::env::current_dir().unwrap_or_default();
    let reach = wrela_driver::reach::Reach::anywhere();
    let (pages, files) = files_of(commit, REPOSITORY, packages, &reach, &here)?;
    for (path, bytes) in &files {
        let path = out.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, bytes)
            .map_err(|e| format!("can't write `{}`: {e}", path.display()))?;
    }
    Ok(pages)
}

/// Files by their paths, with their bytes.
pub(crate) type Files = Vec<(String, Vec<u8>)>;

/// The docs of std and `packages`, as files by their paths in the export: how many pages, and
/// the files. `repository` is where the source is, and `commit` its revision (`main` if none);
/// a package's files are named by their paths from `base`. Files are read through `reach`.
/// The same input gives the same bytes.
pub(crate) fn files_of(
    commit: Option<&str>,
    repository: &str,
    packages: &[PathBuf],
    reach: &wrela_driver::reach::Reach,
    base: &Path,
) -> Result<(usize, Files), String> {
    let mut sources = doc::std_sources();
    let mut names = Vec::new();
    for dir in packages {
        let manifest = reach
            .read_to_string(&dir.join("wrela.toml"))
            .map_err(|e| format!("`{}` isn't a package: {e}", dir.display()))?;
        let name = package_name(&manifest)
            .ok_or_else(|| format!("`{}`'s wrela.toml has no valid package name", dir.display()))?;
        if name.eq_ignore_ascii_case("std") || name.eq_ignore_ascii_case("spec") {
            return Err(format!("`{name}` is reserved by the toolchain's reference docs"));
        }
        if names.iter().any(|prior: &String| prior.eq_ignore_ascii_case(&name)) {
            return Err(format!(
                "two packages have the name `{name}` on a case-insensitive filesystem"
            ));
        }
        sources.extend(doc::package_sources_in(reach, base, dir, Some(&name))?);
        names.push(name);
    }
    let index = doc::index_of(sources);
    let mut pages = spec_pages();
    let mut sections: Vec<Section> = Vec::new();
    for p in &pages {
        sections.extend(sections_of(p));
    }
    let source = format!("{repository}/blob/{}/", commit.unwrap_or("main"));
    let (std_repository, std_commit) = toolchain_source(repository, commit, crate::pin::REVISION);
    let std_source = format!("{std_repository}/blob/{}/", std_commit.unwrap_or("main"));
    let mut items: Vec<Value> = Vec::new();
    for m in 0..index.modules.len() {
        let first = index.modules[m].path.first();
        let in_std = first.is_some_and(|p| p == crate::builtin_types::MODULE || p == "std");
        let (page, mut records) =
            module_page(&index, m, if in_std { &std_source } else { &source });
        if page.text.is_empty() {
            continue;
        }
        items.append(&mut records);
        pages.push(page);
    }
    // The spec in its own order, then the modules by path.
    let spec = pages.iter().filter(|p| p.kind == "spec").count();
    pages[spec..].sort_by(|a, b| a.path.cmp(&b.path));
    let places = spec_places(&pages);
    for p in &mut pages {
        p.text = link_sections(&p.text, &p.path, &places);
    }
    let version = env!("CARGO_PKG_VERSION");
    let index_json = json!({
        "version": version,
        "commit": commit,
        "repository": repository,
        "toolchain": { "version": version, "repository": std_repository, "commit": std_commit },
        "pages": pages.iter().map(|p| json!({
            "path": p.path, "title": p.title, "summary": p.summary, "kind": p.kind,
        })).collect::<Vec<_>>(),
        "items": items,
        "sections": sections.iter().map(|s| json!({
            "page": s.page, "anchor": s.anchor, "headings": s.headings, "text": s.text,
        })).collect::<Vec<_>>(),
    });
    let mut files: Vec<(String, Vec<u8>)> =
        pages.iter().map(|p| (p.path.clone(), p.text.clone().into_bytes())).collect();
    let json = serde_json::to_string_pretty(&index_json).map_err(|e| e.to_string())? + "\n";
    files.push(("index.json".into(), json.into_bytes()));
    let groups = groups_of(&pages);
    let mut sizes = Vec::new();
    for (group, members) in &groups {
        let text = full(members);
        sizes.push((*group, text.len()));
        files.push((format!("{group}/llms-full.txt"), text.into_bytes()));
    }
    let all = full(&pages.iter().collect::<Vec<_>>());
    let map = llms(&groups, &sizes, all.len(), version, commit, repository);
    files.push(("llms.txt".into(), map.into_bytes()));
    files.push(("llms-full.txt".into(), all.into_bytes()));
    unique_paths(&files)?;
    Ok((pages.len(), files))
}

/// Refuse output names that would overwrite each other on a case-insensitive filesystem.
fn unique_paths(files: &Files) -> Result<(), String> {
    let mut paths = BTreeSet::new();
    for (path, _) in files {
        if !paths.insert(path.to_ascii_lowercase()) {
            return Err(format!(
                "two reference files use `{path}` on a case-insensitive filesystem"
            ));
        }
    }
    Ok(())
}

/// Where std's and the spec's source is, and its revision: a release's toolchain repository at
/// the revision it was built from (`release`), else the packages' (`repository` at `commit`),
/// which a development build's toolchain is built beside.
fn toolchain_source<'a>(
    repository: &'a str,
    commit: Option<&'a str>,
    release: Option<&'a str>,
) -> (&'a str, Option<&'a str>) {
    match release {
        Some(revision) => (crate::pin::REPOSITORY, Some(revision)),
        None => (repository, commit),
    }
}

/// The pages by group, in the order a reader learns them: the spec, then std, then the packages
/// by name. A page's group is `spec`, or the package its path starts with.
fn groups_of(pages: &[Page]) -> Vec<(&str, Vec<&Page>)> {
    let mut groups: BTreeMap<&str, Vec<&Page>> = BTreeMap::new();
    for p in pages {
        let group = if p.kind == "spec" { "spec" } else { p.path.split('/').next().unwrap_or("") };
        groups.entry(group).or_default().push(p);
    }
    let mut order: Vec<(&str, Vec<&Page>)> = groups.into_iter().collect();
    order.sort_by_key(|(g, _)| (*g != "spec", *g != "std", *g));
    order
}

/// Pages in one file, each after a comment that names its path.
fn full(pages: &[&Page]) -> String {
    let mut text = String::new();
    for p in pages {
        text.push_str(&format!("<!-- {} -->\n\n", p.path));
        text.push_str(&p.text);
        text.push('\n');
    }
    text
}

/// A size in bytes as a reader reads it: `254 kB`, `1.5 MB`.
fn size_of(bytes: usize) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{} kB", bytes.div_ceil(1000))
    }
}

/// The `name` of a manifest's `[package]`.
fn package_name(manifest: &str) -> Option<String> {
    wrela_driver::manifest::parse(wrela_diag::FileId(0), manifest).ok()?.name.map(|(name, _)| name)
}

/// An anchor for `heading`: lowercase letters, digits and `-`, as Markdown renderers make them.
pub(crate) fn slug(heading: &str) -> String {
    let mut out = String::new();
    for c in heading.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if (c == ' ' || c == '-' || c == '_' || c == ':' || c == '.') && !out.ends_with('-')
        {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Gives each heading of a page its own anchor: a repeat gets `-2`, `-3` and so on.
#[derive(Default)]
struct Anchors(BTreeMap<String, usize>);

impl Anchors {
    fn of(&mut self, heading: &str) -> String {
        let base = slug(heading);
        let n = self.0.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 { base } else { format!("{base}-{n}") }
    }
}

/// The spec's pages: the language's `##` sections, each a page under `spec/language/` (its
/// introduction is `index.md`), the lexical structure and the grammar.
fn spec_pages() -> Vec<Page> {
    let mut pages = Vec::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut title = String::new();
    let mut body = String::new();
    for line in LANGUAGE.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            parts.push((std::mem::take(&mut title), std::mem::take(&mut body)));
            title = h.trim().to_string();
            continue;
        }
        if let Some(h) = line.strip_prefix("# ")
            && parts.is_empty()
            && title.is_empty()
        {
            title = h.trim().to_string();
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    parts.push((title, body));
    for (i, (title, body)) in parts.into_iter().enumerate() {
        let path = if i == 0 {
            "spec/language/index.md".to_string()
        } else {
            format!("spec/language/{}.md", slug(&title))
        };
        // A page's own sections move up a level: `###` becomes `##`.
        let body: String = body
            .lines()
            .map(|l| if l.starts_with("###") { &l[1..] } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.trim().to_string();
        let summary = first_sentence(&body);
        pages.push(Page {
            path,
            text: format!("# {title}\n\n{body}\n"),
            title,
            summary,
            kind: "spec",
        });
    }
    let lexical_title =
        LEXICAL.lines().next().and_then(|l| l.strip_prefix("# ")).unwrap_or("Lexical structure");
    let lexical_body = LEXICAL.split_once('\n').map_or("", |(_, b)| b).trim();
    pages.push(Page {
        path: "spec/lexical.md".to_string(),
        title: lexical_title.to_string(),
        summary: first_sentence(lexical_body),
        kind: "spec",
        text: format!("# {lexical_title}\n\n{lexical_body}\n"),
    });
    pages.push(Page {
        path: "spec/grammar.md".to_string(),
        title: "Wrela's grammar".to_string(),
        summary: "All of Wrela's syntax, in EBNF over the tokens of its lexical structure."
            .to_string(),
        kind: "spec",
        text: format!("# Wrela's grammar\n\n```ebnf\n{}\n```\n", GRAMMAR.trim_end()),
    });
    pages
}

/// A Markdown body's first sentence of prose, on one line, for a summary: the first one of a
/// paragraph that is not a table, code, a heading, a rule, a lead-in (`Goals:`) or an aside in
/// brackets. A list item reads as its text.
fn first_sentence(body: &str) -> String {
    let mut fenced = false;
    let mut paragraphs: Vec<Vec<&str>> = vec![Vec::new()];
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            fenced = !fenced;
            paragraphs.push(Vec::new());
            continue;
        }
        let item = t.starts_with("- ") || t.starts_with("* ");
        let other = t.starts_with('#') || t.starts_with('|') || t.starts_with('>') || t == "---";
        if fenced || t.is_empty() || other || item {
            paragraphs.push(Vec::new());
        }
        if !fenced && !t.is_empty() && !other {
            paragraphs
                .last_mut()
                .expect("one")
                .push(t.trim_start_matches("- ").trim_start_matches("* "));
        }
    }
    for para in paragraphs {
        let text = para.join(" ").replace("**", "");
        let text = text.trim_matches('*').trim();
        let end = text.find(". ").map_or(text.len(), |i| i + 1);
        let sentence = text[..end].trim();
        if sentence.len() < 25 || sentence.ends_with(':') || sentence.starts_with('(') {
            continue;
        }
        return sentence.to_string();
    }
    String::new()
}

/// A page's sections: one for each heading, its text up to the next heading. Text inside a code
/// block is never a heading.
fn sections_of(page: &Page) -> Vec<Section> {
    let mut out = Vec::new();
    let mut anchors = Anchors::default();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut current: Option<Section> = None;
    let mut fenced = false;
    for line in page.text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let level = if fenced { 0 } else { line.chars().take_while(|&c| c == '#').count() };
        if level > 0 && line[level..].starts_with(' ') {
            let heading = line[level + 1..].trim().to_string();
            if let Some(s) = current.take() {
                out.push(s);
            }
            stack.retain(|(l, _)| *l < level);
            stack.push((level, heading.clone()));
            current = Some(Section {
                page: page.path.clone(),
                anchor: anchors.of(&heading),
                headings: stack.iter().map(|(_, h)| h.clone()).collect(),
                text: String::new(),
            });
            continue;
        }
        if let Some(s) = &mut current {
            s.text.push_str(line);
            s.text.push('\n');
        }
    }
    out.extend(current);
    for s in &mut out {
        s.text = s.text.trim().to_string();
    }
    out
}

/// Module `m`'s page and the index's records of its items. A module with no public items has an
/// empty page, which isn't written.
fn module_page(index: &Index, m: usize, source: &str) -> (Page, Vec<Value>) {
    let module = &index.modules[m];
    // The built-in types' page is std's, beside its modules', with the built-in functions.
    let builtins = module.path == [crate::builtin_types::MODULE];
    let (path, title) = if builtins {
        ("std/builtins.md".to_string(), "Built-in types and functions".to_string())
    } else {
        (format!("{}.md", module.path.join("/")), module.path.join("::"))
    };
    let top: Vec<usize> = (0..index.entries.len())
        .filter(|&i| {
            let e = &index.entries[i];
            e.kind != "method"
                && e.path.len() == module.path.len() + 1
                && e.path.starts_with(&module.path)
        })
        .collect();
    let mut page = Page {
        path: path.clone(),
        title: title.clone(),
        summary: first_sentence(&module.intro).replace("language.md §", "§"),
        kind: "module",
        text: String::new(),
    };
    if top.is_empty() {
        return (page, Vec::new());
    }
    let mut anchors = Anchors::default();
    // The page's own title takes its slug first, so an item named as the title gets `-2`.
    anchors.of(&title);
    let mut text = format!("# {title}\n\n");
    if !module.intro.is_empty() {
        text.push_str(&format!("{}\n\n", module.intro));
    }
    let mut records = Vec::new();
    for &i in &top {
        let e = &index.entries[i];
        let heading = e.path.last().cloned().unwrap_or_default();
        let anchor = anchors.of(&heading);
        text.push_str(&format!("## {heading}\n\n"));
        write_entry(&mut text, e, source);
        records.push(record(e, &path, &anchor));
        for &mi in &e.methods {
            let me = &index.entries[mi];
            let heading = me.path[me.path.len() - 2..].join("::");
            let anchor = anchors.of(&heading);
            text.push_str(&format!("### {heading}\n\n"));
            write_entry(&mut text, me, source);
            records.push(record(me, &path, &anchor));
        }
    }
    if builtins {
        text.push_str(&builtin_functions());
    }
    page.text = text;
    (page, records)
}

/// An item's kind, source (a link to its line at `source`, the repository's files at the
/// release's commit), signature, doc and the traits it implements.
fn write_entry(text: &mut String, e: &Entry, source: &str) {
    text.push_str(&format!(
        "`{}` · [`{}:{}`]({source}{}#L{})\n\n",
        e.kind, e.source, e.line, e.source, e.line
    ));
    text.push_str(&format!("```wrela\n{}\n```\n\n", e.signature.trim_end()));
    if !e.doc.is_empty() {
        text.push_str(&format!("{}\n\n", e.doc.trim_end()));
    }
    if !e.implements.is_empty() {
        text.push_str(&format!(
            "Implements: {}.\n\n",
            e.implements.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")
        ));
    }
}

/// An item's record in `index.json`.
fn record(e: &Entry, page: &str, anchor: &str) -> Value {
    json!({
        "path": e.path.join("::"),
        "kind": e.kind,
        "signature": e.signature,
        "summary": doc::first_sentence(&e.doc).replace("language.md §", "§"),
        "doc": e.doc,
        "page": page,
        "anchor": anchor,
        "source": e.source,
        "line": e.line,
    })
}

/// The built-in functions, which no module declares: the end of the built-ins' page.
fn builtin_functions() -> String {
    let mut text = String::from(
        "## Built-in functions\n\nNo import is needed; most work on scalars and vectors alike (language.md §4).\n\n",
    );
    for (name, arity, method) in wrela_driver::builtins() {
        text.push_str(&format!("- `{}`\n", doc::builtin_call(name, arity, method)));
    }
    text
}

/// Where each numbered part of the spec is: `6` and `6.15` to their page and the anchor on it
/// (none for a page's top).
fn spec_places(pages: &[Page]) -> BTreeMap<String, (String, String)> {
    let mut places = BTreeMap::new();
    for p in pages.iter().filter(|p| p.path.starts_with("spec/language/")) {
        for s in sections_of(p) {
            let Some(heading) = s.headings.last() else { continue };
            let number: String =
                heading.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            let number = number.trim_end_matches('.');
            if number.is_empty() {
                continue;
            }
            let anchor = if s.headings.len() == 1 { String::new() } else { s.anchor };
            places.entry(number.to_string()).or_insert((p.path.clone(), anchor));
        }
    }
    places
}

/// The path from page `from` to page `to`, both relative to the export's root.
fn relative(from: &str, to: &str) -> String {
    let dir: Vec<&str> = from.split('/').collect();
    let dir = &dir[..dir.len() - 1];
    let parts: Vec<&str> = to.split('/').collect();
    let common = dir.iter().zip(&parts[..parts.len() - 1]).take_while(|(a, b)| a == b).count();
    let mut out = "../".repeat(dir.len() - common);
    out.push_str(&parts[common..].join("/"));
    out
}

/// `text`, on page `from`, with each reference to a numbered part of the spec (`§17.1`, or
/// `language.md §17.1`) a link to it. Code, links, and references to other documents
/// (`vision.md §2`, `#28 §5`) stay as they are.
fn link_sections(text: &str, from: &str, places: &BTreeMap<String, (String, String)>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push_str(line);
            continue;
        }
        if fenced {
            out.push_str(line);
            continue;
        }
        let mut code = false;
        let mut i = 0;
        while i < line.len() {
            let c = line[i..].chars().next().expect("a char");
            if c == '`' {
                code = !code;
            } else if c == '[' && !code {
                // An existing link, to its target's end.
                if let Some(end) = line[i..]
                    .find("](")
                    .and_then(|k| line[i + k..].find(')').map(|e| i + k + e + 1))
                {
                    out.push_str(&line[i..end]);
                    i = end;
                    continue;
                }
            } else if c == '§' && !code {
                let at = i + c.len_utf8();
                let digits: String =
                    line[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                let number = digits.trim_end_matches('.');
                let before = line[..i].trim_end();
                let word = before.rsplit([' ', '(', '[']).next().unwrap_or("");
                let other = (word.ends_with(".md") && word != "language.md")
                    || (word.starts_with('#') && word.len() > 1);
                if let Some((page, anchor)) = places.get(number).filter(|_| !other) {
                    if word == "language.md" {
                        out.truncate(out.len() - (line[..i].len() - before.len()) - word.len());
                    }
                    let path = if page == from { String::new() } else { relative(from, page) };
                    let target = if anchor.is_empty() { path } else { format!("{path}#{anchor}") };
                    let target = if target.is_empty() { "#".to_string() } else { target };
                    out.push_str(&format!("[§{number}]({target})"));
                    i = at + number.len();
                    continue;
                }
            }
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// `llms.txt`: what the docs are and how to read them, each group of pages in one file with its
/// size, then each page by its group, with its summary.
fn llms(
    groups: &[(&str, Vec<&Page>)],
    sizes: &[(&str, usize)],
    all: usize,
    version: &str,
    commit: Option<&str>,
    repository: &str,
) -> String {
    let at = match commit {
        Some(c) => format!("version {version}, commit {c}"),
        None => format!("version {version}"),
    };
    let mut out = format!(
        "# wrela\n\n> The reference docs of wrela ({at}), an MIT-licensed language, compiler and engine for games that run in the browser. Each page is Markdown. Source: {repository}\n\n\
         How to read them:\n\n\
         - To write Wrela, read the language's pages in order, then look up items in std and the packages.\n\
         - Each group's pages are also in one file, under \"In one file\" below: read a whole group at once where it fits.\n\
         - `index.json` lists every page (`path`, `title`, `summary`, `kind`), every public item (`path`, `kind`, `signature`, `summary`, `doc`, `page`, `anchor`, `source`, `line`) and every section of the spec (`page`, `anchor`, `headings`, `text`). Search it for an item by its path, such as `std::arena::Arena::insert`.\n\
         - An anchor is its heading in lowercase, with `-` for spaces and punctuation: `std/arena.md#arena-insert` is `Arena::insert`.\n"
    );
    out.push_str("\n## In one file\n\n");
    for (g, size) in sizes {
        out.push_str(&format!("- [{}]({g}/llms-full.txt): {}\n", title_of(g), size_of(*size)));
    }
    out.push_str(&format!("- [Everything](llms-full.txt): {}\n", size_of(all)));
    for (g, pages) in groups {
        out.push_str(&format!("\n## {}\n\n", title_of(g)));
        for p in pages {
            if p.summary.is_empty() {
                out.push_str(&format!("- [{}]({})\n", p.title, p.path));
            } else {
                out.push_str(&format!("- [{}]({}): {}\n", p.title, p.path, p.summary));
            }
        }
    }
    out
}

/// A group's title in `llms.txt`.
fn title_of(group: &str) -> &str {
    if group == "spec" { "The language" } else { group }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths_must_be_unique_on_case_insensitive_filesystems() {
        let files =
            vec![("Foo/shapes.md".into(), Vec::new()), ("foo/Shapes.md".into(), Vec::new())];
        assert!(unique_paths(&files).is_err());
    }

    #[test]
    fn a_heading_s_anchor_is_its_slug_and_repeats_are_numbered() {
        assert_eq!(slug("6.1 Values, copies and moves"), "6-1-values-copies-and-moves");
        assert_eq!(slug("Arena::push"), "arena-push");
        let mut a = Anchors::default();
        assert_eq!([a.of("Arena"), a.of("arena"), a.of("Arena")], ["arena", "arena-2", "arena-3"]);
    }

    #[test]
    fn sections_follow_headings_but_not_code() {
        let page = Page {
            path: "p.md".into(),
            title: "P".into(),
            summary: String::new(),
            kind: "spec",
            text: "# P\n\nintro\n\n## A\n\nfirst\n\n```\n# not a heading\n```\n\n### B\n\nsecond\n"
                .into(),
        };
        let s = sections_of(&page);
        let found: Vec<(&str, Vec<&str>)> = s
            .iter()
            .map(|s| (s.anchor.as_str(), s.headings.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(found, [("p", vec!["P"]), ("a", vec!["P", "A"]), ("b", vec!["P", "A", "B"])]);
        assert_eq!(s[1].text, "first\n\n```\n# not a heading\n```");
    }

    #[test]
    fn references_to_the_spec_are_links_but_not_in_code_or_to_other_documents() {
        let page = |path: &str, text: &str| Page {
            path: path.into(),
            title: String::new(),
            summary: String::new(),
            kind: "spec",
            text: text.into(),
        };
        let pages = [
            page("spec/language/6-memory.md", "# 6. Memory\n"),
            page(
                "spec/language/17-host-services.md",
                "# 17. Host services\n\n## 17.1 Requests, not async\n",
            ),
            page("spec/language/4-types.md", "# 4. Types\n"),
        ];
        let places = spec_places(&pages);
        let text = "Requests (language.md §17.1), types (§4, §9) and `§17.1`; vision.md §2, #28 §5.\n\
                    ```\n§4\n```\n[§4](x.md)\n";
        assert_eq!(
            link_sections(text, "std/io.md", &places),
            "Requests ([§17.1](../spec/language/17-host-services.md#17-1-requests-not-async)), types \
             ([§4](../spec/language/4-types.md), §9) and `§17.1`; vision.md §2, #28 §5.\n\
             ```\n§4\n```\n[§4](x.md)\n"
        );
        assert_eq!(
            link_sections("§6 and §4", "spec/language/6-memory.md", &places),
            "[§6](#) and [§4](4-types.md)"
        );
    }

    #[test]
    fn a_summary_is_the_first_sentence_that_is_not_a_lead_in() {
        assert_eq!(
            first_sentence("This section says what tools there are. Three of them:\n\n- one\n"),
            "This section says what tools there are."
        );
        assert_eq!(
            first_sentence("Goals:\n\n- **Memory-safe** without a collector, always.\n"),
            "Memory-safe without a collector, always."
        );
    }

    #[test]
    fn std_s_source_is_the_toolchain_release_s_and_the_packages_otherwise() {
        let ws = "https://github.com/rywible/wrela";
        assert_eq!(
            toolchain_source(ws, Some("abcdef1"), Some("1234567")),
            ("https://github.com/rywible/wrela-compiler", Some("1234567"))
        );
        assert_eq!(toolchain_source(ws, Some("abcdef1"), None), (ws, Some("abcdef1")));
    }

    #[test]
    fn a_manifest_names_its_package() {
        assert_eq!(package_name("# x\n[package]\nname = \"engine\"\n"), Some("engine".into()));
        assert_eq!(package_name("[deps]\nname = \"no\"\n"), None);
    }
}
