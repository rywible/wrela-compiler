//! `wrela doc --export <dir> [--commit <sha>] [<package-dir>...]`: the reference docs as files
//! that people and agents read alike. The language's spec, split by section, and a page per
//! module of std and of each package given, in Markdown; `index.json`, every item and every
//! section of prose with its page, anchor and source; `llms.txt`, a map of the pages, and
//! `llms-full.txt`, all of them in one file. The same input gives the same bytes.
//!
//! The items come from the index `wrela doc` reads (`doc.rs`), so a page says what a lookup
//! says. The spec is the one this compiler was built with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::doc::{self, Entry, Index};

const LANGUAGE: &str = include_str!("../../../docs/language.md");
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
                "wrote {n} pages, index.json, llms.txt and llms-full.txt to {}",
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
    let mut sources = doc::std_sources();
    let mut names = Vec::new();
    for dir in packages {
        let manifest = std::fs::read_to_string(dir.join("wrela.toml"))
            .map_err(|e| format!("`{}` isn't a package: {e}", dir.display()))?;
        let name = package_name(&manifest)
            .ok_or_else(|| format!("`{}`'s wrela.toml names no package", dir.display()))?;
        if name == "std" || names.contains(&name) {
            return Err(format!("two packages are named `{name}`"));
        }
        sources.extend(doc::package_sources(dir, Some(&name))?);
        names.push(name);
    }
    let index = doc::index_of(sources);
    let mut pages = spec_pages();
    let mut sections: Vec<Section> = Vec::new();
    for p in &pages {
        sections.extend(sections_of(p));
    }
    let mut items: Vec<Value> = Vec::new();
    for m in 0..index.modules.len() {
        let (page, mut records) = module_page(&index, m);
        if page.text.is_empty() {
            continue;
        }
        items.append(&mut records);
        pages.push(page);
    }
    pages.push(builtins_page());
    // The spec in its own order, then the modules by path.
    let spec = pages.iter().filter(|p| p.kind == "spec").count();
    pages[spec..].sort_by(|a, b| a.path.cmp(&b.path));
    let version = env!("CARGO_PKG_VERSION");
    let index_json = json!({
        "version": version,
        "commit": commit,
        "repository": REPOSITORY,
        "pages": pages.iter().map(|p| json!({
            "path": p.path, "title": p.title, "summary": p.summary, "kind": p.kind,
        })).collect::<Vec<_>>(),
        "items": items,
        "sections": sections.iter().map(|s| json!({
            "page": s.page, "anchor": s.anchor, "headings": s.headings, "text": s.text,
        })).collect::<Vec<_>>(),
    });
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for p in &pages {
        let path = out.join(&p.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, &p.text)
            .map_err(|e| format!("can't write `{}`: {e}", path.display()))?;
    }
    let json = serde_json::to_string_pretty(&index_json).map_err(|e| e.to_string())? + "\n";
    std::fs::write(out.join("index.json"), json).map_err(|e| e.to_string())?;
    std::fs::write(out.join("llms.txt"), llms(&pages, version, commit))
        .map_err(|e| e.to_string())?;
    let mut full = String::new();
    for p in &pages {
        full.push_str(&format!("<!-- {} -->\n\n", p.path));
        full.push_str(&p.text);
        full.push('\n');
    }
    std::fs::write(out.join("llms-full.txt"), full).map_err(|e| e.to_string())?;
    Ok(pages.len())
}

/// The `name` of a manifest's `[package]`.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(rest) = line.strip_prefix("name") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
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
        title: "wrela grammar".to_string(),
        summary: "The language's syntax, normative, in EBNF over spec/lexical.md's tokens."
            .to_string(),
        kind: "spec",
        text: format!("# wrela grammar\n\n```ebnf\n{}\n```\n", GRAMMAR.trim_end()),
    });
    pages
}

/// A Markdown body's first sentence of prose, on one line, for a summary: the first paragraph
/// that is a sentence, not a table, code, a heading, a rule, a lead-in (`Goals:`) or an aside
/// in brackets. A list item reads as its text.
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
        if text.len() < 25 || text.ends_with(':') || text.starts_with('(') {
            continue;
        }
        let end = text.find(". ").map_or(text.len(), |i| i + 1);
        return text[..end].trim().to_string();
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
fn module_page(index: &Index, m: usize) -> (Page, Vec<Value>) {
    let module = &index.modules[m];
    let name = module.path.join("::");
    let path = format!("{}.md", module.path.join("/"));
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
        title: name.clone(),
        summary: first_sentence(&module.intro),
        kind: "module",
        text: String::new(),
    };
    if top.is_empty() {
        return (page, Vec::new());
    }
    let mut anchors = Anchors::default();
    // The page's own title takes its slug first, so an item named as the module gets `-2`.
    anchors.of(&name);
    let mut text = format!("# {name}\n\n");
    if !module.intro.is_empty() {
        text.push_str(&format!("{}\n\n", module.intro));
    }
    let mut records = Vec::new();
    for &i in &top {
        let e = &index.entries[i];
        let heading = e.path.last().cloned().unwrap_or_default();
        let anchor = anchors.of(&heading);
        text.push_str(&format!("## {heading}\n\n"));
        write_entry(&mut text, e);
        records.push(record(e, &path, &anchor));
        for &mi in &e.methods {
            let me = &index.entries[mi];
            let heading = me.path[me.path.len() - 2..].join("::");
            let anchor = anchors.of(&heading);
            text.push_str(&format!("### {heading}\n\n"));
            write_entry(&mut text, me);
            records.push(record(me, &path, &anchor));
        }
    }
    page.text = text;
    (page, records)
}

/// An item's kind, source, signature, doc and the traits it implements.
fn write_entry(text: &mut String, e: &Entry) {
    text.push_str(&format!("`{}` · `{}:{}`\n\n", e.kind, e.source, e.line));
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
        "summary": doc::first_line(&e.doc),
        "doc": e.doc,
        "page": page,
        "anchor": anchor,
        "source": e.source,
        "line": e.line,
    })
}

/// The built-in functions, which no module declares.
fn builtins_page() -> Page {
    let mut text = String::from(
        "# Built-in functions\n\nNo import is needed; most work on scalars and vectors alike (language.md §4).\n\n",
    );
    for (name, arity, method) in wrela_driver::builtins() {
        let args = ["x", "y", "z"][..arity.min(3)].join(", ");
        if method {
            text.push_str(&format!(
                "- `x.{name}({})`\n",
                ["", "y", "y, z"][arity.saturating_sub(1).min(2)]
            ));
        } else {
            text.push_str(&format!("- `{name}({args})`\n"));
        }
    }
    Page {
        path: "std/builtins.md".to_string(),
        title: "Built-in functions".to_string(),
        summary: "The functions every program has without an import.".to_string(),
        kind: "module",
        text,
    }
}

/// `llms.txt`: what the docs are, then each page by its group, with its summary.
fn llms(pages: &[Page], version: &str, commit: Option<&str>) -> String {
    let at = match commit {
        Some(c) => format!("version {version}, commit {c}"),
        None => format!("version {version}"),
    };
    let mut out = format!(
        "# wrela\n\n> The reference docs of wrela ({at}), an MIT-licensed language, compiler and engine for games that run in the browser. Each page is Markdown. `index.json` lists every public item and every section of the spec, with its page, anchor and source file; `llms-full.txt` is every page in one file. Source: {REPOSITORY}\n"
    );
    let mut groups: BTreeMap<&str, Vec<&Page>> = BTreeMap::new();
    for p in pages {
        let group = if p.kind == "spec" { "spec" } else { p.path.split('/').next().unwrap_or("") };
        groups.entry(group).or_default().push(p);
    }
    // The spec first, then std, then the packages by name.
    let mut order: Vec<&str> = groups.keys().copied().collect();
    order.sort_by_key(|g| (*g != "spec", *g != "std", *g));
    for g in order {
        let title = match g {
            "spec" => "The language",
            "std" => "std",
            other => other,
        };
        out.push_str(&format!("\n## {title}\n\n"));
        for p in &groups[g] {
            if p.summary.is_empty() {
                out.push_str(&format!("- [{}]({})\n", p.title, p.path));
            } else {
                out.push_str(&format!("- [{}]({}): {}\n", p.title, p.path, p.summary));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_manifest_names_its_package() {
        assert_eq!(package_name("# x\n[package]\nname = \"engine\"\n"), Some("engine".into()));
        assert_eq!(package_name("[deps]\nname = \"no\"\n"), None);
    }
}
