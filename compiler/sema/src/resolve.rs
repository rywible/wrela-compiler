//! Name resolution: module paths, scopes, and type expressions.

use crate::builtins::{BuiltinFn, BuiltinTy};
use crate::defs::*;
use crate::program::Program;
use crate::ty::*;
use std::collections::HashMap;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

/// What's in scope where a type or path is resolved, besides the module's items.
#[derive(Clone, Debug)]
pub struct Scope {
    pub module: ModuleId,
    /// Generic parameters, innermost last.
    pub params: Vec<(String, ParamId)>,
    pub self_ty: Option<TyId>,
    /// The impl whose items these are: `Self::Name` names its associated type.
    pub impl_: Option<ImplId>,
}

impl Scope {
    pub fn new(module: ModuleId) -> Scope {
        Scope { module, params: Vec::new(), self_ty: None, impl_: None }
    }

    /// What's in scope in the items of `owner` in `module`: an impl's parameters, and `Self`,
    /// the type it's for; a trait's `Self` and parameters; nothing for a free function.
    pub fn of_owner(p: &Program, module: ModuleId, owner: FnOwner) -> Scope {
        let mut scope = Scope::new(module);
        match owner {
            FnOwner::Free | FnOwner::Const(_) => {}
            FnOwner::Impl(i) => {
                scope.self_ty = Some(p.impl_(i).self_ty);
                scope.impl_ = Some(i);
                scope.push_params(p, &p.impl_(i).generics);
            }
            FnOwner::Trait(t) => {
                let tr = p.trait_(t);
                scope.self_ty = Some(p.types.param(tr.self_param));
                scope.push_params(p, &tr.generics);
            }
        }
        scope
    }

    /// What a function's signature and body see: its owner's scope, then its own parameters.
    pub fn of_fn(p: &Program, f: FnId) -> Scope {
        let def = p.func(f);
        let mut scope = Scope::of_owner(p, def.module, def.owner);
        scope.push_params(p, &def.generics);
        scope
    }

    pub fn push_params(&mut self, p: &Program, params: &[ParamId]) {
        for &id in params {
            let def = p.param(id);
            if !def.is_self {
                self.params.push((def.name.clone(), id));
            }
        }
    }

    pub fn param(&self, name: &str) -> Option<ParamId> {
        self.params.iter().rev().find(|(n, _)| n == name).map(|(_, p)| *p)
    }
}

/// Where a type expression appears: a trait name means something different in each.
pub enum TyPos<'a> {
    Normal,
    /// A parameter's type: `x: Surface` makes the function generic over it.
    Param(&'a mut ImplicitParams),
    /// A return type: `-> Surface` names the one concrete type the body returns.
    Return(&'a mut Option<Vec<TraitRef>>),
    /// A type alias's type: one that names traits names the type a function returns
    /// (`ty.opaque-alias`); any other is as `Normal`.
    Alias(&'a mut Option<Vec<TraitRef>>),
    /// A local binding's type: a run may be one (§6.6), a trait may not.
    Local,
    /// A generic parameter's bound: a function type may be one (`F: fn(vec3) -> f32`).
    FnBound,
}

impl TyPos<'_> {
    /// Whether a projection type (a run, `str`) may be written here: a parameter's, a
    /// result's or a local binding's type, never a field's or a type argument (§6.6).
    fn allows_runs(&self) -> bool {
        !matches!(self, TyPos::Normal | TyPos::Alias(_))
    }
}

/// The generic parameters a signature's `x: Trait` parameters introduce, numbered after the
/// program's existing ones; the collector adds them to the program in order.
pub struct ImplicitParams {
    first: u32,
    pub defs: Vec<ParamDef>,
}

impl ImplicitParams {
    pub fn new(p: &Program) -> ImplicitParams {
        ImplicitParams { first: p.params.len() as u32, defs: Vec::new() }
    }

    /// A parameter of the signature's own: an `x: Trait` parameter's, or a `take` parameter's
    /// function type's.
    pub fn add_def(&mut self, def: ParamDef) -> ParamId {
        self.defs.push(def);
        ParamId(self.first + self.defs.len() as u32 - 1)
    }
}

pub enum PathLookup {
    Found(Res),
    /// Not found yet: an import that would provide it may still be pending. Says which name it
    /// waits for, in which module's scope.
    NotYet(ModuleId, String),
    /// Names an item that failed to parse: its error is reported.
    Broken,
    Error(Box<Diagnostic>),
}

/// Whether `name` in module `m` is an item that failed to parse (or an import of one).
pub fn is_broken(p: &Program, m: ModuleId, name: &str) -> bool {
    p.module(m).broken.contains(name)
}

/// Where std defines a public item named `name`, if it does, outside the prelude (whose items
/// are in scope already): `std::hash`. An unknown name that's one of std's is usually one that
/// isn't imported.
pub fn std_home(p: &Program, name: &str) -> Option<String> {
    p.modules.iter().enumerate().find_map(|(i, m)| {
        let defined_here = m
            .scope
            .get(name)
            .is_some_and(|b| b.public && res_module(p, b.res) == Some(ModuleId(i as u32)));
        (m.path.first().is_some_and(|f| f == "std")
            && m.path.get(1).is_some_and(|s| s != "prelude")
            && defined_here)
            .then(|| m.path.join("::"))
    })
}

/// The module an item resolution is defined in, for the items that have one.
fn res_module(p: &Program, r: Res) -> Option<ModuleId> {
    match r {
        Res::Fn(f) => Some(p.func(f).module),
        Res::Adt(a) => Some(p.adt(a).module),
        Res::Trait(t) => Some(p.trait_(t).module),
        Res::TraitSet(s) => Some(p.trait_sets[s.index()].module),
        Res::Const(c) => Some(p.const_(c).module),
        _ => None,
    }
}

/// `d` with what fixes an unknown `name` that std has: an import, at the top of the file.
pub fn with_std_import(p: &Program, mut d: Diagnostic, name: &str, at: Span) -> Diagnostic {
    match std_home(p, name) {
        Some(home) => {
            // The import is the fix: what was suggested for a name nothing declares isn't.
            d.fixes.clear();
            d.help.clear();
            // L2 permits a UTF-8 BOM only at the start of a file. Keep an inserted import after
            // it, or applying the fix turns the formerly leading BOM into an invalid character.
            let start =
                u32::from(p.text(Span::new(at.file, 0, 3)).is_some_and(|text| text == "\u{feff}"))
                    * 3;
            d.with_note(format!("`{name}` is in `{home}`, which this module doesn't import"))
                .with_fix(
                    format!("import it: `use {home}::{name}`"),
                    Span::new(at.file, start, start),
                    format!("use {home}::{name}\n"),
                )
        }
        None => d,
    }
}

/// `d` with what fixes an unknown `name` among `candidates`: the closest as a fix ("did you
/// mean"), or, when none is close, a help that lists them after `what` ("its variants are").
/// With no candidates at all, `d` as it is.
pub fn suggest_or_list<'a>(
    d: Diagnostic,
    name: &str,
    at: Span,
    candidates: impl IntoIterator<Item = &'a str>,
    what: &str,
) -> Diagnostic {
    let mut all: Vec<&str> = candidates.into_iter().filter(|c| *c != name).collect();
    all.sort_unstable();
    all.dedup();
    if let Some(s) = closest(name, all.iter().copied()) {
        return d.with_fix(format!("did you mean `{s}`?"), at, s);
    }
    match listed(what, &all) {
        Some(help) => d.with_help(help),
        None => d,
    }
}

/// `what` and then `names` in backticks, at most eight and then `…`; `None` for no names.
pub fn listed(what: &str, names: &[&str]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let mut shown: Vec<String> = names.iter().take(8).map(|n| format!("`{n}`")).collect();
    if names.len() > 8 {
        shown.push("…".into());
    }
    Some(format!("{what} {}", shown.join(", ")))
}

/// The closest of `candidates` to `name` by edit distance, if it's close enough to suggest.
pub fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.len() / 3).max(1);
    let chars: Vec<char> = name.chars().collect();
    candidates
        .filter(|c| *c != name)
        .map(|c| (edit_distance(&chars, c), c))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, c)| c)
}

/// Optimal string alignment distance from the characters `a` to `b`: Levenshtein's, where
/// swapping two neighbouring characters costs one edit too, so the commonest typos (`widht`,
/// `aera`) are one edit from the name.
fn edit_distance(a: &[char], b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    // Rows i - 2, i - 1 and i of the table of distances between prefixes.
    let mut prev2 = vec![0usize; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(prev2[j - 2] + 1);
            }
            cur[j] = best;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The std prelude's module, once std is loaded.
fn prelude(p: &Program) -> Option<ModuleId> {
    p.module(p.std_root?).children.get("prelude").copied()
}

/// The std prelude's names: its public items and the variants of its enums (`Some`, `None`).
pub fn prelude_lookup(p: &Program, name: &str) -> Option<Res> {
    let scope = &p.module(prelude(p)?).scope;
    if let Some(b) = scope.get(name)
        && b.public
    {
        return Some(b.res);
    }
    for b in scope.values() {
        if let Res::Adt(a) = b.res
            && let Some(i) = p.adt(a).variants().iter().position(|v| v.name == name)
        {
            return Some(Res::Variant(a, i as u32));
        }
    }
    None
}

/// A name every module sees after its own: the prelude's, then the built-ins'.
pub fn lookup_global(p: &Program, name: &str) -> Option<Res> {
    prelude_lookup(p, name)
        .or_else(|| BuiltinTy::lookup(name).map(Res::BuiltinTy))
        .or_else(|| BuiltinFn::lookup(name).map(Res::BuiltinFn))
}

/// A name as an item or module path's first segment sees it: the module's own scope, then the
/// prelude, then the built-ins.
pub fn lookup_name(p: &Program, m: ModuleId, name: &str) -> Option<Res> {
    match p.module(m).scope.get(name) {
        Some(b) => Some(b.res),
        None => lookup_global(p, name),
    }
}

/// The names visible in `m` that name types: structs, enums, traits and built-in types
/// (and modules, which a type's path can start with).
pub fn visible_type_names(p: &Program, m: ModuleId) -> Vec<String> {
    let is_type = |r: &Res| {
        matches!(
            r,
            Res::Adt(_)
                | Res::Trait(_)
                | Res::TraitSet(_)
                | Res::BuiltinTy(_)
                | Res::Module(_)
                | Res::Alias(_)
        )
    };
    let mut out: Vec<String> =
        p.module(m).scope.iter().filter(|(_, b)| is_type(&b.res)).map(|(n, _)| n.clone()).collect();
    if let Some(prelude) = prelude(p) {
        let scope = &p.module(prelude).scope;
        out.extend(scope.iter().filter(|(_, b)| is_type(&b.res)).map(|(n, _)| n.clone()));
    }
    out
}

/// Every name visible in a module, for suggestions.
pub fn visible_names(p: &Program, m: ModuleId) -> Vec<String> {
    let mut out: Vec<String> = p.module(m).scope.keys().cloned().collect();
    if let Some(prelude) = prelude(p) {
        let scope = &p.module(prelude).scope;
        out.extend(scope.keys().cloned());
        for b in scope.values() {
            if let Res::Adt(a) = b.res {
                out.extend(p.adt(a).variants().iter().map(|v| v.name.clone()));
            }
        }
    }
    out
}

/// How a diagnostic names what `r` is: "the constant `X`", "the type `T`".
pub(crate) fn describe_res(p: &Program, r: Res) -> String {
    match r {
        Res::Module(m) => format!("the module `{}`", p.module(m).name()),
        Res::Adt(a) => format!("the type `{}`", p.adt(a).name),
        Res::Trait(t) => format!("the trait `{}`", p.trait_(t).name),
        Res::TraitSet(s) => format!("the trait set `{}`", p.trait_sets[s.index()].name),
        Res::Fn(f) => format!("the function `{}`", p.func(f).name),
        Res::Const(c) => format!("the constant `{}`", p.const_(c).name),
        Res::Alias(a) => format!("the type `{}`", p.aliases[a.index()].name),
        Res::Variant(a, v) => {
            format!("the variant `{}::{}`", p.adt(a).name, p.adt(a).variants()[v as usize].name)
        }
        Res::BuiltinTy(_) => "a built-in type".into(),
        Res::BuiltinFn(f) => format!("the built-in function `{}`", f.name()),
    }
}

/// A segment of a path as module path resolution reads it: a name, without generic arguments.
pub trait PathSeg {
    fn ident(&self) -> &ast::Ident;
}

impl PathSeg for ast::Ident {
    fn ident(&self) -> &ast::Ident {
        self
    }
}

impl PathSeg for ast::PathSegment {
    fn ident(&self) -> &ast::Ident {
        &self.ident
    }
}

/// Resolves a path of plain names (a `use` path, or a path's module prefix) from module `from`.
/// The first segment is `std`, a name in `from`'s scope, a top-level module of `from`'s
/// package, one of its package's dependencies, or a prelude name. Before the `final_pass`, a
/// name that isn't found is `NotYet`: an import may still bind it.
pub fn resolve_module_path_in(
    p: &Program,
    from: ModuleId,
    segs: &[impl PathSeg],
    final_pass: bool,
) -> PathLookup {
    let first = segs[0].ident();
    let package = p.package_of(from);
    let top = |name: &str| p.module(package.root).children.get(name).copied();
    let dep = |name: &str| package.deps.get(name).map(|d| p.packages[d.index()].root);
    let mut res = if first.name == "std" {
        match p.std_root {
            Some(s) => Res::Module(s),
            None => {
                return PathLookup::Error(Box::new(Diagnostic::new(
                    codes::E0202,
                    first.span,
                    "std isn't loaded",
                )));
            }
        }
    } else if let Some(b) = p.module(from).scope.get(&first.name).filter(|b| {
        // `use blob::blob` imports the function `blob` from the module `blob`; a later
        // `blob::x` still means the module, since a function has no items.
        segs.len() == 1
            || matches!(b.res, Res::Module(_))
            || matches!(b.res, Res::Adt(a) if p.adt(a).is_enum())
            || top(&first.name).is_none() && dep(&first.name).is_none()
    }) {
        b.res
    } else if let Some(m) = top(&first.name) {
        Res::Module(m)
    } else if let Some(m) = dep(&first.name) {
        Res::Module(m)
    } else if let Some(r) = lookup_global(p, &first.name) {
        r
    } else if is_broken(p, from, &first.name) {
        return PathLookup::Broken;
    } else {
        if !final_pass {
            return PathLookup::NotYet(from, first.name.clone());
        }
        let mut d = Diagnostic::new(
            codes::E0202,
            first.span,
            format!("there's no module or item `{}` here", first.name),
        );
        if crate::OUTPUT_DIRS.contains(&first.name.as_str()) {
            d = d.with_note(format!(
                "a `{}/` directory at the package's top level holds outputs, so it's never a module",
                first.name
            ));
        }
        let mut names = visible_names(p, from);
        names.extend(p.module(package.root).children.keys().cloned());
        names.extend(package.deps.keys().cloned());
        if let Some(s) = closest(&first.name, names.iter().map(String::as_str)) {
            d = d.with_fix(format!("did you mean `{s}`?"), first.span, s);
        } else {
            d = d.with_help(format!(
                "a module is a file of the package (`{0}.wrela` is `{0}`), and another package is named in `wrela.toml`'s `[dependencies]`",
                first.name
            ));
        }
        if segs.len() == 1 {
            d = with_std_import(p, d, &first.name, first.span);
        }
        return PathLookup::Error(Box::new(d));
    };
    for seg in &segs[1..] {
        let seg = seg.ident();
        res = match res {
            Res::Module(m) => {
                let module = p.module(m);
                if let Some(b) = module.scope.get(&seg.name) {
                    if b.public && b.package_only && !p.same_package(m, from) {
                        let d = Diagnostic::new(
                            codes::E0203,
                            seg.span,
                            format!(
                                "`{}` is visible only inside the package `{}`",
                                seg.name,
                                p.package_of(m).name
                            ),
                        )
                        .with_secondary(b.span, "declared `pub(package)` here")
                        .with_note("only a package's `pub` items cross to the packages that depend on it (§3)")
                        .with_help("make it `pub` in its package, or use a `pub` item of that package instead");
                        return PathLookup::Error(Box::new(d));
                    }
                    if !b.public && m != from {
                        let d = Diagnostic::new(
                            codes::E0203,
                            seg.span,
                            format!("`{}` is private to `{}`", seg.name, module.name()),
                        )
                        .with_secondary(b.span, "declared here without `pub`")
                        .with_help(format!("mark it `pub` in `{}` to use it here", module.name()));
                        return PathLookup::Error(Box::new(d));
                    }
                    b.res
                } else if let Some(&c) = module.children.get(&seg.name) {
                    Res::Module(c)
                } else if module.broken.contains(&seg.name) {
                    return PathLookup::Broken;
                } else {
                    if !final_pass {
                        return PathLookup::NotYet(m, seg.name.clone());
                    }
                    let d = Diagnostic::new(
                        codes::E0200,
                        seg.span,
                        format!("`{}` has no item `{}`", module.name(), seg.name),
                    );
                    // Only what this module may name: not another module's private items and
                    // imports, nor another package's `pub(package)` ones (E0203).
                    let visible = |b: &Binding| {
                        m == from || (b.public && (!b.package_only || p.same_package(m, from)))
                    };
                    let names = module
                        .scope
                        .iter()
                        .filter(|(_, b)| visible(b))
                        .map(|(n, _)| n.as_str())
                        .chain(module.children.keys().map(String::as_str));
                    let mut d = suggest_or_list(d, &seg.name, seg.span, names, "its items are");
                    if d.help.is_empty() && d.fixes.is_empty() {
                        let (m, x) = (module.name(), &seg.name);
                        d = d.with_help(if module.scope.is_empty() && module.children.is_empty() {
                            format!("`{m}` has nothing in it yet: declare `{x}` there")
                        } else {
                            format!("nothing in `{m}` is visible here: declare `{x}` there, marked `pub`")
                        });
                    }
                    return PathLookup::Error(Box::new(d));
                }
            }
            Res::Adt(a) if p.adt(a).is_enum() => {
                match p.adt(a).variants().iter().position(|v| v.name == seg.name) {
                    Some(i) => Res::Variant(a, i as u32),
                    None => {
                        let d = Diagnostic::new(
                            codes::E0211,
                            seg.span,
                            format!("`{}` has no variant `{}`", p.adt(a).name, seg.name),
                        );
                        let d = suggest_or_list(
                            d,
                            &seg.name,
                            seg.span,
                            p.adt(a).variants().iter().map(|v| v.name.as_str()),
                            "its variants are",
                        );
                        return PathLookup::Error(Box::new(d));
                    }
                }
            }
            other => {
                let d = Diagnostic::new(
                    codes::E0202,
                    seg.span,
                    format!(
                        "{} isn't a module, so it has no item `{}`",
                        describe_res(p, other),
                        seg.name
                    ),
                );
                return PathLookup::Error(Box::new(d));
            }
        };
    }
    PathLookup::Found(res)
}

/// What a path names, in the final pass: `None` if it names nothing, with its error (if it has
/// one) added to `diags`.
pub fn lookup_or_report(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    from: ModuleId,
    segs: &[impl PathSeg],
) -> Option<Res> {
    match resolve_module_path_in(p, from, segs, true) {
        PathLookup::Found(r) => Some(r),
        PathLookup::Error(d) => {
            diags.push(*d);
            None
        }
        PathLookup::NotYet(..) | PathLookup::Broken => None,
    }
}

/// E0322: `name` is written with generic arguments, and names something that takes none.
pub(crate) fn no_generics(name: &ast::Ident) -> Diagnostic {
    Diagnostic::new(codes::E0322, name.span, format!("`{}` takes no generic arguments", name.name))
        .with_help(format!("write `{}` without `<...>`", name.name))
}

/// E0322: `name` takes `want` generic arguments, but `given` are written.
pub(crate) fn wrong_generic_count(span: Span, name: &str, want: usize, given: usize) -> Diagnostic {
    let s = wrela_diag::plural(want);
    Diagnostic::new(
        codes::E0322,
        span,
        format!("`{name}` takes {want} generic argument{s}, not {given}"),
    )
    .with_help(if want == 0 {
        format!("write `{name}` without `<...>`")
    } else {
        format!("give `{name}` exactly {want}: `{name}<{}>`", vec!["_"; want].join(", "))
    })
}

/// Resolves one argument for declared generic parameter `param`, enforcing whether it is a type
/// or a `const u32`. Generic arguments share type syntax, so this check must happen at every use
/// rather than relying on [`resolve_type`].
pub(crate) fn resolve_generic_arg(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    arg: &ast::TypeExpr,
    param: ParamId,
) -> TyId {
    if !p.param(param).is_const {
        if let ast::TypeExprKind::Path(path) = &arg.kind
            && path.is_single()
            && let Some(g) = scope.param(&path.segments[0].ident.name)
            && p.param(g).is_const
        {
            diags.push(
                Diagnostic::new(
                    codes::E0212,
                    arg.span,
                    format!("`{}` is a constant parameter, not a type", p.param(g).name),
                )
                .with_help(format!(
                    "`{}` is a type parameter: pass a type here, such as `f32`",
                    p.param(param).name
                )),
            );
            return p.types.error;
        }
        return resolve_type(p, diags, scope, arg, TyPos::Normal);
    }

    match &arg.kind {
        ast::TypeExprKind::Int(lit) => {
            if let ast::LitKind::Int(v) = &lit.kind
                && let Some(v) = v.ok().and_then(|v| u32::try_from(v).ok())
            {
                return p.types.intern(TyKind::ConstU32(v));
            }
            diags.push(
                Diagnostic::new(codes::E0212, arg.span, "a const argument is a `u32`")
                    .with_help("pass a whole number from 0 to 4294967295"),
            );
            return p.types.error;
        }
        ast::TypeExprKind::Path(path) => {
            if let Some(seg) = path.segments.iter().find(|s| s.generics.is_some()) {
                diags.push(no_generics(&seg.ident));
                return p.types.error;
            }
            if path.is_single()
                && let Some(g) = scope.param(&path.segments[0].ident.name)
            {
                if p.param(g).is_const {
                    return p.types.param(g);
                }
                diags.push(
                    Diagnostic::new(
                        codes::E0212,
                        arg.span,
                        format!("`{}` is a type parameter, not a constant", p.param(g).name),
                    )
                    .with_help(format!(
                        "`{}` is `const {}: u32`: pass a number, a `u32` constant or a const parameter, or declare `{}` as `const {}: u32`",
                        p.param(param).name,
                        p.param(param).name,
                        p.param(g).name,
                        p.param(g).name
                    )),
                );
                return p.types.error;
            }
            if let Some(Res::Const(c)) = resolve_value_item(p, scope.module, path) {
                if let Some(n) = const_value_u32(p, c) {
                    return p.types.intern(TyKind::ConstU32(n));
                }
                let e = ast::Expr::new(ast::ExprKind::Path(path.clone()), path.span);
                diags.push(length_error(p, scope.module, &e));
                return p.types.error;
            }
        }
        _ => {}
    }
    diags.push(
        Diagnostic::new(
            codes::E0212,
            arg.span,
            "a const argument is a number, a `u32` constant or a const parameter",
        )
        .with_help(format!(
            "`{}` is `const {}: u32`, a number, not a type: pass one such as `4`",
            p.param(param).name,
            p.param(param).name
        )),
    );
    p.types.error
}

/// The generic arguments written on `seg`, for the parameters `params`; one past them is
/// resolved as a type, for the caller to count.
fn written_args(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    seg: &ast::PathSegment,
    params: &[ParamId],
) -> Vec<TyId> {
    seg.generics
        .iter()
        .flatten()
        .enumerate()
        .map(|(i, a)| written_arg(p, diags, scope, a, params.get(i).copied()))
        .collect()
}

/// One written generic argument, for the parameter `param` if there is one.
fn written_arg(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    arg: &ast::TypeExpr,
    param: Option<ParamId>,
) -> TyId {
    match param {
        Some(param) => resolve_generic_arg(p, diags, scope, arg, param),
        None => resolve_type(p, diags, scope, arg, TyPos::Normal),
    }
}

/// Resolves a bound, a supertrait or an opt-in: a trait, or a trait set's traits (§7), with
/// their generic arguments. Empty after an error.
pub fn resolve_trait_refs(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    te: &ast::TypeExpr,
) -> Vec<TraitRef> {
    if let ast::TypeExprKind::Path(path) = &te.kind
        && let Some(Res::TraitSet(s)) = resolve_value_item(p, scope.module, path)
    {
        return set_traits(p, diags, scope, te, path, s).unwrap_or_default();
    }
    resolve_trait_ref(p, diags, scope, te).into_iter().collect()
}

/// A trait set's traits, with the arguments `path` gives its parameters.
fn set_traits(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    te: &ast::TypeExpr,
    path: &ast::Path,
    s: TraitSetId,
) -> Option<Vec<TraitRef>> {
    let def = &p.trait_sets[s.index()];
    let last = &path.segments[path.segments.len() - 1];
    let args = written_args(p, diags, scope, last, &def.generics);
    if args.len() != def.generics.len() {
        diags.push(wrong_generic_count(te.span, &def.name, def.generics.len(), args.len()));
        return None;
    }
    let subst = Subst::from_pairs(&def.generics, &args);
    Some(def.traits.iter().map(|r| r.subst(&p.types, &subst)).collect())
}

/// Resolves a trait where exactly one is needed (an impl's): a path naming a trait, with its
/// generic arguments. A trait set is E0419.
pub fn resolve_trait_ref(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    te: &ast::TypeExpr,
) -> Option<TraitRef> {
    let ast::TypeExprKind::Path(path) = &te.kind else {
        diags.push(Diagnostic::new(codes::E0400, te.span, "expected a trait"));
        return None;
    };
    let res = lookup_or_report(p, diags, scope.module, &path.segments)?;
    if let Res::TraitSet(s) = res {
        let def = &p.trait_sets[s.index()];
        let names: Vec<String> =
            def.traits.iter().map(|r| format!("`{}`", p.trait_(r.trait_).name)).collect();
        diags.push(
            Diagnostic::new(
                codes::E0419,
                te.span,
                format!("`{}` is a trait set, so it can't be implemented", def.name),
            )
            .with_note(format!("it names {}", names.join(", ")))
            .with_help("implement each of its traits, or declare the ones a type opts in to"),
        );
        return None;
    }
    let Res::Trait(t) = res else {
        diags.push(
            Diagnostic::new(
                codes::E0400,
                te.span,
                format!("{} isn't a trait", describe_res(p, res)),
            )
            .with_help("name a trait here: `wrela doc` lists them")
            .with_note("a bound or an opt-in names a trait, such as `Copy` or `Surface`"),
        );
        return None;
    };
    let last = &path.segments[path.segments.len() - 1];
    let args = written_args(p, diags, scope, last, &p.trait_(t).generics);
    let (name, want) = (&p.trait_(t).name, p.trait_(t).generics.len());
    if args.len() != want {
        diags.push(wrong_generic_count(te.span, name, want, args.len()));
        return None;
    }
    Some(TraitRef { trait_: t, args })
}

/// Every trait a generic parameter is bounded by, including supertraits, with arguments.
pub fn param_bounds_closure(p: &Program, param: ParamId) -> Vec<TraitRef> {
    let mut out: Vec<TraitRef> = Vec::new();
    let self_ty = p.types.param(param);
    for b in &p.param(param).bounds {
        add_with_supertraits(p, self_ty, b.clone(), &mut out);
    }
    out
}

/// Adds `r`, a trait `self_ty` has, and, transitively, its supertraits (with `Self` and
/// arguments substituted). A cycle of supertraits (E0411) is followed once around: bounds are
/// resolved before the cycles are found and cut, and a cycle whose arguments grow
/// (`trait A<T>: B<W<T>>`, `trait B<T>: A<T>`) would never repeat a bound.
pub fn add_with_supertraits(p: &Program, self_ty: TyId, r: TraitRef, out: &mut Vec<TraitRef>) {
    fn go(
        p: &Program,
        self_ty: TyId,
        r: TraitRef,
        out: &mut Vec<TraitRef>,
        path: &mut Vec<TraitId>,
    ) {
        if out.contains(&r) || path.contains(&r.trait_) {
            return;
        }
        let tr = p.trait_(r.trait_);
        let mut subst = Subst::from_pairs(&tr.generics, &r.args);
        subst.insert(tr.self_param, self_ty);
        path.push(r.trait_);
        out.push(r);
        for s in &tr.supertraits {
            go(p, self_ty, s.subst(&p.types, &subst), out, path);
        }
        path.pop();
    }
    go(p, self_ty, r, out, &mut Vec::new());
}

/// E0327: a run (`what`) where a value is stored: a field, a type argument.
fn run_stored(span: Span, what: &str) -> Diagnostic {
    Diagnostic::new(codes::E0327, span, format!("{what} is a projection, so it can't be stored here"))
        .with_note("a run borrows its caller's data: it's a parameter's, a result's or a local binding's type, never a field or a type argument (§6.6)")
        .with_help("store an owned `Vec<T>` or `String`, a handle, or group projections in a `borrow struct`")
}

/// Resolves a type expression.
pub fn resolve_type(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    te: &ast::TypeExpr,
    pos: TyPos,
) -> TyId {
    match &te.kind {
        // Its syntax error is reported.
        ast::TypeExprKind::Error => p.types.error,
        ast::TypeExprKind::Paren(inner) => resolve_type(p, diags, scope, inner, pos),
        ast::TypeExprKind::Tuple(elems) => {
            let ts =
                elems.iter().map(|e| resolve_type(p, diags, scope, e, TyPos::Normal)).collect();
            p.types.tuple(ts)
        }
        ast::TypeExprKind::Array(elem, len) => {
            let e = resolve_type(p, diags, scope, elem, TyPos::Normal);
            let len = len.as_deref().map(unparen);
            if let Some(len) = len
                && let ast::ExprKind::Path(path) = &len.kind
                && let Some(seg) = path.segments.iter().find(|s| s.generics.is_some())
            {
                diags.push(no_generics(&seg.ident));
                return p.types.error;
            }
            // A length that's a `const N: u32` parameter (§4).
            if let Some(l) = len
                && let ast::ExprKind::Path(path) = &l.kind
                && path.is_single()
                && let Some(g) = scope.param(&path.segments[0].ident.name)
                && p.param(g).is_const
            {
                return p.types.intern(TyKind::ArrayN(e, p.types.param(g)));
            }
            match len {
                Some(len) => match const_u32(p, scope.module, len) {
                    Some(n) => p.types.array(e, n),
                    None => {
                        diags.push(length_error(p, scope.module, len));
                        p.types.error
                    }
                },
                None => {
                    if !pos.allows_runs() {
                        diags.push(run_stored(te.span, "a run `[T]`"));
                        return p.types.error;
                    }
                    p.types.intern(TyKind::Slice(e))
                }
            }
        }
        ast::TypeExprKind::Fn(f) => {
            // Only a written type; a body's inferred types are checked once it's finished.
            if !matches!(pos, TyPos::Param(_) | TyPos::FnBound) {
                diags.push(
                    Diagnostic::new(codes::E0510, te.span, "a function type can only be a parameter's type or a struct field's")
                        .with_note("a struct with a field of a function type is generic over it, and so is a function with a `take` parameter of one (§6.7)")
                        .with_help("hold it in a struct's field, or keep work for later as an enum of actions you `match` on"),
                );
                return p.types.error;
            }
            let mut flags = FnFlags::default();
            for a in &f.attrs {
                if let Some(with) = flags.with_attr(&a.name) {
                    flags = with;
                } else {
                    diags.push(
                        Diagnostic::new(
                            codes::E0204,
                            a.span,
                            format!("`@{}` isn't an attribute of function types", a.name),
                        )
                        .with_note(
                            "a function type takes `@deterministic`, `@parallel` or `@audio` (§9)",
                        ),
                    );
                }
            }
            if f.params.len() > FnFlags::MAX_MODES {
                diags.push(Diagnostic::new(
                    codes::E0702,
                    te.span,
                    format!("a function type has at most {} parameters", FnFlags::MAX_MODES),
                ));
            }
            let mut ps = Vec::new();
            for (i, fp) in f.params.iter().enumerate() {
                // A parameter of a function type may be a run, as a function's may (§6.6).
                ps.push(resolve_type(p, diags, scope, &fp.ty, TyPos::Local));
                flags = flags.with_mode(i, fp.mode);
            }
            let r = match &f.ret {
                Some(r) => resolve_type(p, diags, scope, r, TyPos::Normal),
                None => p.types.unit,
            };
            p.types.intern(TyKind::FnPtr(ps, r, flags))
        }
        ast::TypeExprKind::Int(_) => {
            diags.push(
                Diagnostic::new(
                    codes::E0212,
                    te.span,
                    "a number is a constant generic argument, not a type",
                )
                .with_help(
                    "a type goes here, such as `f32`; a number is for a `const N: u32` parameter",
                ),
            );
            p.types.error
        }
        ast::TypeExprKind::Path(path) => resolve_type_path(p, diags, scope, te, path, pos),
        ast::TypeExprKind::Traits(list) => {
            let before = diags.len();
            let refs: Vec<TraitRef> =
                list.iter().flat_map(|t| resolve_trait_refs(p, diags, scope, t)).collect();
            if diags[before..].iter().any(|d| d.is_error()) || refs.is_empty() {
                return p.types.error;
            }
            let names: Vec<&str> = refs.iter().map(|r| p.trait_(r.trait_).name.as_str()).collect();
            let names = names.join(" + ");
            traits_as_type(p, diags, pos, refs, &names, te.span, || {
                Diagnostic::new(codes::E0410, te.span, format!("`{names}` are traits, so they aren't a type here"))
                    .with_note("traits name a type only as a parameter's type (any type with them) or a return type (the one type the body returns)")
                    .with_help("add a generic parameter, as in `struct S<T: A + B> { x: T }`")
            })
        }
    }
}

/// `e` without the parentheses around it.
fn unparen(mut e: &ast::Expr) -> &ast::Expr {
    while let ast::ExprKind::Paren(inner) = &e.kind {
        e = inner;
    }
    e
}

/// Why `len`, written as an array length in `module`, isn't one: a path that names nothing
/// usable (a private constant, an unknown name), a literal past `u32`, or what a length may be.
pub fn length_error(p: &Program, module: ModuleId, len: &ast::Expr) -> Diagnostic {
    match &unparen(len).kind {
        ast::ExprKind::Path(path) if !path.is_single() => {
            if let PathLookup::Error(d) = resolve_module_path_in(p, module, &path.segments, true) {
                return *d;
            }
        }
        ast::ExprKind::Path(path) => {
            let name = &path.segments[0].ident.name;
            let found = lookup_name(p, module, name);
            if let Some(Res::Adt(a)) = found {
                let def = p.adt(a);
                return if def.is_enum() {
                    Diagnostic::new(
                        codes::E0325,
                        len.span,
                        format!("`{name}` can't be an array's length: an enum is one when its variants hold nothing and its discriminants are 0, 1, 2 and on"),
                    )
.with_help("give the array a number or a `const N: u32` as its length, or make the enum's variants hold nothing")
                    .with_note("each variant's discriminant is its element's index (§4)")
                } else {
                    Diagnostic::new(
                        codes::E0325,
                        len.span,
                        format!(
                            "`{name}` is a struct; an array's length is a number, a constant holding one, or an enum (one element a variant)"
                        ),
                    )
.with_help("give the array a number, a `const N: u32`, or a fieldless enum as its length")
                };
            }
            if found.is_none() && !is_broken(p, module, name) {
                return Diagnostic::new(
                    codes::E0200,
                    len.span,
                    format!("there's no constant `{name}` here"),
                )
                .with_help(format!(
                    "declare it (`const {name}: u32 = 4`), or write the number itself"
                ));
            }
        }
        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(_), .. }) => {
            return Diagnostic::new(
                codes::E0325,
                len.span,
                format!("an array length is at most {} (a `u32`)", u32::MAX),
            )
            .with_help("use a `Vec` for this many elements");
        }
        _ => {}
    }
    Diagnostic::new(
        codes::E0325,
        len.span,
        "an array length is an integer constant expression with no calls",
    )
    .with_help("write the length as a literal or a constant: `const N: u32 = 8`")
    .with_note("its value must fit a `u32`, without overflow or division by zero")
}

/// How many variants enum `a` has, when it can be an array's length: its variants hold nothing
/// and their discriminants are 0, 1, 2 and on, so each is its own element's index.
pub fn enum_length(p: &Program, a: AdtId) -> Option<u32> {
    let def = p.adt(a);
    let dense = def.is_enum()
        && def
            .variants()
            .iter()
            .enumerate()
            .all(|(i, v)| v.shape == VariantShape::Unit && v.discriminant == i as u32);
    dense.then_some(def.variants().len() as u32)
}

/// The value of an array length: integer arithmetic without calls, resolved in `module`;
/// a constant's own value is resolved in the module that defines it, with its own integer
/// type. Unconstrained literals in a length use `u32`. `None` for invalid operations, a
/// negative result, a result past `u32`, or a constant cycle.
pub fn const_u32(p: &Program, module: ModuleId, e: &ast::Expr) -> Option<u32> {
    let mut constants = IntegerConstants::default();
    let ty = integer_hint(p, module, e, &mut constants)?.unwrap_or(IntTy::U32);
    u32::try_from(integer_value(p, module, e, ty, &mut constants)?).ok()
}

/// The value of constant `c` as an array length: [`const_u32`] of its value.
fn const_value_u32(p: &Program, c: ConstId) -> Option<u32> {
    u32::try_from(integer_constant(p, c, &mut IntegerConstants::default())?.0).ok()
}

/// Each referenced constant is evaluated once, even when many expressions share it.
#[derive(Default)]
struct IntegerConstants {
    visiting: Vec<ConstId>,
    values: HashMap<ConstId, Option<(i128, IntTy)>>,
}

/// A constant's integer value and type, before body checking has resolved all constant types.
fn integer_constant(
    p: &Program,
    c: ConstId,
    constants: &mut IntegerConstants,
) -> Option<(i128, IntTy)> {
    if let Some(&value) = constants.values.get(&c) {
        return value;
    }
    if constants.visiting.contains(&c) {
        return None;
    }
    constants.visiting.push(c);
    let result = (|| {
        let def = p.const_(c);
        // A later declaration's annotation may not have been resolved yet. Resolve a scalar
        // path here; aggregate annotations can't be integers and may depend on lengths.
        let written = match (&def.ty, &def.annotation) {
            (Some(t), _) => Some(*t),
            (None, Some(annotation)) => {
                let mut annotation = annotation;
                while let ast::TypeExprKind::Paren(inner) = &annotation.kind {
                    annotation = inner;
                }
                if !matches!(annotation.kind, ast::TypeExprKind::Path(_)) {
                    return None;
                }
                Some(resolve_type(
                    p,
                    &mut Vec::new(),
                    &Scope::new(def.module),
                    annotation,
                    TyPos::Normal,
                ))
            }
            (None, None) => None,
        };
        let ty = match written {
            Some(t) => match p.types.kind(t) {
                TyKind::Int(ty) => *ty,
                _ => return None,
            },
            None => integer_hint(p, def.module, &def.value, constants)?.unwrap_or(IntTy::I32),
        };
        Some((integer_value(p, def.module, &def.value, ty, constants)?, ty))
    })();
    constants.visiting.pop();
    constants.values.insert(c, result);
    result
}

/// The concrete integer type imposed by an expression's constants, if any. Literal types
/// stay open, as they do in body inference. `None` rejects non-integers and conflicting types.
fn integer_hint(
    p: &Program,
    module: ModuleId,
    e: &ast::Expr,
    constants: &mut IntegerConstants,
) -> Option<Option<IntTy>> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(_), .. }) => Some(None),
        ast::ExprKind::Paren(inner) | ast::ExprKind::Unary(_, inner) => {
            integer_hint(p, module, inner, constants)
        }
        ast::ExprKind::Binary(ast::BinOp::Shl | ast::BinOp::Shr | ast::BinOp::Pow, a, _) => {
            integer_hint(p, module, a, constants)
        }
        ast::ExprKind::Binary(_, a, b) => {
            let (a, b) =
                (integer_hint(p, module, a, constants)?, integer_hint(p, module, b, constants)?);
            if a.is_some() && b.is_some() && a != b { None } else { Some(a.or(b)) }
        }
        ast::ExprKind::Path(path) => {
            if path.segments.iter().any(|s| s.generics.is_some()) {
                return None;
            }
            match resolve_value_item(p, module, path)? {
                Res::Const(c) => Some(Some(integer_constant(p, c, constants)?.1)),
                Res::Adt(a) => enum_length(p, a).map(|_| Some(IntTy::U32)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Evaluates only the integer operations available without calls. Each intermediate value
/// must fit its inferred type. Shifts truncate bits as the CPU does and reject large amounts.
fn integer_value(
    p: &Program,
    module: ModuleId,
    e: &ast::Expr,
    ty: IntTy,
    constants: &mut IntegerConstants,
) -> Option<i128> {
    let min = if ty.signed() { -(1i128 << (ty.bits() - 1)) } else { 0 };
    let max = i128::from(ty.max());
    let truncate = |value: i128| {
        let bits = value & ((1i128 << ty.bits()) - 1);
        if ty.signed() && bits > max { bits - (1i128 << ty.bits()) } else { bits }
    };
    let value = match &e.kind {
        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. }) => i128::from(v.ok()?),
        ast::ExprKind::Paren(inner) => integer_value(p, module, inner, ty, constants)?,
        ast::ExprKind::Unary(ast::UnOp::Neg, inner) if ty.signed() => {
            // A negative literal can name the signed minimum, whose positive part doesn't fit.
            match &unparen(inner).kind {
                ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. }) => {
                    -i128::from(v.ok()?)
                }
                _ => integer_value(p, module, inner, ty, constants)?.checked_neg()?,
            }
        }
        ast::ExprKind::Unary(ast::UnOp::Not, inner) => {
            truncate(!integer_value(p, module, inner, ty, constants)?)
        }
        ast::ExprKind::Binary(op, a, b) => {
            let a = integer_value(p, module, a, ty, constants)?;
            let amount = matches!(op, ast::BinOp::Shl | ast::BinOp::Shr | ast::BinOp::Pow);
            let b = integer_value(p, module, b, if amount { IntTy::U32 } else { ty }, constants)?;
            match op {
                ast::BinOp::Add => a.checked_add(b)?,
                ast::BinOp::Sub => a.checked_sub(b)?,
                ast::BinOp::Mul => a.checked_mul(b)?,
                ast::BinOp::Div => a.checked_div(b)?,
                ast::BinOp::Rem => a.checked_rem(b)?,
                ast::BinOp::Pow => a.checked_pow(u32::try_from(b).ok()?)?,
                ast::BinOp::Shl if b < i128::from(ty.bits()) => truncate(a << b),
                ast::BinOp::Shr if b < i128::from(ty.bits()) => a >> b,
                ast::BinOp::BitAnd => a & b,
                ast::BinOp::BitOr => a | b,
                ast::BinOp::BitXor => a ^ b,
                _ => return None,
            }
        }
        ast::ExprKind::Path(path) if path.segments.iter().all(|s| s.generics.is_none()) => {
            match resolve_value_item(p, module, path)? {
                Res::Const(c) => {
                    let (value, actual) = integer_constant(p, c, constants)?;
                    if ty != actual {
                        return None;
                    }
                    value
                }
                Res::Adt(a) => i128::from(enum_length(p, a)?),
                _ => return None,
            }
        }
        _ => return None,
    };
    (min <= value && value <= max).then_some(value)
}

/// The item a value path names from `module` (`N`, `a::inner::N`), if it names one; generic
/// arguments are ignored. A constant it names is used (W0008): every caller is code that
/// names it, in an expression, a type, a length, an attribute or another constant's value.
pub fn resolve_value_item(p: &Program, module: ModuleId, path: &ast::Path) -> Option<Res> {
    let r = if path.is_single() {
        lookup_name(p, module, &path.segments[0].ident.name)
    } else {
        match resolve_module_path_in(p, module, &path.segments, true) {
            PathLookup::Found(r) => Some(r),
            _ => None,
        }
    };
    if let Some(Res::Const(c)) = r {
        p.note_const_use(c);
    }
    r
}

fn resolve_type_path(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    te: &ast::TypeExpr,
    path: &ast::Path,
    pos: TyPos,
) -> TyId {
    let segs = &path.segments;
    let last = &segs[segs.len() - 1];
    // `T`, `Self`, `T::Kind`, `Self::Out`.
    let first = &segs[0].ident.name;
    let base_param = if first == "Self" {
        match scope.self_ty {
            Some(t) => Some(t),
            None => {
                diags.push(Diagnostic::new(
                    codes::E0200,
                    segs[0].ident.span,
                    "`Self` only exists inside an `impl` or a `trait`",
                ));
                return p.types.error;
            }
        }
    } else if let Some(id) = scope.param(first) {
        if p.param(id).is_const {
            diags.push(
                Diagnostic::new(
                    codes::E0212,
                    segs[0].ident.span,
                    format!("`{first}` is a constant parameter, not a type"),
                )
                .with_help(format!(
                    "`{first}` is a number: use it as a length (`[f32; {first}]`) or a value, or declare a type parameter for the type"
                )),
            );
            return p.types.error;
        }
        Some(p.types.param(id))
    } else {
        None
    };
    if let Some(base) = base_param {
        diags.extend(segs.iter().filter(|s| s.generics.is_some()).map(|s| no_generics(&s.ident)));
        if segs.len() == 1 {
            return base;
        }
        if segs.len() == 2 {
            return projection(p, diags, scope, base, &segs[1].ident);
        }
        diags.push(Diagnostic::new(
            codes::E0408,
            te.span,
            "a projection is one name deep: `T::Name`",
        ));
        return p.types.error;
    }
    let res = if segs.len() == 1 {
        match lookup_name(p, scope.module, first) {
            Some(r) => r,
            None if is_broken(p, scope.module, first) => return p.types.error,
            None => {
                let mut d = Diagnostic::new(
                    codes::E0200,
                    segs[0].ident.span,
                    format!("there's no type `{first}` here"),
                );
                let mut names = visible_type_names(p, scope.module);
                names.extend(scope.params.iter().map(|(n, _)| n.clone()));
                names.extend(
                    ["f32", "u32", "i32", "bool", "vec2", "vec3", "vec4", "mat3", "mat4"]
                        .iter()
                        .map(|s| s.to_string()),
                );
                if let Some(s) = closest(first, names.iter().map(String::as_str)) {
                    d = d.with_fix(format!("did you mean `{s}`?"), segs[0].ident.span, s);
                } else if lookup_name(p, scope.module, &first.to_lowercase()).is_some()
                    || p.module(scope.module).scope.contains_key(first)
                {
                    d = d.with_help(format!("`{first}` is a value here; a type is a struct, an enum, a type alias or a built-in type such as `f32`"));
                } else {
                    d = d.with_help(format!(
                        "declare it (`struct {first} {{ ... }}`), or import it with `use`"
                    ));
                }
                let d = with_std_import(p, d, first, segs[0].ident.span);
                diags.push(d);
                return p.types.error;
            }
        }
    } else {
        diags.extend(
            segs[..segs.len() - 1]
                .iter()
                .filter(|s| s.generics.is_some())
                .map(|s| no_generics(&s.ident)),
        );
        match lookup_or_report(p, diags, scope.module, segs) {
            Some(r) => r,
            None => return p.types.error,
        }
    };
    let params: &[ParamId] = match res {
        Res::Adt(a) => &p.adt(a).generics,
        Res::Alias(a) => &p.aliases[a.index()].generics,
        Res::TraitSet(s) => &p.trait_sets[s.index()].generics,
        Res::Trait(t) => &p.trait_(t).generics,
        _ => &[],
    };
    let args: Vec<TyId> = last
        .generics
        .iter()
        .flatten()
        .enumerate()
        .map(|(i, a)| match &a.kind {
            // `Job<f>` takes a job, a function (§16.3): its type is the function's.
            ast::TypeExprKind::Path(path)
                if matches!(res, Res::Adt(j) if p.is_lang_adt(j, Lang::Job))
                    && let Some(Res::Fn(f)) = resolve_value_item(p, scope.module, path) =>
            {
                if p.func(f).attrs.job.is_none() {
                    diags.push(
                        Diagnostic::new(
                            codes::E0336,
                            a.span,
                            format!("`{}` isn't a job: `Job<F>` takes a `@job fn`", p.func(f).name),
                        )
                        .with_help(format!(
                            "mark it `@job` (`@job fn {}(...)`), or call it directly",
                            p.func(f).name
                        )),
                    );
                    return p.types.error;
                }
                // A generic job's types: `Job<realize<Coat, Fawn>>`.
                let written =
                    path.segments.last().and_then(|s| s.generics.as_deref()).unwrap_or_default();
                let params = p.fn_all_generics(f);
                if written.len() != params.len() {
                    diags.push(wrong_generic_count(
                        a.span,
                        &p.func(f).name,
                        params.len(),
                        written.len(),
                    ));
                    return p.types.error;
                }
                let given: Vec<TyId> = written
                    .iter()
                    .zip(&params)
                    .map(|(t, &param)| resolve_generic_arg(p, diags, scope, t, param))
                    .collect();
                p.types.intern(TyKind::FnDef(f, given))
            }
            _ => written_arg(p, diags, scope, a, params.get(i).copied()),
        })
        .collect();
    match res {
        Res::BuiltinTy(b) => {
            if !args.is_empty() {
                diags.push(
                    Diagnostic::new(
                        codes::E0322,
                        te.span,
                        format!("`{}` takes no generic arguments", last.ident.name),
                    )
                    .with_note(
                        "units are constants, not types: a length is an `f32` in metres (§5)",
                    )
                    .with_help(format!("write `{}`", last.ident.name)),
                );
            }
            if b == BuiltinTy::Str && !pos.allows_runs() {
                diags.push(run_stored(te.span, "a `str`"));
                return p.types.error;
            }
            b.ty(&p.types)
        }
        Res::Adt(a) => {
            let def = p.adt(a);
            let want = def.written_generics();
            if args.len() != want {
                diags.push(wrong_generic_count(te.span, &def.name, want, args.len()));
                return p.types.error;
            }
            if def.fn_fields.is_empty() {
                return p.types.adt(a, args);
            }
            // Its function fields' parameters: a parameter's type makes the function generic
            // over each, bounded by the field's function type (`fn.fields`).
            let TyPos::Param(implicit) = pos else {
                diags.push(
                    Diagnostic::new(
                        codes::E0410,
                        te.span,
                        format!("`{}` holds functions, so it's named here only as a parameter's type", def.name),
                    )
                    .with_note("a field of a function type makes its struct generic over the function, and a parameter's type makes the function generic over it, as a trait does (§6.7); a field's or a result's type would need the function written, which nothing names")
                    .with_help("pass it as a parameter, or build it where it's used"),
                );
                return p.types.error;
            };
            let written = Subst::from_pairs(&def.generics[..want], &args);
            let mut all = args;
            for (field, g) in &def.fn_fields {
                let bound = p.param(*g).fn_bound.map(|b| p.types.subst(b, &written));
                let param = ParamDef::new(format!("{}.{field}", def.name), te.span);
                let id = implicit.add_def(ParamDef { fn_bound: bound, ..param });
                all.push(p.types.param(id));
            }
            p.types.adt(a, all)
        }
        Res::Alias(a) => {
            let def = &p.aliases[a.index()];
            if args.len() != def.generics.len() {
                diags.push(wrong_generic_count(te.span, &def.name, def.generics.len(), args.len()));
                return p.types.error;
            }
            p.types.subst(def.ty, &Subst::from_pairs(&def.generics, &args))
        }
        Res::TraitSet(s) => {
            let def = &p.trait_sets[s.index()];
            if args.len() != def.generics.len() {
                diags.push(wrong_generic_count(te.span, &def.name, def.generics.len(), args.len()));
                return p.types.error;
            }
            let subst = Subst::from_pairs(&def.generics, &args);
            let refs: Vec<TraitRef> =
                def.traits.iter().map(|r| r.subst(&p.types, &subst)).collect();
            traits_as_type(p, diags, pos, refs, &def.name, te.span, || {
                Diagnostic::new(codes::E0410, te.span, format!("`{}` is a trait set, so it isn't a type here", def.name))
                    .with_note("traits name a type only as a parameter's type (any type with them) or a return type (the one type the body returns)")
                    .with_help("add a generic parameter, as in `struct S<T: Set> { x: T }`")
            })
        }
        Res::Trait(t) => {
            let want = p.trait_(t).generics.len();
            if args.len() != want {
                diags.push(wrong_generic_count(te.span, &p.trait_(t).name, want, args.len()));
                return p.types.error;
            }
            let name = &p.trait_(t).name;
            traits_as_type(p, diags, pos, vec![TraitRef { trait_: t, args }], name, te.span, || {
                Diagnostic::new(codes::E0410, te.span, format!("`{name}` is a trait, so it isn't a type here"))
                    .with_note("a trait names a type only as a parameter's type (any type with it) or a return type (the one type the body returns)")
                    .with_help("add a generic parameter, as in `struct S<T: Trait> { x: T }`")
            })
        }
        other => {
            let help = match other {
                Res::BuiltinFn(f) => format!(
                    "a type is a scalar, a vector, a struct or an enum here; `{0}` is a function, called as `{0}(x)`",
                    f.name()
                ),
                _ => "name a struct, an enum, a type alias or a built-in type such as `f32` here"
                    .into(),
            };
            diags.push(
                Diagnostic::new(
                    codes::E0212,
                    te.span,
                    format!("{} isn't a type", describe_res(p, other)),
                )
                .with_help(help),
            );
            p.types.error
        }
    }
}

/// A type that names the traits `refs` (shown as `name`), at `pos`: a parameter's type makes
/// the function generic over a parameter with them; a return or alias type names the type the
/// body returns, which the caller makes (this gives the error type); anywhere else it's the
/// `error` diagnostic.
fn traits_as_type(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    pos: TyPos,
    refs: Vec<TraitRef>,
    name: &str,
    span: Span,
    error: impl FnOnce() -> Diagnostic,
) -> TyId {
    match pos {
        TyPos::Param(implicit) => {
            let def = ParamDef { bounds: refs, ..ParamDef::new(format!("impl {name}"), span) };
            p.types.param(implicit.add_def(def))
        }
        TyPos::Return(opaque) | TyPos::Alias(opaque) => {
            *opaque = Some(refs);
            p.types.error
        }
        TyPos::Normal | TyPos::Local | TyPos::FnBound => {
            diags.push(error());
            p.types.error
        }
    }
}

/// `T::Name` or `Self::Name`: finds the trait among `base`'s bounds that declares `Name`.
fn projection(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    scope: &Scope,
    base: TyId,
    name: &ast::Ident,
) -> TyId {
    let mut candidates: Vec<TraitRef> = match p.types.kind(base) {
        TyKind::Param(id) => param_bounds_closure(p, *id),
        _ => Vec::new(),
    };
    // `Self::Name` in an impl of a trait: the trait's (or a supertrait's) associated type, for
    // the impl's type; this impl's own, once it's known.
    let imp = scope.impl_.map(|i| p.impl_(i)).filter(|imp| imp.self_ty == base);
    if let Some((imp, r)) = imp.and_then(|imp| Some((imp, imp.trait_ref.as_ref()?))) {
        let own = p.trait_(r.trait_).assoc_types.iter().any(|a| a.name == name.name);
        if own && let Some(&t) = imp.assoc_types.get(&name.name) {
            return t;
        }
        add_with_supertraits(p, base, r.clone(), &mut candidates);
    }
    let hits: Vec<TraitRef> = candidates
        .into_iter()
        .filter(|r| p.trait_(r.trait_).assoc_types.iter().any(|a| a.name == name.name))
        .collect();
    match hits.as_slice() {
        [r] => p.types.intern(TyKind::Projection {
            self_ty: base,
            trait_: r.trait_,
            trait_args: r.args.clone(),
            name: name.name.clone(),
        }),
        [] => {
            diags.push(Diagnostic::new(
                codes::E0408,
                name.span,
                format!("`{}` has no associated type `{}`", p.display_ty(base), name.name),
            )
            .with_help("name an associated type that a bound of it declares (`T::Box` with `T: Domain`), or declare it in the trait: `type Name`"));
            p.types.error
        }
        _ => {
            diags.push(Diagnostic::new(
                codes::E0406,
                name.span,
                format!(
                    "`{}` is an associated type of more than one of `{}`'s traits",
                    name.name,
                    p.display_ty(base)
                ),
            ).with_help(format!("rename it in one of the traits, or bound `{}` by only the trait whose `{}` this is", p.display_ty(base), name.name)));
            p.types.error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing item's help lists only what the using module may name: `std::quat`'s private
    /// imports and functions aren't among them.
    #[test]
    fn a_missing_items_help_lists_only_visible_items() {
        let source = "pub fn f() -> u32 {\n    std::quat::zzzzzz()\n}\n";
        let mut diagnostics = Vec::new();
        crate::check_program(crate::test_units(source), &mut diagnostics);
        let d = diagnostics.iter().find(|d| d.code == codes::E0200).expect("E0200");
        assert_eq!(d.help, ["its items are `Quat`"], "{diagnostics:#?}");
    }

    #[test]
    fn array_lengths_accept_integer_constant_expressions() {
        let source = "\
            struct Samples { values: [u32; COUNT - 2] }\n\
            const COUNT: u32 = WIDTH * 3 + 1\n\
            const WIDTH: u32 = 2\n\
            pub fn count() -> u32 {\n\
                let values: [u32; (WIDTH + 3)] = [0; COUNT - 2]\n\
                values.len()\n\
            }\n";
        let mut diagnostics = Vec::new();
        crate::check_program(crate::test_units(source), &mut diagnostics);
        assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
    }

    #[test]
    fn constant_parameter_lengths_accept_parentheses() {
        let source = "\
            pub fn count<const N: u32>() -> u32 {\n\
                let values: [u32; (N)] = [0; ((N))]\n\
                values.len()\n\
            }\n";
        let mut diagnostics = Vec::new();
        crate::check_program(crate::test_units(source), &mut diagnostics);
        assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
    }

    #[test]
    fn array_length_constants_use_later_type_aliases() {
        let source = "\
            type Samples = [u32; COUNT]\n\
            type Size = u32\n\
            const COUNT: Size = WIDTH + 1\n\
            const WIDTH: Size = 2\n\
            pub fn count(values: Samples) -> u32 { values.len() }\n";
        let mut diagnostics = Vec::new();
        crate::check_program(crate::test_units(source), &mut diagnostics);
        assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
    }

    #[test]
    fn alias_length_parameters_shadow_global_constants() {
        let source = "\
            type Buffer<const N: u32> = [u32; N]\n\
            const N: Buffer<2> = [1, 2]\n\
            type Identity<Identity> = Identity\n\
            pub fn identity(value: Identity<u32>) -> u32 { value }\n\
            pub fn size() -> u32 { N.len() }\n";
        let mut diagnostics = Vec::new();
        crate::check_program(crate::test_units(source), &mut diagnostics);
        assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
    }

    #[test]
    fn integer_length_operations_do_not_wrap_or_trap() {
        let mut diagnostics = Vec::new();
        let p = crate::collect::collect(
            crate::test_units(
                "\
                const WIDTH: u32 = 2\n\
                const SIGNED = !0\n\
                const BACK = -2 + 4\n\
                const BYTE: u8 = 255\n\
                const FLOAT: f32 = 2\n",
            ),
            &crate::collect::PackageInfo::std_and_program(),
            &mut diagnostics,
        );
        assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
        let module = p.main.expect("main");
        for (expression, expected) in [
            ("WIDTH ** 3", Some(8)),
            ("SIGNED", None),
            ("SIGNED + 2", Some(1)),
            ("BACK", Some(2)),
            ("BYTE << 1", Some(254)),
            ("BYTE + 1", None),
            ("FLOAT", None),
            ("(9 / 2) + (9 % 2)", Some(5)),
            ("(1 << 3) | (7 & 3) ^ (8 >> 2)", Some(9)),
            ("4294967295 + 1", None),
            ("0 - 1", None),
            ("65536 * 65536", None),
            ("2 ** 32", None),
            ("1 / 0", None),
            ("1 % 0", None),
            ("1 << 32", None),
            ("1 >> 32", None),
            ("WIDTH::<u32> + 1", None),
            ("2 ** WIDTH::<u32>", None),
            ("(!0) ^ 4294967295", Some(0)),
            ("width()", None),
        ] {
            let parsed = wrela_syntax::parse(
                wrela_diag::FileId(0),
                &format!("const LENGTH = {expression}\n"),
            );
            assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
            let ast::ItemKind::Const(c) = &parsed.file.items[0].kind else {
                panic!("a constant");
            };
            assert_eq!(const_u32(&p, module, &c.value), expected, "{expression}");
        }
    }

    #[test]
    fn frontend_perf_suggestions_preserve_unicode_distance_and_ties() {
        fn reference(a: &[char], b: &[char]) -> usize {
            let mut rows = vec![vec![0; b.len() + 1]; a.len() + 1];
            for (i, row) in rows.iter_mut().enumerate() {
                row[0] = i;
            }
            for (j, cell) in rows[0].iter_mut().enumerate() {
                *cell = j;
            }
            for i in 1..=a.len() {
                for j in 1..=b.len() {
                    rows[i][j] = (rows[i - 1][j] + 1)
                        .min(rows[i][j - 1] + 1)
                        .min(rows[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
                    if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                        rows[i][j] = rows[i][j].min(rows[i - 2][j - 2] + 1);
                    }
                }
            }
            rows[a.len()][b.len()]
        }
        let words = ["", "a", "b", "ab", "ba", "α", "αβ", "éa", "🦀x", "kitten", "sitting"];
        for a in words {
            for b in words {
                let ac: Vec<_> = a.chars().collect();
                let bc: Vec<_> = b.chars().collect();
                assert_eq!(edit_distance(&ac, b), reference(&ac, &bc), "{a:?}, {b:?}");
            }
        }
        assert_eq!(closest("cat", ["cut", "bat", "cat"].into_iter()), Some("bat"));
        assert_eq!(closest("αβ", ["αa", "αβ"].into_iter()), Some("αa"));
        assert_eq!(closest("cat", ["different"].into_iter()), None);
        // A swap of two neighbours is one edit, so short names' transpositions are suggested.
        assert_eq!(edit_distance(&['a', 'b'], "ba"), 1);
        assert_eq!(closest("widht", ["width", "height"].into_iter()), Some("width"));
        assert_eq!(closest("aera", ["area", "perimeter"].into_iter()), Some("area"));
        assert_eq!(closest("lenght", ["length"].into_iter()), Some("length"));
        assert_eq!(closest("x", ["y"].into_iter()), Some("y"));
        assert_eq!(closest("xyz", ["abc"].into_iter()), None);
    }
}
