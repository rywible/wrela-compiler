//! What the compiler knows of a program, for tools (`wrela query`, `wrela context`): one check
//! answers a batch of queries, as JSON.
//!
//! | Query | Answer |
//! |---|---|
//! | `type <file>:<line>:<col>` | the type of the smallest expression, local or parameter there |
//! | `callers <fn>` | each call of the function (through its trait too), and where |
//! | `callees <fn>` | what the function calls, dispatches or draws, and where first |
//! | `impls <trait or type>` | a trait's impls, or a type's |
//! | `effects <fn>` | its effects (language.md §8), each with a call chain to where it happens |
//! | `borrows <file>:<line>` | the loans live and the places moved out of before the line runs |
//! | `instantiations <fn>` | the type arguments lowering instantiates it with, CPU and GPU |
//! | `search <signature>` | functions whose signature fits: `(vec3, _) -> f32`, `(Grid, ..)` |
//!
//! A function or type is named by its path (`main::step`, `std::field::sphere`, `Grid::at`) or
//! its last parts (`step`, `at`); a name with several matches is an error that lists them.

use crate::SourceMap;
use serde_json::{Value, json};
use std::path::Path;
use wrela_diag::{FileId, SourceFile, Span};
use wrela_sema::Checked;
use wrela_sema::defs::{AdtKind, FnOwner};
use wrela_sema::mir::{
    self, Arg, FnUse as Use, Operand, OperandKind, Rvalue, StatementKind, rvalues,
};
use wrela_sema::program::Program;
use wrela_sema::ty::{AdtId, FnId, ModuleId, TraitId, TyId, TyKind};

/// What each query takes, for usage messages.
pub const KINDS: &str = "type <file>:<line>:<col> | callers <fn> | callees <fn> | impls <trait-or-type> | \
effects <fn> | borrows <file>:<line> | instantiations <fn> | search <signature>";

/// One query.
#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    Type { file: String, line: u32, column: u32 },
    Callers(String),
    Callees(String),
    Impls(String),
    Effects(String),
    Borrows { file: String, line: u32 },
    Instantiations(String),
    Search(String),
}

/// Reads a query as written: `callers main::step`.
pub fn parse(text: &str) -> Result<Query, String> {
    let text = text.trim();
    let (kind, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(format!("`{text}` isn't a query: {KINDS}"));
    }
    let at = |n: usize| -> Result<(String, Vec<u32>), String> {
        let parts: Vec<&str> = rest.rsplitn(n + 1, ':').collect();
        let nums: Option<Vec<u32>> =
            parts[..parts.len() - 1].iter().rev().map(|p| p.parse().ok()).collect();
        match nums {
            Some(v) if parts.len() == n + 1 && v.iter().all(|&x| x > 0) => {
                Ok((parts[parts.len() - 1].to_string(), v))
            }
            _ => Err(format!(
                "`{kind}` takes `<file>{}`, not `{rest}`",
                [":<line>", ":<col>"][..n].concat()
            )),
        }
    };
    Ok(match kind {
        "type" => {
            let (file, v) = at(2)?;
            Query::Type { file, line: v[0], column: v[1] }
        }
        "borrows" => {
            let (file, v) = at(1)?;
            Query::Borrows { file, line: v[0] }
        }
        "callers" => Query::Callers(rest.into()),
        "callees" => Query::Callees(rest.into()),
        "impls" => Query::Impls(rest.into()),
        "effects" => Query::Effects(rest.into()),
        "instantiations" => Query::Instantiations(rest.into()),
        "search" => Query::Search(rest.into()),
        _ => return Err(format!("`{kind}` isn't a query: {KINDS}")),
    })
}

/// Answers `queries` on the package at `root`, in order: each answer has the query's text as
/// `query`, and `error` if it couldn't be answered. `Err` if the package doesn't check.
pub fn run(root: &Path, queries: &[(String, Query)]) -> Result<Vec<Value>, String> {
    let lower = queries.iter().any(|(_, q)| matches!(q, Query::Instantiations(_)));
    crate::with_lowered(root, lower, |checked, sources, lowered| {
        let ix = Index::new(checked, sources);
        let effects = std::cell::OnceCell::new();
        queries
            .iter()
            .map(|(text, q)| {
                let answer = match q {
                    Query::Type { file, line, column } => ix.type_at(file, *line, *column),
                    Query::Callers(name) => ix.callers(name),
                    Query::Callees(name) => ix.callees(name),
                    Query::Impls(name) => ix.impls(name),
                    Query::Effects(name) => {
                        let fx = effects.get_or_init(|| {
                            wrela_sema::effects::Effects::build(ix.p, &checked.mir)
                        });
                        ix.effects(name, fx)
                    }
                    Query::Borrows { file, line } => ix.borrows(file, *line),
                    Query::Instantiations(name) => ix.instantiations(name, lowered),
                    Query::Search(sig) => ix.search(sig),
                };
                let mut v = answer.unwrap_or_else(|e| json!({ "error": e }));
                if let Value::Object(m) = &mut v {
                    m.insert("query".into(), Value::String(text.clone()));
                }
                v
            })
            .collect()
    })
}

/// How a use is named in the queries' answers.
fn how(u: &Use) -> &'static str {
    match u {
        Use::Call(_) => "call",
        Use::Trait(..) => "trait call",
        Use::Value(_) => "as a value",
        Use::Dispatch(_) => "dispatch",
        Use::Draw(_) => "draw",
    }
}

/// Each use `body` makes of a function, and where ([`mir::fn_uses`]).
fn uses(p: &Program, body: &mir::Body) -> Vec<(Use, Span)> {
    let mut out = Vec::new();
    for (s, r) in rvalues(body) {
        mir::fn_uses(p, s, r, |u, span| out.push((u, span)));
    }
    out
}

/// Whether `a` and `b` have the same head: the same struct or enum, whatever its arguments, or
/// else the same type.
pub(crate) fn same_head(p: &Program, a: TyId, b: TyId) -> bool {
    match (p.types.kind(a), p.types.kind(b)) {
        (TyKind::Adt(x, _), TyKind::Adt(y, _)) => x == y,
        _ => a == b,
    }
}

/// The first line of the doc comment and attributes (`///` and `@` lines) directly above line
/// `line` of `f`, or `line` if there are none.
pub(crate) fn doc_top(f: &SourceFile, line: usize) -> usize {
    let mut top = line;
    while top > 0 {
        let t = f.line_text(top - 1).trim_start();
        if !(t.starts_with("///") || t.starts_with('@')) {
            break;
        }
        top -= 1;
    }
    top
}

/// The types (structs and enums) a type names.
fn adts_in(p: &Program, t: TyId, out: &mut Vec<AdtId>) {
    match p.types.kind(t) {
        TyKind::Adt(a, args) => {
            if !out.contains(a) {
                out.push(*a);
            }
            for &x in args.clone().iter() {
                adts_in(p, x, out);
            }
        }
        TyKind::Tuple(ts) => {
            for &x in ts.clone().iter() {
                adts_in(p, x, out);
            }
        }
        TyKind::Array(x, _) | TyKind::ArrayN(x, _) | TyKind::Slice(x) => adts_in(p, *x, out),
        _ => {}
    }
}

/// A type's text without spaces, for comparing signatures.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Splits `s` at its top-level commas.
fn split_top(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in s.chars() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// A signature to search for: its parameters' types (`_` any one, `..` any number) and its
/// return type (`None`: any).
fn parse_signature(sig: &str) -> Result<(Vec<String>, Option<String>), String> {
    let s = sig.trim();
    let s = s.strip_prefix("fn").unwrap_or(s).trim();
    let bad = || {
        format!(
            "a signature is `(<type>, ...) -> <type>`, with `_` for any type and `..` for any parameters: not `{sig}`"
        )
    };
    let (params, ret) = match s.find("->") {
        Some(i) if s.starts_with('(') => (&s[..i], Some(s[i + 2..].trim())),
        None if s.starts_with('(') => (s, None),
        Some(0) => ("(..)", Some(s[2..].trim())),
        _ => return Err(bad()),
    };
    let params = params.trim();
    let inner = params.strip_prefix('(').and_then(|x| x.strip_suffix(')')).ok_or_else(bad)?;
    let strip = |t: &str| {
        let t = t.trim();
        let t = t.strip_prefix("mut ").or_else(|| t.strip_prefix("take ")).unwrap_or(t);
        squash(t.strip_prefix("borrow ").unwrap_or(t))
    };
    let ps = split_top(inner).iter().map(|t| strip(t)).collect();
    Ok((ps, ret.filter(|r| *r != "_").map(squash)))
}

/// Whether `have` (a function's parameter types) fit `want` (a search's).
fn params_fit(want: &[String], have: &[String]) -> bool {
    match want.split_first() {
        None => have.is_empty(),
        Some((w, rest)) if w == ".." => (0..=have.len()).any(|k| params_fit(rest, &have[k..])),
        Some((w, rest)) => {
            have.split_first().is_some_and(|(h, hr)| (w == "_" || w == h) && params_fit(rest, hr))
        }
    }
}

/// The program's functions, types and traits by path, and how to show them.
pub(crate) struct Index<'a> {
    pub p: &'a Program,
    checked: &'a Checked,
    sources: &'a SourceMap,
    fns: Vec<(FnId, Vec<String>)>,
    adts: Vec<(AdtId, Vec<String>)>,
    traits: Vec<(TraitId, Vec<String>)>,
}

/// One of the program's named things.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Item {
    Fn(FnId),
    Adt(AdtId),
    Trait(TraitId),
}

/// What an item's declaration says: its module, its name, where it is, and whether it's `pub`.
pub(crate) struct Decl<'a> {
    pub module: ModuleId,
    pub name: &'a str,
    pub span: Span,
    /// Its name's span; a trait's is its whole declaration's.
    pub name_span: Span,
    pub public: bool,
}

impl Item {
    pub fn decl(self, p: &Program) -> Decl<'_> {
        let (module, name, span, name_span, public) = match self {
            Item::Fn(f) => {
                let d = p.func(f);
                (d.module, &d.name, d.span, d.name_span, d.public)
            }
            Item::Adt(a) => {
                let d = p.adt(a);
                (d.module, &d.name, d.span, d.name_span, d.public)
            }
            Item::Trait(t) => {
                let d = p.trait_(t);
                (d.module, &d.name, d.span, d.span, d.public)
            }
        };
        Decl { module, name, span, name_span, public }
    }
}

impl<'a> Index<'a> {
    pub fn new(checked: &'a Checked, sources: &'a SourceMap) -> Index<'a> {
        let p = &checked.program;
        let base = |t: TyId| {
            let d = p.display_ty(t);
            d.split('<').next().unwrap_or(&d).to_string()
        };
        let mut fns = Vec::new();
        for (i, f) in p.fns.iter().enumerate() {
            let mut path = p.module(f.module).path.clone();
            match f.owner {
                FnOwner::Free => {}
                FnOwner::Impl(im) => path.push(base(p.impl_(im).self_ty)),
                FnOwner::Trait(t) => path.push(p.trait_(t).name.clone()),
                FnOwner::Const(_) => continue,
            }
            path.push(f.name.clone());
            fns.push((FnId(i as u32), path));
        }
        let path = |item: Item| {
            let d = item.decl(p);
            let mut path = p.module(d.module).path.clone();
            path.push(d.name.to_string());
            path
        };
        let adts =
            (0..p.adts.len() as u32).map(|i| (AdtId(i), path(Item::Adt(AdtId(i))))).collect();
        let traits = (0..p.traits.len() as u32)
            .map(|i| (TraitId(i), path(Item::Trait(TraitId(i)))))
            .collect();
        Index { p, checked, sources, fns, adts, traits }
    }

    /// The items named `name`: those whose path is it, else those whose path ends with it.
    pub fn find(&self, name: &str) -> Vec<(Item, String)> {
        let q: Vec<&str> = name.trim().split("::").collect();
        let mut all: Vec<(Item, &Vec<String>)> = Vec::new();
        all.extend(self.fns.iter().map(|(f, p)| (Item::Fn(*f), p)));
        all.extend(self.adts.iter().map(|(a, p)| (Item::Adt(*a), p)));
        all.extend(self.traits.iter().map(|(t, p)| (Item::Trait(*t), p)));
        let exact: Vec<_> = all
            .iter()
            .filter(|(_, p)| p.iter().map(String::as_str).eq(q.iter().copied()))
            .collect();
        let found: Vec<_> = if exact.is_empty() {
            all.iter()
                .filter(|(_, p)| {
                    p.len() >= q.len()
                        && p[p.len() - q.len()..].iter().map(String::as_str).eq(q.iter().copied())
                })
                .collect()
        } else {
            exact
        };
        let mut out: Vec<(Item, String)> =
            found.into_iter().map(|(i, p)| (*i, p.join("::"))).collect();
        // Of several, std's private items go (a full path still names them), then the program's
        // own item wins if there's one.
        if out.len() > 1 {
            let module = |i: Item| i.decl(self.p).module;
            let public = |i: Item| i.decl(self.p).public;
            let shown: Vec<(Item, String)> = out
                .iter()
                .filter(|(i, _)| !self.p.is_std(module(*i)) || public(*i))
                .cloned()
                .collect();
            let own: Vec<(Item, String)> = shown
                .iter()
                .filter(|(i, _)| {
                    self.p.package_of(module(*i)).kind == wrela_sema::defs::PackageKind::Program
                })
                .cloned()
                .collect();
            out = if own.len() == 1 { own } else { shown };
        }
        // A path names a method once, though a generic impl may give it several.
        out.dedup_by(|a, b| a.1 == b.1 && matches!((a.0, b.0), (Item::Fn(_), Item::Fn(_))));
        out
    }

    /// The one item named `name`, or why not.
    pub fn one(&self, name: &str) -> Result<(Item, String), String> {
        let found = self.find(name);
        match found.len() {
            0 => Err(format!("nothing is named `{name}`")),
            1 => Ok(found.into_iter().next().unwrap()),
            n => {
                let names: Vec<&str> = found.iter().take(12).map(|(_, p)| p.as_str()).collect();
                let more = if n > 12 { format!(", and {} more", n - 12) } else { String::new() };
                Err(format!(
                    "{n} items are named `{name}`: {}{more}; name one by its path",
                    names.join(", ")
                ))
            }
        }
    }

    fn one_fn(&self, name: &str) -> Result<(FnId, String), String> {
        let fns: Vec<(Item, String)> =
            self.find(name).into_iter().filter(|(i, _)| matches!(i, Item::Fn(_))).collect();
        match fns.as_slice() {
            [(Item::Fn(f), path)] => Ok((*f, path.clone())),
            [] => match self.one(name) {
                Ok((_, path)) => Err(format!("`{path}` isn't a function")),
                Err(e) => Err(e),
            },
            _ => {
                let names: Vec<&str> = fns.iter().take(12).map(|(_, p)| p.as_str()).collect();
                Err(format!(
                    "{} functions are named `{name}`: {}; name one by its path",
                    fns.len(),
                    names.join(", ")
                ))
            }
        }
    }

    pub fn fn_path(&self, f: FnId) -> String {
        self.fns
            .iter()
            .find(|(g, _)| *g == f)
            .map_or_else(|| self.p.fn_display_name(f), |(_, p)| p.join("::"))
    }

    pub fn at(&self, span: Span) -> String {
        self.sources.file(span.file).location(span.start)
    }

    /// The source line `span` starts on, trimmed.
    pub fn line(&self, span: Span) -> String {
        let f = self.sources.file(span.file);
        f.line_text(f.line_index(span.start)).trim().to_string()
    }

    /// A function's signature as written, on one line.
    pub fn signature(&self, f: FnId) -> String {
        let def = self.p.func(f);
        let text = self.p.text(def.sig_span).unwrap_or(&def.name);
        // A signature written over several lines has a trailing comma, and a break after `(`.
        text.split_whitespace().collect::<Vec<_>>().join(" ").replace("( ", "(").replace(", )", ")")
    }

    /// The source text of `span`, with the doc comment and attributes above it.
    pub fn source_with_doc(&self, span: Span) -> String {
        let f = self.sources.file(span.file);
        let start = f.line_start(doc_top(f, f.line_index(span.start))) as usize;
        f.text.get(start..span.end as usize).unwrap_or("").to_string()
    }

    /// The first line of the doc comment above `span`, if any.
    pub fn summary(&self, span: Span) -> String {
        let f = self.sources.file(span.file);
        let line = f.line_index(span.start);
        (doc_top(f, line)..line)
            .find_map(|l| f.line_text(l).trim_start().strip_prefix("///"))
            .map_or_else(String::new, |doc| doc.trim().to_string())
    }

    fn file(&self, name: &str) -> Result<FileId, String> {
        let found: Vec<(FileId, &str)> = self
            .sources
            .files()
            .filter(|(_, f)| f.name == name || f.name.ends_with(&format!("/{name}")))
            .map(|(id, f)| (id, f.name.as_str()))
            .collect();
        match found.as_slice() {
            [(id, _)] => Ok(*id),
            [] => Err(format!("no file of the program is `{name}`")),
            many => {
                let names: Vec<&str> = many.iter().map(|(_, n)| *n).collect();
                Err(format!("several files are `{name}`: {}", names.join(", ")))
            }
        }
    }

    /// The function whose body holds byte `offset` of `file` (the innermost, for a method).
    fn fn_at(&self, file: FileId, offset: u32) -> Option<FnId> {
        self.checked
            .mir
            .keys()
            .filter(|f| {
                let s = self.p.func(**f).span;
                s.file == file && s.start <= offset && offset < s.end
            })
            .min_by_key(|f| {
                let s = self.p.func(**f).span;
                s.end - s.start
            })
            .copied()
    }

    /// The byte offset of `line` and `column` (1-based, in characters) in `file`.
    fn offset(&self, file: FileId, line: u32, column: u32) -> Result<u32, String> {
        let f = self.sources.file(file);
        let l = line as usize - 1;
        if l >= f.line_count() {
            return Err(format!("{} has {} lines", f.name, f.line_count()));
        }
        let text = f.line_text(l);
        let bytes: usize = text.chars().take(column as usize - 1).map(char::len_utf8).sum();
        Ok(f.line_start(l) + bytes as u32)
    }

    fn type_at(&self, file: &str, line: u32, column: u32) -> Result<Value, String> {
        let file = self.file(file)?;
        let offset = self.offset(file, line, column)?;
        let f = self.fn_at(file, offset).ok_or("no function's code is there")?;
        let body = &self.checked.mir[&f];
        let def = self.p.func(f);
        // Every span with a type: parameters, locals, operands and what's assigned.
        let mut found: Vec<(Span, TyId, &str)> = Vec::new();
        for prm in &def.params {
            found.push((prm.span, prm.ty, "parameter"));
        }
        for l in &body.locals {
            if matches!(l.kind, mir::LocalKind::User(_)) {
                found.push((l.span, l.ty, "local"));
            }
        }
        let operand = |o: &Operand, found: &mut Vec<(Span, TyId, &str)>| {
            if !matches!(o.kind, OperandKind::Const(_)) || o.span.end > o.span.start {
                found.push((o.span, o.ty, "expression"));
            }
        };
        for (s, r) in rvalues(body) {
            if let StatementKind::Assign(place, _) = &s.kind {
                found.push((s.span, mir::place_ty(self.p, &body.locals, place), "expression"));
            }
            match r {
                Rvalue::Use(o)
                | Rvalue::Unary(_, o)
                | Rvalue::Convert(o)
                | Rvalue::ArrayRepeat(o, _) => operand(o, &mut found),
                Rvalue::Binary(_, a, b) => {
                    operand(a, &mut found);
                    operand(b, &mut found);
                }
                Rvalue::Adt { fields: os, .. }
                | Rvalue::Tuple(os)
                | Rvalue::Array(os)
                | Rvalue::Construct(os) => {
                    for o in os {
                        operand(o, &mut found);
                    }
                }
                Rvalue::Call(c) => {
                    for a in &c.args {
                        match a {
                            Arg::Take(o) => operand(o, &mut found),
                            Arg::Borrow(place, span) | Arg::Mut(place, span) => {
                                let ty = mir::place_ty(self.p, &body.locals, place);
                                found.push((*span, ty, "expression"));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let (span, ty, what) = found
            .into_iter()
            .filter(|(s, _, _)| {
                s.file == file && s.start <= offset && offset < s.end.max(s.start + 1)
            })
            .min_by_key(|(s, _, _)| (s.end - s.start, s.start))
            .ok_or("nothing with a type is there")?;
        Ok(json!({
            "at": self.at(span),
            "text": self.p.text(span).unwrap_or(""),
            "kind": what,
            "type": self.p.display_ty(ty),
            "in": self.fn_path(f),
        }))
    }

    /// Each use of `f`, by any function: the user, how and where.
    pub(crate) fn uses_of(&self, f: FnId) -> Vec<(FnId, Use, Span)> {
        let p = self.p;
        // An impl's method is also called through its trait, on its type.
        let through: Option<(FnId, TyId)> = match p.func(f).owner {
            FnOwner::Impl(im) => p.impl_(im).trait_ref.as_ref().and_then(|tr| {
                let m = wrela_sema::traits::trait_method(p, tr.trait_, &p.func(f).name)?;
                Some((m, p.impl_(im).self_ty))
            }),
            _ => None,
        };
        let mut out = Vec::new();
        for (&caller, body) in &self.checked.mir {
            for (u, span) in uses(p, body) {
                let hit = match &u {
                    Use::Trait(m, self_ty) => {
                        *m == f
                            || through
                                .is_some_and(|(tm, ty)| tm == *m && same_head(p, *self_ty, ty))
                    }
                    other => other.func() == f,
                };
                if hit {
                    out.push((caller, u, span));
                }
            }
        }
        out.sort_by_key(|(_, _, s)| (s.file, s.start));
        out
    }

    fn callers(&self, name: &str) -> Result<Value, String> {
        let (f, path) = self.one_fn(name)?;
        let calls: Vec<Value> = self
            .uses_of(f)
            .into_iter()
            .map(|(caller, u, span)| {
                json!({ "caller": self.fn_path(caller), "how": how(&u), "at": self.at(span), "line": self.line(span) })
            })
            .collect();
        let mut v = json!({ "function": path, "callers": calls });
        if !matches!(self.p.func(f).owner, FnOwner::Free) {
            v["note"] = Value::String(
                "calls through a generic parameter's trait method aren't listed: they're known once instantiated (see `instantiations`)".into(),
            );
        }
        Ok(v)
    }

    fn callees(&self, name: &str) -> Result<Value, String> {
        let (f, path) = self.one_fn(name)?;
        let body = self.checked.mir.get(&f).ok_or_else(|| format!("`{path}` has no body here"))?;
        let mut seen: Vec<(String, Option<String>)> = Vec::new();
        let mut out = Vec::new();
        for (u, span) in uses(self.p, body) {
            let on = match &u {
                Use::Trait(_, t) => Some(self.p.display_ty(*t)),
                _ => None,
            };
            let key = (self.fn_path(u.func()), on.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            let mut v = json!({ "callee": key.0, "how": how(&u), "at": self.at(span), "signature": self.signature(u.func()) });
            if let Some(t) = on {
                v["on"] = Value::String(t);
            }
            out.push(v);
        }
        Ok(json!({ "function": path, "callees": out }))
    }

    fn impls(&self, name: &str) -> Result<Value, String> {
        let p = self.p;
        let found: Vec<(Item, String)> =
            self.find(name).into_iter().filter(|(i, _)| !matches!(i, Item::Fn(_))).collect();
        let (item, path) = match found.as_slice() {
            [one] => one.clone(),
            [] => return Err(format!("no trait or type is named `{name}`")),
            many => {
                let names: Vec<&str> = many.iter().map(|(_, p)| p.as_str()).collect();
                return Err(format!(
                    "several traits and types are named `{name}`: {}",
                    names.join(", ")
                ));
            }
        };
        let show = |i: usize| {
            let im = &p.impls[i];
            let methods: Vec<&str> = im.methods.iter().map(|&m| p.func(m).name.as_str()).collect();
            json!({
                "type": p.display_ty(im.self_ty),
                "trait": im.trait_ref.as_ref().map(|t| p.display_trait_ref(t)),
                "at": self.at(im.span),
                "declared": im.from_opt_in,
                "methods": methods,
            })
        };
        let impls: Vec<Value> = self.impl_ids(item).into_iter().map(show).collect();
        let what = if matches!(item, Item::Trait(_)) { "trait" } else { "type" };
        Ok(json!({ what: path, "impls": impls }))
    }

    fn effects(&self, name: &str, fx: &wrela_sema::effects::Effects) -> Result<Value, String> {
        let (f, path) = self.one_fn(name)?;
        let (why, open) = fx.explain(f).ok_or_else(|| format!("`{path}` has no body here"))?;
        let effects: Vec<Value> = why
            .iter()
            .map(|w| json!({ "effect": w.effect.name(), "chain": w.chain, "at": self.at(w.span), "what": w.what }))
            .collect();
        Ok(json!({ "function": path, "effects": effects, "depends_on_instantiation": open }))
    }

    fn borrows(&self, file: &str, line: u32) -> Result<Value, String> {
        let file = self.file(file)?;
        let lo = self.offset(file, line, 1)?;
        let f = self.sources.file(file);
        let hi = f.line_end(line as usize - 1).max(lo + 1);
        let func = self
            .fn_at(file, lo)
            .or_else(|| self.fn_at(file, hi - 1))
            .ok_or("no function's code is on that line")?;
        let body = &self.checked.mir[&func];
        let st = wrela_sema::borrowck::state_at(self.p, body, file, lo, hi)
            .ok_or("no code of the function starts on that line")?;
        let loans: Vec<Value> = st
            .loans
            .iter()
            .map(|l| json!({ "place": l.place, "mutable": l.mutable, "holder": l.holder, "at": self.at(l.span) }))
            .collect();
        let moved: Vec<Value> = st
            .moved
            .iter()
            .map(|m| json!({ "place": m.place, "maybe": m.maybe, "at": self.at(m.span) }))
            .collect();
        Ok(json!({ "in": self.fn_path(func), "loans": loans, "moved": moved }))
    }

    fn instantiations(
        &self,
        name: &str,
        lowered: Option<&wrela_lower::Lowered>,
    ) -> Result<Value, String> {
        let (f, path) = self.one_fn(name)?;
        let lowered = lowered.ok_or("the program wasn't lowered")?;
        let p = self.p;
        // A trait's method is instantiated as its impls' methods.
        let fns = self.with_impls(f);
        let mut found: Vec<(FnId, Vec<String>, Vec<String>)> = Vec::new();
        let mut add = |g: FnId, substs: &[TyId], target: String| {
            let types: Vec<String> = substs.iter().map(|&t| p.display_ty(t)).collect();
            match found.iter_mut().find(|(h, t, _)| *h == g && *t == types) {
                Some((_, _, ts)) if !ts.contains(&target) => ts.push(target),
                Some(_) => {}
                None => found.push((g, types, vec![target])),
            }
        };
        for (g, substs) in &lowered.instances {
            if fns.contains(g) {
                add(*g, substs, "cpu".into());
            }
        }
        for pl in &lowered.pipelines {
            for (g, substs) in &pl.instances {
                if fns.contains(g) {
                    add(*g, substs, format!("pipeline {}", pl.name));
                }
            }
        }
        let list: Vec<Value> = found
            .into_iter()
            .map(|(g, t, ts)| {
                let mut v = json!({ "types": t, "in": ts });
                if g != f {
                    v["function"] = Value::String(self.fn_path(g));
                }
                v
            })
            .collect();
        Ok(
            json!({ "function": path, "generic": !p.fn_all_generics(f).is_empty(), "instantiations": list }),
        )
    }

    fn search(&self, sig: &str) -> Result<Value, String> {
        let (want, ret) = parse_signature(sig)?;
        let p = self.p;
        let mut hits = Vec::new();
        for (f, path) in &self.fns {
            let def = p.func(*f);
            let program = !p.is_std(def.module)
                && p.package_of(def.module).kind == wrela_sema::defs::PackageKind::Program;
            if !(def.public || program) {
                continue;
            }
            let have: Vec<String> =
                def.params.iter().map(|x| squash(&p.display_ty(x.ty))).collect();
            if !params_fit(&want, &have) {
                continue;
            }
            if let Some(r) = &ret
                && squash(&p.display_ty(def.ret)) != *r
            {
                continue;
            }
            hits.push(json!({ "function": path.join("::"), "signature": self.signature(*f), "at": self.at(def.name_span) }));
        }
        let total = hits.len();
        hits.truncate(50);
        Ok(json!({ "signature": sig, "matches": hits, "total": total }))
    }

    /// `f`, and if it's a trait's method, its impls' methods of its name.
    pub fn with_impls(&self, f: FnId) -> Vec<FnId> {
        let p = self.p;
        let mut fns = vec![f];
        if let FnOwner::Trait(t) = p.func(f).owner {
            for im in p.impls_of(t) {
                fns.extend(
                    p.impl_(*im).methods.iter().filter(|&&m| p.func(m).name == p.func(f).name),
                );
            }
        }
        fns
    }

    /// A trait's impls, or a type's (those for it, whatever its arguments), by index.
    pub fn impl_ids(&self, item: Item) -> Vec<usize> {
        let p = self.p;
        match item {
            Item::Trait(t) => p.impls_of(t).iter().map(|i| i.index()).collect(),
            Item::Adt(a) => (0..p.impls.len())
                .filter(
                    |&i| matches!(p.types.kind(p.impls[i].self_ty), TyKind::Adt(b, _) if *b == a),
                )
                .collect(),
            Item::Fn(_) => Vec::new(),
        }
    }

    /// The types an item's signature (or fields) name, but itself.
    pub fn types_of(&self, item: Item) -> Vec<AdtId> {
        let p = self.p;
        let mut out = Vec::new();
        match item {
            Item::Fn(f) => {
                for prm in &p.func(f).params {
                    adts_in(p, prm.ty, &mut out);
                }
                adts_in(p, p.func(f).ret, &mut out);
            }
            Item::Adt(a) => match &p.adt(a).kind {
                AdtKind::Struct(fields) => {
                    for x in fields {
                        adts_in(p, x.ty, &mut out);
                    }
                }
                AdtKind::Enum(vs) => {
                    for v in vs {
                        for x in &v.fields {
                            adts_in(p, x.ty, &mut out);
                        }
                    }
                }
            },
            Item::Trait(_) => {}
        }
        if let Item::Adt(a) = item {
            out.retain(|b| *b != a);
        }
        out
    }

    /// The functions an item uses (a type's or trait's: its methods'), each once.
    pub fn callees_of(&self, item: Item) -> Vec<FnId> {
        let Item::Fn(f) = item else { return Vec::new() };
        let mut fns = Vec::new();
        if let Some(body) = self.checked.mir.get(&f) {
            for (u, _) in uses(self.p, body) {
                if !fns.contains(&u.func()) && u.func() != f {
                    fns.push(u.func());
                }
            }
        }
        fns
    }

    /// The uses of a function by others.
    pub fn callers_of(&self, f: FnId) -> Vec<(FnId, Span)> {
        self.uses_of(f).into_iter().filter(|(c, _, _)| *c != f).map(|(c, _, s)| (c, s)).collect()
    }

    /// A type's or a trait's impls, as `impl Trait for Type` lines, with where each is.
    pub fn impl_lines(&self, item: Item) -> Vec<String> {
        let p = self.p;
        self.impl_ids(item)
            .into_iter()
            .map(|i| {
                let im = &p.impls[i];
                let what = match &im.trait_ref {
                    Some(t) => {
                        format!("impl {} for {}", p.display_trait_ref(t), p.display_ty(im.self_ty))
                    }
                    None => format!("impl {}", p.display_ty(im.self_ty)),
                };
                let methods: Vec<&str> =
                    im.methods.iter().map(|&m| p.func(m).name.as_str()).collect();
                let methods = if methods.is_empty() {
                    String::new()
                } else {
                    format!(": {}", methods.join(", "))
                };
                format!("{what} ({}){methods}", self.at(im.span))
            })
            .collect()
    }
}

/// `wrela context <item> --budget n`: what an agent needs to change `item`, in at most about
/// `budget` tokens (a token is taken as 4 characters): the item's source with its doc comment;
/// the types its signature names; the signatures of what it calls; where it's called; its
/// impls. Each part goes in whole, in that order, while it fits; what didn't fit is listed.
pub fn context(root: &Path, item: &str, budget: usize) -> Result<String, String> {
    crate::with_checked(root, |checked, sources| {
        let ix = Index::new(checked, sources);
        let (it, path) = ix.one(item)?;
        let mut parts: Vec<(String, String)> = Vec::new();
        let span = it.decl(ix.p).span;
        parts.push((
            format!("`{path}`"),
            format!("// {path} ({})\n{}\n", ix.at(span), ix.source_with_doc(span)),
        ));
        for a in ix.types_of(it) {
            let s = ix.p.adt(a).span;
            parts.push((
                format!("the type `{}`", ix.p.adt(a).name),
                format!("// {} ({})\n{}\n", ix.p.adt(a).name, ix.at(s), ix.source_with_doc(s)),
            ));
        }
        let callees = ix.callees_of(it);
        if !callees.is_empty() {
            let mut text = String::from("// What it calls:\n");
            for f in &callees {
                let def = ix.p.func(*f);
                let doc = ix.summary(def.span);
                let doc = if doc.is_empty() { String::new() } else { format!("  // {doc}") };
                text.push_str(&format!("{}: {}{doc}\n", ix.fn_path(*f), ix.signature(*f)));
            }
            parts.push(("what it calls".into(), text));
        }
        if let Item::Fn(f) = it {
            let callers = ix.callers_of(f);
            if !callers.is_empty() {
                let mut text = String::from("// Where it's called:\n");
                for (c, s) in callers {
                    text.push_str(&format!("{} ({}): {}\n", ix.fn_path(c), ix.at(s), ix.line(s)));
                }
                parts.push(("where it's called".into(), text));
            }
        }
        let impls = ix.impl_lines(it);
        if !impls.is_empty() {
            parts.push(("its impls".into(), format!("// Its impls:\n{}\n", impls.join("\n"))));
        }
        let mut out = String::new();
        let mut left = Vec::new();
        let mut used = 0;
        // The item itself always comes first: cut at a line, if it alone is over the budget.
        if parts[0].1.len().div_ceil(4) > budget {
            let mut cut = String::new();
            for line in parts[0].1.lines() {
                if (cut.len() + line.len() + 1).div_ceil(4) > budget {
                    break;
                }
                cut.push_str(line);
                cut.push('\n');
            }
            cut.push_str("// ... (cut here: the rest is over the budget)\n");
            parts[0].1 = cut;
        }
        for (i, (name, text)) in parts.into_iter().enumerate() {
            let cost = text.len().div_ceil(4);
            if i == 0 || used + cost <= budget {
                used += cost;
                out.push_str(&text);
                out.push('\n');
            } else {
                left.push(name);
            }
        }
        if !left.is_empty() {
            out.push_str(&format!(
                "// Left out to keep within {budget} tokens: {}.\n",
                left.join(", ")
            ));
        }
        Ok(out)
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_read_as_written() {
        assert_eq!(
            parse("type main.wrela:12:9"),
            Ok(Query::Type { file: "main.wrela".into(), line: 12, column: 9 })
        );
        assert_eq!(
            parse("borrows a/b.wrela:3"),
            Ok(Query::Borrows { file: "a/b.wrela".into(), line: 3 })
        );
        assert_eq!(parse("callers main::step"), Ok(Query::Callers("main::step".into())));
        assert!(parse("type main.wrela:12").is_err());
        assert!(parse("wander main").is_err());
        assert!(parse("callers").is_err());
    }

    #[test]
    fn signatures_fit_with_wildcards() {
        let (w, r) = parse_signature("fn(vec3, _) -> f32").unwrap();
        assert_eq!(
            (w.as_slice(), r.as_deref()),
            (["vec3".to_string(), "_".to_string()].as_slice(), Some("f32"))
        );
        assert!(params_fit(&w, &["vec3".into(), "u32".into()]));
        assert!(!params_fit(&w, &["vec3".into()]));
        let (w, _) = parse_signature("(Grid, ..)").unwrap();
        assert!(
            params_fit(&w, &["Grid".into()])
                && params_fit(&w, &["Grid".into(), "f32".into(), "f32".into()])
        );
        assert!(!params_fit(&w, &["f32".into(), "Grid".into()]));
        let (w, r) = parse_signature("-> Vec<f32>").unwrap();
        assert!(params_fit(&w, &["u32".into()]) && r.as_deref() == Some("Vec<f32>"));
        assert_eq!(
            parse_signature("(mut Vec<u32, f32>, take f32)").unwrap().0,
            ["Vec<u32,f32>", "f32"]
        );
        assert!(parse_signature("vec3 -> f32").is_err());
    }
}
