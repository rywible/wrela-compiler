//! Name resolution: module paths, scopes, and type expressions.

use crate::builtins::{BuiltinFn, BuiltinTy};
use crate::defs::*;
use crate::program::Program;
use crate::ty::*;
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
    /// A local binding's type: a run may be one (§6.6), a trait may not.
    Local,
    /// A generic parameter's bound: a function type may be one (`F: fn(vec3) -> f32`).
    FnBound,
}

impl TyPos<'_> {
    /// Whether a projection type (a run, `str`) may be written here: a parameter's, a
    /// result's or a local binding's type, never a field's or a type argument (§6.6).
    fn allows_runs(&self) -> bool {
        !matches!(self, TyPos::Normal)
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

    fn add(&mut self, def: ParamDef) -> ParamId {
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
pub fn with_std_import(p: &Program, d: Diagnostic, name: &str, at: Span) -> Diagnostic {
    match std_home(p, name) {
        Some(home) => {
            let mut d = d;
            d.fixes.clear();
            d.with_note(format!("`{name}` is in `{home}`, which this module doesn't import"))
                .with_fix(
                    format!("import it: `use {home}::{name}`"),
                    Span::new(at.file, 0, 0),
                    format!("use {home}::{name}\n"),
                )
        }
        None => d,
    }
}

/// The closest of `candidates` to `name` by edit distance, if it's close enough to suggest.
pub fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.len() / 3).max(1);
    candidates
        .filter(|c| *c != name)
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
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
fn lookup_global(p: &Program, name: &str) -> Option<Res> {
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

/// Every name visible in a module, for suggestions.
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

fn describe_res(p: &Program, r: Res) -> String {
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
                        .with_note("only a package's `pub` items cross to the packages that depend on it (§3)");
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
                    let mut d = Diagnostic::new(
                        codes::E0200,
                        seg.span,
                        format!("`{}` has no item `{}`", module.name(), seg.name),
                    );
                    let names: Vec<&str> = module
                        .scope
                        .keys()
                        .map(String::as_str)
                        .chain(module.children.keys().map(String::as_str))
                        .collect();
                    if let Some(s) = closest(&seg.name, names.into_iter()) {
                        d = d.with_fix(format!("did you mean `{s}`?"), seg.span, s);
                    }
                    return PathLookup::Error(Box::new(d));
                }
            }
            Res::Adt(a) if p.adt(a).is_enum() => {
                match p.adt(a).variants().iter().position(|v| v.name == seg.name) {
                    Some(i) => Res::Variant(a, i as u32),
                    None => {
                        let mut d = Diagnostic::new(
                            codes::E0211,
                            seg.span,
                            format!("`{}` has no variant `{}`", p.adt(a).name, seg.name),
                        );
                        if let Some(s) =
                            closest(&seg.name, p.adt(a).variants().iter().map(|v| v.name.as_str()))
                        {
                            d = d.with_fix(format!("did you mean `{s}`?"), seg.span, s);
                        }
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

/// E0322: `name` takes `want` generic arguments, but `given` are written.
pub(crate) fn wrong_generic_count(span: Span, name: &str, want: usize, given: usize) -> Diagnostic {
    let s = wrela_diag::plural(want);
    Diagnostic::new(
        codes::E0322,
        span,
        format!("`{name}` takes {want} generic argument{s}, not {given}"),
    )
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
    let args: Vec<TyId> = last
        .generics
        .iter()
        .flatten()
        .map(|a| resolve_type(p, diags, scope, a, TyPos::Normal))
        .collect();
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
            .with_note("a bound or an opt-in names a trait, such as `Copy` or `Surface`"),
        );
        return None;
    };
    let last = &path.segments[path.segments.len() - 1];
    let args: Vec<TyId> = last
        .generics
        .iter()
        .flatten()
        .map(|a| resolve_type(p, diags, scope, a, TyPos::Normal))
        .collect();
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
                    Diagnostic::new(codes::E0510, te.span, "a function type can only be a parameter's type")
                        .with_note("a closure that captures only values can be stored, but its type is its own and inferred (§6.7)")
                        .with_help("to keep work for later, store an enum of actions and `match` on it"),
                );
                return p.types.error;
            }
            let mut flags = FnFlags::default();
            for a in &f.attrs {
                if a.name == "deterministic" {
                    flags.deterministic = true;
                } else if a.name == "parallel" {
                    flags.parallel = true;
                } else if a.name == "audio" {
                    flags.audio = true;
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
            diags.push(Diagnostic::new(
                codes::E0212,
                te.span,
                "a number is a constant generic argument, not a type",
            ));
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
            match pos {
                TyPos::Param(implicit) => {
                    let id = implicit.add(ParamDef {
                        name: format!("impl {}", names.join(" + ")),
                        bounds: refs,
                        span: te.span,
                        is_self: false,
                        is_const: false,
                        fn_bound: None,
                    });
                    p.types.param(id)
                }
                TyPos::Return(opaque) => {
                    *opaque = Some(refs);
                    p.types.error // replaced by the caller with the opaque type
                }
                TyPos::Normal | TyPos::Local | TyPos::FnBound => {
                    diags.push(
                        Diagnostic::new(codes::E0410, te.span, format!("`{}` are traits, so they aren't a type here", names.join(" + ")))
                            .with_note("traits name a type only as a parameter's type (any type with them) or a return type (the one type the body returns)")
                            .with_help("add a generic parameter, as in `struct S<T: A + B> { x: T }`"),
                    );
                    p.types.error
                }
            }
        }
    }
}

/// Why `len`, written as an array length in `module`, isn't one: a path that names nothing
/// usable (a private constant, an unknown name), a literal past `u32`, or what a length may be.
pub fn length_error(p: &Program, module: ModuleId, len: &ast::Expr) -> Diagnostic {
    let mut inner = len;
    while let ast::ExprKind::Paren(x) = &inner.kind {
        inner = x;
    }
    match &inner.kind {
        ast::ExprKind::Path(path) if !path.is_single() => {
            if let PathLookup::Error(d) = resolve_module_path_in(p, module, &path.segments, true) {
                return *d;
            }
        }
        ast::ExprKind::Path(path) => {
            let name = &path.segments[0].ident.name;
            if lookup_name(p, module, name).is_none() && !is_broken(p, module, name) {
                return Diagnostic::new(
                    codes::E0200,
                    len.span,
                    format!("there's no constant `{name}` here"),
                );
            }
        }
        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(_), .. }) => {
            return Diagnostic::new(
                codes::E0325,
                len.span,
                format!("an array length is at most {} (a `u32`)", u32::MAX),
            );
        }
        _ => {}
    }
    Diagnostic::new(
        codes::E0325,
        len.span,
        "an array length is an integer literal or a constant holding one",
    )
    .with_note("evaluating expressions at compile time is tier 1")
}

/// The value of an array length: an integer literal, or a constant holding one, resolved in
/// `module`; a constant's own value is resolved in the module that defines it. `None` for
/// anything else, including a constant whose value refers back to itself.
pub fn const_u32(p: &Program, module: ModuleId, e: &ast::Expr) -> Option<u32> {
    fn go(
        p: &Program,
        module: ModuleId,
        e: &ast::Expr,
        visiting: &mut Vec<ConstId>,
    ) -> Option<u32> {
        match &e.kind {
            ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. }) => {
                v.ok().and_then(|v| u32::try_from(v).ok())
            }
            ast::ExprKind::Paren(inner) => go(p, module, inner, visiting),
            ast::ExprKind::Path(path) => {
                let Res::Const(c) = resolve_value_item(p, module, path)? else { return None };
                if visiting.contains(&c) {
                    return None;
                }
                visiting.push(c);
                let def = p.const_(c);
                let v = go(p, def.module, &def.value, visiting);
                visiting.pop();
                v
            }
            _ => None,
        }
    }
    go(p, module, e, &mut Vec::new())
}

/// The item a value path names from `module` (`N`, `a::inner::N`), if it names one; generic
/// arguments are ignored.
pub fn resolve_value_item(p: &Program, module: ModuleId, path: &ast::Path) -> Option<Res> {
    if path.is_single() {
        return lookup_name(p, module, &path.segments[0].ident.name);
    }
    match resolve_module_path_in(p, module, &path.segments, true) {
        PathLookup::Found(r) => Some(r),
        _ => None,
    }
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
    } else {
        scope.param(first).map(|id| p.types.param(id))
    };
    if let Some(base) = base_param {
        if segs.len() == 1 {
            if segs[0].generics.is_some() {
                diags.push(Diagnostic::new(
                    codes::E0322,
                    te.span,
                    format!("`{first}` takes no generic arguments"),
                ));
            }
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
                }
                let d = with_std_import(p, d, first, segs[0].ident.span);
                diags.push(d);
                return p.types.error;
            }
        }
    } else {
        match lookup_or_report(p, diags, scope.module, segs) {
            Some(r) => r,
            None => return p.types.error,
        }
    };
    // A struct's `const N: u32` parameter takes a number (or a constant's name, or a const
    // parameter).
    let const_at = |i: usize| match res {
        Res::Adt(a) => p.adt(a).generics.get(i).is_some_and(|&g| p.param(g).is_const),
        _ => false,
    };
    let args: Vec<TyId> = last
        .generics
        .iter()
        .flatten()
        .enumerate()
        .map(|(i, a)| match &a.kind {
            ast::TypeExprKind::Int(lit) if const_at(i) => match &lit.kind {
                ast::LitKind::Int(v) if v.ok().is_some_and(|v| u32::try_from(v).is_ok()) => {
                    p.types.intern(TyKind::ConstU32(v.ok().unwrap_or(0) as u32))
                }
                _ => {
                    diags.push(Diagnostic::new(codes::E0212, a.span, "a length is a `u32`"));
                    p.types.error
                }
            },
            _ => resolve_type(p, diags, scope, a, TyPos::Normal),
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
            let want = p.adt(a).generics.len();
            if args.len() != want {
                diags.push(wrong_generic_count(te.span, &p.adt(a).name, want, args.len()));
                return p.types.error;
            }
            p.types.adt(a, args)
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
            match pos {
                TyPos::Param(implicit) => {
                    let id = implicit.add(ParamDef {
                        name: format!("impl {}", def.name),
                        bounds: refs,
                        span: te.span,
                        is_self: false,
                        is_const: false,
                        fn_bound: None,
                    });
                    p.types.param(id)
                }
                TyPos::Return(opaque) => {
                    *opaque = Some(refs);
                    p.types.error // replaced by the caller with the opaque type
                }
                TyPos::Normal | TyPos::Local | TyPos::FnBound => {
                    diags.push(
                        Diagnostic::new(codes::E0410, te.span, format!("`{}` is a trait set, so it isn't a type here", def.name))
                            .with_note("traits name a type only as a parameter's type (any type with them) or a return type (the one type the body returns)")
                            .with_help("add a generic parameter, as in `struct S<T: Set> { x: T }`"),
                    );
                    p.types.error
                }
            }
        }
        Res::Trait(t) => {
            let want = p.trait_(t).generics.len();
            if args.len() != want {
                diags.push(wrong_generic_count(te.span, &p.trait_(t).name, want, args.len()));
                return p.types.error;
            }
            let r = TraitRef { trait_: t, args };
            match pos {
                TyPos::Param(implicit) => {
                    let id = implicit.add(ParamDef {
                        name: format!("impl {}", p.trait_(t).name),
                        bounds: vec![r],
                        span: te.span,
                        is_self: false,
                        is_const: false,
                        fn_bound: None,
                    });
                    p.types.param(id)
                }
                TyPos::Return(opaque) => {
                    *opaque = Some(vec![r]);
                    p.types.error // replaced by the caller with the opaque type
                }
                TyPos::Normal | TyPos::Local | TyPos::FnBound => {
                    diags.push(
                        Diagnostic::new(codes::E0410, te.span, format!("`{}` is a trait, so it isn't a type here", p.trait_(t).name))
                            .with_note("a trait names a type only as a parameter's type (any type with it) or a return type (the one type the body returns)")
                            .with_help("add a generic parameter, as in `struct S<T: Trait> { x: T }`"),
                    );
                    p.types.error
                }
            }
        }
        other => {
            diags.push(Diagnostic::new(
                codes::E0212,
                te.span,
                format!("{} isn't a type", describe_res(p, other)),
            ));
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
            ));
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
            ));
            p.types.error
        }
    }
}
