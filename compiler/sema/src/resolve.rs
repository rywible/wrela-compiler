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
            FnOwner::Free => {}
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
        Res::Fn(f) => format!("the function `{}`", p.func(f).name),
        Res::Const(c) => format!("the constant `{}`", p.const_(c).name),
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
/// The first segment is `std`, a name in `from`'s scope, a top-level module of the package, or
/// a prelude name. Before the `final_pass`, a name that isn't found is `NotYet`: an import may
/// still bind it.
pub fn resolve_module_path_in(
    p: &Program,
    from: ModuleId,
    segs: &[impl PathSeg],
    final_pass: bool,
) -> PathLookup {
    let first = segs[0].ident();
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
            || !p.package_root.is_some_and(|r| p.module(r).children.contains_key(&first.name))
    }) {
        b.res
    } else if let Some(&m) = p.package_root.and_then(|r| p.module(r).children.get(&first.name)) {
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
        let mut names = visible_names(p, from);
        if let Some(r) = p.package_root {
            names.extend(p.module(r).children.keys().cloned());
        }
        if let Some(s) = closest(&first.name, names.iter().map(String::as_str)) {
            d = d.with_fix(format!("did you mean `{s}`?"), first.span, s);
        }
        return PathLookup::Error(Box::new(d));
    };
    for seg in &segs[1..] {
        let seg = seg.ident();
        res = match res {
            Res::Module(m) => {
                let module = p.module(m);
                if let Some(b) = module.scope.get(&seg.name) {
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

/// Resolves a bound or supertrait: a path naming a trait, with its generic arguments.
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
            match len {
                Some(len) => {
                    match const_u32(p, scope.module, len) {
                        Some(n) => p.types.array(e, n),
                        None => {
                            diags.push(
                            Diagnostic::new(codes::E0325, len.span, "an array length is an integer literal or a constant holding one")
                                .with_note("evaluating expressions at compile time is tier 1"),
                        );
                            p.types.error
                        }
                    }
                }
                None => {
                    if !matches!(pos, TyPos::Param(_)) {
                        diags.push(
                            Diagnostic::new(codes::E0327, te.span, "a run `[T]` can only be a parameter's type")
                                .with_note("a run borrows its caller's data, so it can't be stored or returned (§6.2)")
                                .with_help("use a fixed-size array `[T; N]`"),
                        );
                        return p.types.error;
                    }
                    p.types.intern(TyKind::Slice(e))
                }
            }
        }
        ast::TypeExprKind::Fn(params, ret) => {
            // Only a written type; a body's inferred types are checked once it's finished.
            if !matches!(pos, TyPos::Param(_)) {
                diags.push(
                    Diagnostic::new(codes::E0510, te.span, "a function type can only be a parameter's type")
                        .with_note("closures don't escape the call they're passed to (§6.7); storing or returning one is tier 1 (`@escaping`)"),
                );
                return p.types.error;
            }
            let ps =
                params.iter().map(|t| resolve_type(p, diags, scope, t, TyPos::Normal)).collect();
            let r = match ret {
                Some(r) => resolve_type(p, diags, scope, r, TyPos::Normal),
                None => p.types.unit,
            };
            p.types.intern(TyKind::FnPtr(ps, r))
        }
        ast::TypeExprKind::Path(path) => resolve_type_path(p, diags, scope, te, path, pos),
    }
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
                let mut names = visible_names(p, scope.module);
                names.extend(scope.params.iter().map(|(n, _)| n.clone()));
                names.extend(
                    ["f32", "u32", "i32", "bool", "vec2", "vec3", "vec4", "mat3", "mat4"]
                        .iter()
                        .map(|s| s.to_string()),
                );
                if let Some(s) = closest(first, names.iter().map(String::as_str)) {
                    d = d.with_fix(format!("did you mean `{s}`?"), segs[0].ident.span, s);
                } else if first == "str" || first == "String" {
                    d = Diagnostic::new(codes::E0901, segs[0].ident.span, "strings are tier 1");
                } else if matches!(
                    first.as_str(),
                    "Vec" | "Box" | "Handle" | "Arena" | "List" | "Region" | "Result"
                ) {
                    d = d.with_note(format!("`{first}` comes with tier 1's stdlib (milestone 2)"));
                }
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
    let args: Vec<TyId> = last
        .generics
        .iter()
        .flatten()
        .map(|a| resolve_type(p, diags, scope, a, TyPos::Normal))
        .collect();
    match res {
        Res::BuiltinTy(b) => {
            if !args.is_empty() {
                diags.push(
                    Diagnostic::new(
                        codes::E0900,
                        te.span,
                        format!(
                            "`{}` takes no generic arguments here; vector units are tier 1",
                            last.ident.name
                        ),
                    )
                    .with_help(format!("write `{}`", last.ident.name)),
                );
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
                    });
                    p.types.param(id)
                }
                TyPos::Return(opaque) => {
                    *opaque = Some(vec![r]);
                    p.types.error // replaced by the caller with the opaque type
                }
                TyPos::Normal => {
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
