//! Collects a program's definitions from its parsed files: the module tree, every item, `use`
//! imports, and every signature (generics, fields, variants, traits, impls, functions).

use crate::defs::*;
use crate::graph::{Visit, dfs_cycles};
use crate::program::{LangRes, Program};
use crate::resolve::{self, Scope, TyPos};
use crate::ty::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

/// The most elements a tuple has for a `@fieldwise` trait to be derived for it.
const MAX_DERIVED_TUPLE: usize = 12;

/// One parsed source file and the module it is.
pub struct SourceUnit {
    /// The module path in its package: `["shapes", "blob"]`, or `["std", "field"]` for std.
    pub path: Vec<String>,
    /// Its tree, shared: std's are parsed once a process.
    pub ast: std::sync::Arc<ast::File>,
    /// Its package, by index into the packages `collect` is given. std's privileges come
    /// from std's package (`PackageKind::Std`).
    pub package: usize,
    /// Where its syntax errors are: code around them may be missing parts.
    pub syntax_errors: Vec<Span>,
    /// Its text, for fixes that move what's written (`Program::text`).
    pub text: std::sync::Arc<str>,
}

struct PendingUse {
    module: ModuleId,
    path: Vec<ast::Ident>,
    alias: ast::Ident,
    vis: Vis,
    span: Span,
}

/// Who sees a name: `pub` makes it public, and `pub(package)` public in its package only.
#[derive(Clone, Copy)]
struct Vis {
    public: bool,
    package_only: bool,
}

impl Vis {
    const PRIVATE: Vis = Vis { public: false, package_only: false };

    fn of(v: &Option<ast::Vis>) -> Vis {
        Vis { public: v.is_some(), package_only: v.as_ref().is_some_and(|v| v.package) }
    }
}

/// A package the program is made of, as the driver found it (language.md §3).
#[derive(Clone, Debug)]
pub struct PackageInfo {
    pub name: String,
    pub kind: PackageKind,
    /// Its dependencies: the name its code uses for each, and which package it is (an index).
    pub deps: Vec<(String, usize)>,
    /// Its manifest declares `unsafe`.
    pub unsafe_ok: bool,
    /// A lifted build lifts its literals (language.md §23): its literal tables are built where
    /// they're used, from their literals, rather than kept as data.
    pub lifted: bool,
}

impl PackageInfo {
    /// The packages of a program with no dependencies: std, then the program's own.
    pub fn std_and_program() -> Vec<PackageInfo> {
        vec![
            PackageInfo {
                name: "std".into(),
                kind: PackageKind::Std,
                deps: Vec::new(),
                unsafe_ok: true,
                lifted: false,
            },
            PackageInfo {
                name: "main".into(),
                kind: PackageKind::Program,
                deps: Vec::new(),
                unsafe_ok: false,
                lifted: false,
            },
        ]
    }
}

/// What `collect` remembers between its passes about each AST item.
enum PendingItem<'u> {
    Fn(FnId, &'u ast::FnDecl),
    Adt(AdtId, &'u ast::Item),
    Trait(TraitId, &'u ast::TraitDecl, Vec<(FnId, &'u ast::FnDecl)>),
    Impl(ImplId, &'u ast::ImplDecl, Vec<(FnId, &'u ast::FnDecl)>),
    Const(ConstId, &'u ast::ConstDecl),
    Alias(AliasId, &'u ast::TypeAliasDecl),
    TraitSet(TraitSetId, &'u ast::TraitSetDecl),
}

pub fn collect(
    units: Vec<SourceUnit>,
    packages: &[PackageInfo],
    diags: &mut Vec<Diagnostic>,
) -> Program {
    let mut c = Collector {
        p: Program::default(),
        diags,
        pending: Vec::new(),
        uses: Vec::new(),
        alias_fns: HashMap::new(),
    };
    c.p.syntax_errors = units.iter().flat_map(|u| u.syntax_errors.iter().copied()).collect();
    c.p.texts = units.iter().map(|u| (u.ast.span.file, u.text.clone())).collect();
    let modules = c.build_modules(&units, packages);
    c.declare_items(&units, &modules);
    c.resolve_imports();
    c.check_items_beside_modules();
    c.find_lang_items();
    c.resolve_signatures();
    c.make_packed();
    c.index_impls();
    // After the impls: a bound type's fields ask which types are `Copy`, an answer kept for the
    // rest of the compile, and a field's `T::Out` is known only through its impl.
    c.make_bound_types();
    c.check_recursive_types();
    c.check_impls();
    c.p
}

struct Collector<'d, 'u> {
    p: Program,
    diags: &'d mut Vec<Diagnostic>,
    pending: Vec<(ModuleId, PendingItem<'u>)>,
    uses: Vec<PendingUse>,
    /// The functions that define aliases naming traits, and those traits.
    alias_fns: HashMap<FnId, (AliasId, Vec<TraitRef>)>,
}

impl<'d, 'u> Collector<'d, 'u> {
    // ---- modules ---------------------------------------------------------------------------

    fn new_module(&mut self, path: Vec<String>, package: PackageId) -> ModuleId {
        self.p.modules.push(Module {
            path,
            children: BTreeMap::new(),
            scope: BTreeMap::new(),
            broken: BTreeSet::new(),
            package,
        });
        ModuleId(self.p.modules.len() as u32 - 1)
    }

    /// E0201: an item with the name of a module beside it (`fn b` in `a.wrela` and the file
    /// `a/b.wrela`): a module has one namespace, so `a::b` would name both.
    fn check_items_beside_modules(&mut self) {
        for m in &self.p.modules {
            if self.p.packages[m.package.index()].kind == PackageKind::Std {
                continue;
            }
            for (name, &child) in &m.children {
                if let Some(b) = m.scope.get(name) {
                    let path = self.p.modules[child.index()].path.join("/");
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0201,
                            b.span,
                            format!("`{name}` is defined twice in this module: it's also the module in `{path}.wrela` or `{path}/`"),
                        )
                        .with_note("a module has one namespace for its items and the modules in it (§3)")
                        .with_help("rename the item or the module"),
                    );
                }
            }
        }
    }

    /// Builds each package's module tree; returns the module each unit is. A dependency's
    /// modules are named under it: `fieldkit::shapes`.
    fn build_modules(&mut self, units: &[SourceUnit], packages: &[PackageInfo]) -> Vec<ModuleId> {
        for (i, info) in packages.iter().enumerate() {
            let id = PackageId(i as u32);
            let path = match info.kind {
                PackageKind::Std => vec!["std".to_string()],
                PackageKind::Program => Vec::new(),
                PackageKind::Dependency => vec![info.name.clone()],
            };
            let root = self.new_module(path, id);
            match info.kind {
                PackageKind::Std => self.p.std_root = Some(root),
                PackageKind::Program => self.p.package_root = Some(root),
                PackageKind::Dependency => {}
            }
            self.p.packages.push(PackageDef {
                name: info.name.clone(),
                root,
                kind: info.kind,
                deps: info.deps.iter().map(|(n, d)| (n.clone(), PackageId(*d as u32))).collect(),
                unsafe_ok: info.unsafe_ok,
                lifted: info.lifted,
            });
        }
        let mut modules = Vec::new();
        let mut named_std = false;
        for u in units {
            let is_std = packages[u.package].kind == PackageKind::Std;
            if !is_std && u.path.first().is_some_and(|s| s == "std") && !named_std {
                // A path's `std` is always the standard library, so this module couldn't be
                // reached.
                named_std = true;
                self.diags.push(
                    Diagnostic::new(
                        codes::E0201,
                        u.ast.span.shrink_to_start(),
                        "the package has a module named `std`, which is the standard library's name",
                    )
                    .with_note("a path that starts with `std` names the standard library")
                    .with_help("rename the file or directory"),
                );
            }
            let package = &self.p.packages[u.package];
            let (mut m, rest) =
                if is_std { (package.root, &u.path[1..]) } else { (package.root, &u.path[..]) };
            let program = package.kind == PackageKind::Program;
            for seg in rest {
                m = match self.p.modules[m.index()].children.get(seg) {
                    Some(&c) => c,
                    None => {
                        let mut path = self.p.modules[m.index()].path.clone();
                        path.push(seg.clone());
                        let c = self.new_module(path, PackageId(u.package as u32));
                        self.p.modules[m.index()].children.insert(seg.clone(), c);
                        c
                    }
                };
            }
            if program && u.path == ["main"] {
                self.p.main = Some(m);
                self.p.main_file = Some(u.ast.span.file);
            }
            modules.push(m);
        }
        modules
    }

    fn bind(&mut self, m: ModuleId, name: &ast::Ident, res: Res, vis: Vis) {
        let scope = &mut self.p.modules[m.index()].scope;
        if let Some(prev) = scope.get(&name.name) {
            let prev_span = prev.span;
            self.diags.push(
                Diagnostic::new(
                    codes::E0201,
                    name.span,
                    format!("`{}` is defined twice in this module", name.name),
                )
                .with_secondary(prev_span, "first defined here")
                .with_help("rename one of them"),
            );
            return;
        }
        let Vis { public, package_only } = vis;
        scope.insert(name.name.clone(), Binding { res, public, package_only, span: name.span });
    }

    // ---- items -----------------------------------------------------------------------------

    fn declare_items(&mut self, units: &'u [SourceUnit], modules: &[ModuleId]) {
        for (u, &m) in units.iter().zip(modules) {
            for item in &u.ast.items {
                let is_std = self.p.is_std(m);
                self.declare_item(m, item, is_std);
            }
        }
    }

    fn declare_generics(&mut self, generics: &[ast::GenericParam]) -> Vec<ParamId> {
        for (i, g) in generics.iter().enumerate() {
            if let Some(prev) = generics[..i].iter().find(|p| p.name.name == g.name.name) {
                self.diags.push(
                    Diagnostic::new(
                        codes::E0201,
                        g.name.span,
                        format!("the generic parameter `{}` is declared twice", g.name.name),
                    )
                    .with_help("rename one of them")
                    .with_secondary(prev.name.span, "first declared here"),
                );
            }
        }
        generics
            .iter()
            .map(|g| {
                // `const N: u32`: lengths are the one kind of constant parameter (§4).
                if let Some(t) = &g.const_ty
                    && !matches!(&t.kind, ast::TypeExprKind::Path(p) if p.is_single() && p.segments[0].ident.name == "u32")
                {
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0331,
                            t.span,
                            "a `const` generic parameter is a `u32`: an array's length",
                        )
                        .with_fix("make it a `u32`", t.span, "u32"),
                    );
                }
                let param = ParamDef::new(g.name.name.clone(), g.name.span);
                self.p.new_param(ParamDef { is_const: g.const_ty.is_some(), ..param })
            })
            .collect()
    }

    fn new_fn(
        &mut self,
        m: ModuleId,
        owner: FnOwner,
        f: &ast::FnDecl,
        vis: Vis,
        attrs: &[ast::Attribute],
        is_std: bool,
    ) -> FnId {
        let Vis { public, package_only } = vis;
        let generics = self.declare_generics(&f.generics);
        let core = is_std
            && self.p.module(m).path.get(1).is_some_and(|s| UNSAFE_CORE.contains(&s.as_str()));
        let mut attrs = self.fn_attrs(attrs, is_std, core);
        // std's unsafe core: its public functions are callable only inside `unsafe` (§6.12).
        attrs.unsafe_call = is_std
            && public
            && matches!(self.p.module(m).path.get(1).map(String::as_str), Some("mem" | "alloc"))
            && !matches!(
                f.name.name.as_str(),
                "size_of"
                    | "align_of"
                    | "needs_drop"
                    | "is_copy"
                    | "allocations"
                    | "debug_build"
                    | "test_build"
            );
        self.p.fns.push(FnDef {
            name: f.name.name.clone(),
            module: m,
            owner,
            generics,
            params: Vec::new(),
            ret: self.p.types.unit,
            ret_mode: RetMode::Owned,
            opaque: None,
            attrs,
            body: f.body.clone().map(Rc::new),
            public,
            package_only,
            span: f.sig_span.to(f.body.as_ref().map_or(f.sig_span, |b| b.span)),
            name_span: f.name.span,
            sig_span: f.sig_span,
            lang: None,
            derived: None,
        });
        FnId(self.p.fns.len() as u32 - 1)
    }

    fn declare_item(&mut self, m: ModuleId, item: &'u ast::Item, is_std: bool) {
        let vis = Vis::of(&item.vis);
        let public = vis.public;
        // Functions' attributes are read with their declarations; traits', structs' and
        // enums' here.
        let (fieldwise, diagnostic) = match item.kind {
            ast::ItemKind::Fn(_) => (false, None),
            ast::ItemKind::Trait(_) => self.type_attrs(&item.attrs, true),
            ast::ItemKind::Struct(_) | ast::ItemKind::Enum(_) => {
                self.type_attrs(&item.attrs, false)
            }
            // A constant's only attribute is its fuel, read with it.
            ast::ItemKind::Const(_) => (false, None),
            _ => {
                self.attrs_not_on_fn(&item.attrs);
                (false, None)
            }
        };
        match &item.kind {
            ast::ItemKind::Error(name) => {
                if let Some(n) = name {
                    self.p.modules[m.index()].broken.insert(n.name.clone());
                }
            }
            ast::ItemKind::Fn(f) => {
                let id = self.new_fn(m, FnOwner::Free, f, vis, &item.attrs, is_std);
                self.bind(m, &f.name, Res::Fn(id), vis);
                self.pending.push((m, PendingItem::Fn(id, f)));
            }
            ast::ItemKind::Struct(s) => {
                let kind = AdtKind::Struct(Vec::new());
                let a = self.declare_adt(m, item, &s.name, &s.generics, kind, vis);
                self.p.adts[a.index()].diagnostic = diagnostic;
            }
            ast::ItemKind::Enum(e) => {
                // The variants' names now, for imports (`use E::B`); their fields come with
                // the signatures. A name declared twice is reported then; the first one counts.
                let mut variants: Vec<VariantDef> = Vec::new();
                // Each discriminant as written, or one past the one before (§3): checked with
                // the signatures, and known now for an enum that's an array's length (`[T; E]`).
                let mut next: u32 = 0;
                for v in &e.variants {
                    if variants.iter().any(|p| p.name == v.name.name) {
                        continue;
                    }
                    let discriminant = match v.discriminant.as_ref().map(|l| &l.kind) {
                        Some(ast::LitKind::Int(v)) => {
                            v.ok().and_then(|n| u32::try_from(n).ok()).unwrap_or(u32::MAX)
                        }
                        _ => next,
                    };
                    next = discriminant.saturating_add(1);
                    let shape = match &v.kind {
                        ast::VariantKind::Unit => VariantShape::Unit,
                        ast::VariantKind::Tuple(_) => VariantShape::Tuple,
                        ast::VariantKind::Struct(_) => VariantShape::Struct,
                    };
                    let name = v.name.name.clone();
                    variants.push(VariantDef {
                        name,
                        shape,
                        fields: Vec::new(),
                        span: v.span,
                        discriminant,
                    });
                }
                let kind = AdtKind::Enum(variants);
                let a = self.declare_adt(m, item, &e.name, &e.generics, kind, vis);
                self.p.adts[a.index()].diagnostic = diagnostic;
            }
            ast::ItemKind::Trait(t) => {
                let id = TraitId(self.p.traits.len() as u32);
                let param = ParamDef::new("Self".into(), t.name.span);
                let self_param = self.p.new_param(ParamDef { is_self: true, ..param });
                let generics = self.declare_generics(&t.generics);
                self.p.traits.push(TraitDef {
                    name: t.name.name.clone(),
                    module: m,
                    generics,
                    self_param,
                    supertraits: Vec::new(),
                    assoc_types: Vec::new(),
                    methods: Vec::new(),
                    public,
                    package_only: vis.package_only,
                    span: item.span,
                    lang: None,
                    fieldwise,
                    diagnostic,
                    field_param: None,
                });
                // A field a method's walk visits: any type with this trait (its bound is set
                // with the trait's header).
                if fieldwise {
                    let g = self.p.new_param(ParamDef::new("field".into(), t.name.span));
                    self.p.traits[id.index()].field_param = Some(g);
                }
                let mut methods = Vec::new();
                for member in &t.members {
                    if let ast::TraitMemberKind::Type { .. } = &member.kind {
                        self.attrs_not_on_fn(&member.attrs);
                    }
                    if let ast::TraitMemberKind::Fn(f) = &member.kind {
                        if let Some(prev) = methods
                            .iter()
                            .find(|(_, g): &&(FnId, &ast::FnDecl)| g.name.name == f.name.name)
                        {
                            let prev_span = prev.1.name.span;
                            self.diags.push(
                                Diagnostic::new(
                                    codes::E0201,
                                    f.name.span,
                                    format!("`{}` is declared twice in this trait", f.name.name),
                                )
                                .with_secondary(prev_span, "first declared here"),
                            );
                            continue;
                        }
                        let vis = Vis { public: true, ..vis };
                        let fid = self.new_fn(m, FnOwner::Trait(id), f, vis, &member.attrs, is_std);
                        methods.push((fid, f));
                    }
                }
                self.p.traits[id.index()].methods = methods.iter().map(|(f, _)| *f).collect();
                self.bind(m, &t.name, Res::Trait(id), vis);
                self.pending.push((m, PendingItem::Trait(id, t, methods)));
            }
            ast::ItemKind::Impl(i) => {
                let id = ImplId(self.p.impls.len() as u32);
                let generics = self.declare_generics(&i.generics);
                self.p.impls.push(ImplDef {
                    module: m,
                    generics,
                    trait_ref: None,
                    self_ty: self.p.types.error,
                    assoc_types: BTreeMap::new(),
                    methods: Vec::new(),
                    span: item.span,
                    from_opt_in: false,
                });
                let mut methods = Vec::new();
                for member in &i.members {
                    if let ast::ImplMemberKind::Fn(f) = &member.kind {
                        let vis = Vis {
                            public: member.vis.is_some() || i.trait_.is_some(),
                            ..Vis::of(&member.vis)
                        };
                        let fid = self.new_fn(m, FnOwner::Impl(id), f, vis, &member.attrs, is_std);
                        methods.push((fid, f));
                    }
                }
                self.p.impls[id.index()].methods = methods.iter().map(|(f, _)| *f).collect();
                self.pending.push((m, PendingItem::Impl(id, i, methods)));
            }
            ast::ItemKind::Const(c) => {
                let id = ConstId(self.p.consts.len() as u32);
                let fuel = self.const_attrs(&item.attrs);
                let eval = self.const_eval_fn(id, &c.name.name, m, item.span, c.name.span);
                self.p.consts.push(ConstDef {
                    name: c.name.name.clone(),
                    module: m,
                    ty: None,
                    annotation: c.ty.clone(),
                    value: c.value.clone(),
                    public,
                    span: item.span,
                    eval,
                    default: false,
                    fuel,
                });
                self.bind(m, &c.name, Res::Const(id), vis);
                self.pending.push((m, PendingItem::Const(id, c)));
            }
            ast::ItemKind::Use(u) => self.flatten_use(m, u, Vec::new(), vis),
            ast::ItemKind::TypeAlias(t) => {
                let id = AliasId(self.p.aliases.len() as u32);
                let generics = self.declare_generics(&t.generics);
                let ty = self.p.types.error;
                self.p.aliases.push(AliasDef {
                    name: t.name.name.clone(),
                    module: m,
                    generics,
                    ty,
                    defined_by: None,
                    public,
                    span: item.span,
                });
                self.bind(m, &t.name, Res::Alias(id), vis);
                self.pending.push((m, PendingItem::Alias(id, t)));
            }
            ast::ItemKind::TraitSet(t) => {
                let id = TraitSetId(self.p.trait_sets.len() as u32);
                let generics = self.declare_generics(&t.generics);
                self.p.trait_sets.push(TraitSetDef {
                    name: t.name.name.clone(),
                    module: m,
                    generics,
                    traits: Vec::new(),
                    public,
                    span: item.span,
                });
                self.bind(m, &t.name, Res::TraitSet(id), vis);
                self.pending.push((m, PendingItem::TraitSet(id, t)));
            }
        }
    }

    /// A trait's attributes (`trait_`: `@fieldwise` and `@diagnostic`), or a struct's or an
    /// enum's (`@diagnostic`): whether it's fieldwise, and its diagnostic's message.
    fn type_attrs(&mut self, attrs: &[ast::Attribute], trait_: bool) -> (bool, Option<String>) {
        let (mut fieldwise, mut diagnostic) = (false, None);
        for a in attrs {
            match a.name.name.as_str() {
                "fieldwise" if trait_ => {
                    if a.args.is_some() {
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0204,
                                a.span,
                                "`@fieldwise` takes no arguments",
                            )
                            .with_fix(
                                "remove the arguments",
                                a.span,
                                "@fieldwise",
                            ),
                        );
                    }
                    fieldwise = true;
                }
                "diagnostic" => {
                    let text = match a.args.as_deref() {
                        Some([arg]) => str_lit(&arg.value),
                        _ => None,
                    };
                    match text {
                        Some(t) => diagnostic = Some(t),
                        None => self.diags.push(
                            Diagnostic::new(
                                codes::E0204,
                                a.span,
                                "`@diagnostic` takes one string: the message",
                            )
                            .with_help(r#"write `@diagnostic("what's missing, and what to do")`"#),
                        ),
                    }
                }
                name => {
                    let on = if trait_ { "a trait" } else { "a struct or an enum" };
                    let takes =
                        if trait_ { "`@fieldwise` and `@diagnostic`" } else { "`@diagnostic`" };
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0108,
                            a.span,
                            format!("`@{name}` isn't an attribute of {on}"),
                        )
                        .with_note(format!("{on} takes {takes} (§9)"))
                        .with_fix("remove it", a.span, ""),
                    );
                }
            }
        }
        (fieldwise, diagnostic)
    }

    /// A constant's attributes: `@fuel(n)`, the work its computation may do (§13), at most
    /// once; any other is E0108.
    fn const_attrs(&mut self, attrs: &[ast::Attribute]) -> Option<u64> {
        let mut fuel = None;
        for a in attrs {
            if a.name.name != "fuel" {
                self.attrs_not_on_fn(std::slice::from_ref(a));
                continue;
            }
            self.fuel_attr(a, &mut fuel, "a constant's");
        }
        fuel
    }

    /// `@fuel(n)` into `fuel`, once (`whose`: "a constant's"); else E0108.
    fn fuel_attr(&mut self, a: &ast::Attribute, fuel: &mut Option<u64>, whose: &str) {
        if fuel.is_some() {
            self.diags.push(
                Diagnostic::new(codes::E0108, a.span, format!("{whose} fuel is set once"))
                    .with_fix("remove it", a.span, ""),
            );
            return;
        }
        let value = match a.args.as_deref() {
            Some([arg]) if arg.name.is_none() => fuel_value(&arg.value),
            _ => None,
        };
        match value {
            Some(v) if (1..=MAX_FUEL).contains(&v) => *fuel = Some(v),
            _ => self.diags.push(
                Diagnostic::new(
                    codes::E0108,
                    a.span,
                    "`@fuel` takes one whole number of fuel units, at most 2 ** 46",
                )
                .with_note("a unit is about one WASM instruction; the build's own limit is 2 ** 34, a test's 2 ** 37 (§13)")
                .with_help("write it as a literal or a power of two: `@fuel(2 ** 40)`"),
            ),
        }
    }

    /// E0108 for each attribute on something that isn't a function: an item, or an associated
    /// type.
    fn attrs_not_on_fn(&mut self, attrs: &[ast::Attribute]) {
        for a in attrs {
            self.diags.push(
                Diagnostic::new(
                    codes::E0108,
                    a.span,
                    format!("`@{}` only applies to functions", a.name.name),
                )
                .with_fix("remove it", a.span, ""),
            );
        }
    }

    /// The function the build runs to compute constant `id` ([`FnOwner::Const`]), written at
    /// `span`. Its type, once it's known, is the function's result (`check_program`).
    fn const_eval_fn(
        &mut self,
        id: ConstId,
        name: &str,
        m: ModuleId,
        span: Span,
        name_span: Span,
    ) -> FnId {
        self.p.fns.push(FnDef {
            name: name.to_string(),
            module: m,
            owner: FnOwner::Const(id),
            generics: Vec::new(),
            params: Vec::new(),
            ret: self.p.types.error,
            ret_mode: RetMode::Owned,
            opaque: None,
            attrs: FnAttrs::default(),
            body: None,
            public: false,
            package_only: false,
            span,
            name_span,
            sig_span: span,
            lang: None,
            derived: None,
        });
        FnId(self.p.fns.len() as u32 - 1)
    }

    /// Declares a struct or enum; its fields or variants come with the signatures.
    fn declare_adt(
        &mut self,
        m: ModuleId,
        item: &'u ast::Item,
        name: &ast::Ident,
        generics: &[ast::GenericParam],
        kind: AdtKind,
        vis: Vis,
    ) -> AdtId {
        let generics = self.declare_generics(generics);
        let id = AdtId(self.p.adts.len() as u32);
        self.p.adts.push(AdtDef {
            name: name.name.clone(),
            module: m,
            generics,
            kind,
            opt_in: Vec::new(),
            public: vis.public,
            span: item.span,
            name_span: name.span,
            lang: None,
            diagnostic: None,
            borrow: matches!(&item.kind, ast::ItemKind::Struct(s) if s.borrow),
            entry: None,
            fn_fields: Vec::new(),
        });
        // Each field of a function type is a value of a parameter of its own, after those
        // written (`fn.fields`).
        if let ast::ItemKind::Struct(s) = &item.kind {
            for f in &s.fields {
                if matches!(f.ty.kind, ast::TypeExprKind::Fn(_))
                    && !self.p.adts[id.index()].fn_fields.iter().any(|(n, _)| *n == f.name.name)
                {
                    let g =
                        self.p.new_param(ParamDef::new(format!("fn {}", f.name.name), f.ty.span));
                    let def = &mut self.p.adts[id.index()];
                    def.generics.push(g);
                    def.fn_fields.push((f.name.name.clone(), g));
                }
            }
        }
        self.bind(m, name, Res::Adt(id), vis);
        self.pending.push((m, PendingItem::Adt(id, item)));
        id
    }

    fn flatten_use(&mut self, m: ModuleId, u: &ast::UseTree, prefix: Vec<ast::Ident>, vis: Vis) {
        let mut path = prefix;
        path.extend(u.path.iter().cloned());
        match &u.kind {
            ast::UseKind::Simple(alias) => {
                let alias = alias.clone().unwrap_or_else(|| path[path.len() - 1].clone());
                self.uses.push(PendingUse { module: m, path, alias, vis, span: u.span });
            }
            ast::UseKind::Group(trees) => {
                for t in trees {
                    self.flatten_use(m, t, path.clone(), vis);
                }
            }
        }
    }

    fn fn_attrs(&mut self, attrs: &[ast::Attribute], is_std: bool, core: bool) -> FnAttrs {
        let mut out = FnAttrs::default();
        for a in attrs {
            let name = a.name.name.as_str();
            match name {
                "thread_entry" | "effects" if !core => {
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0204,
                            a.span,
                            format!("`@{name}` is for std's unsafe core alone"),
                        )
                        .with_note(match name {
                            "effects" => "effects are inferred, never written (§8): std's core states only what comes through memory from another thread or the host",
                            _ => "a host calls a thread entry on a thread of its own; programs start threads through std (`std::tick`, `std::par`, `std::audio`)",
                        }),
                    );
                }
                "thread_entry" => {
                    self.no_args(a, codes::E0204);
                    out.thread_entry = Some(a.span);
                }
                "effects" => out.effects = self.declared_effects(a),
                "compute" | "vertex" | "fragment" => {
                    if let Some((_, prev)) = out.entry {
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0602,
                                a.span,
                                "a function is at most one GPU entry point",
                            )
                            .with_secondary(prev, "already an entry point here"),
                        );
                        continue;
                    }
                    let entry = match name {
                        "vertex" | "fragment" => {
                            self.no_args(a, codes::E0602);
                            if name == "vertex" { Entry::Vertex } else { Entry::Fragment }
                        }
                        // A size that's wrong is reported; the kernel is still one, so its
                        // dispatches aren't reported too.
                        _ => Entry::Compute(self.workgroup_size(a).unwrap_or([1, 1, 1])),
                    };
                    out.entry = Some((entry, a.span));
                }
                "gpu" => {
                    self.no_args(a, codes::E0602);
                    out.gpu = Some(a.span);
                }
                "intrinsic" if is_std => out.intrinsic = true,
                "test" => {
                    match &a.args {
                        Some(args) if args.iter().any(is_tool_arg) => {
                            out.test_tool = self.tool_test(a, args);
                        }
                        Some(args) => {
                            let t = self.test_run(a, args);
                            out.test_run = t.run;
                            out.test_input = t.input;
                            out.test_gpu = t.gpu;
                            out.test_golden = t.golden;
                            out.test_hold = t.hold;
                            out.test_both = t.both;
                        }
                        None => {}
                    }
                    out.test = Some(a.span);
                }
                "testing" => {
                    self.no_args(a, codes::E0222);
                    out.testing = Some(a.span);
                }
                "job" => {
                    self.no_args(a, codes::E0336);
                    out.job = Some(a.span);
                }
                "client" | "server" | "tool" => {
                    self.no_args(a, codes::E0204);
                    let role = match name {
                        "client" => Role::Client,
                        "server" => Role::Server,
                        _ => Role::Tool,
                    };
                    if let Some((other, _)) = out.role
                        && other != role
                    {
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0204,
                                a.name.span,
                                format!(
                                    "a function has one role: it's `@{}` already, so not `@{name}`",
                                    other.name()
                                ),
                            )
                            .with_note(
                                "`@client`, `@server` and `@tool` each mark what only that role's build may run (§15.3)",
                            ),
                        );
                    }
                    out.role = Some((role, a.span));
                }
                "deterministic" | "audio" => {
                    self.no_args(a, codes::E0204);
                    if name == "deterministic" {
                        out.deterministic = Some(a.span);
                    } else {
                        out.audio = Some(a.span);
                    }
                }
                "fuel" => {
                    let mut fuel = out.fuel.map(|(v, _)| v);
                    self.fuel_attr(a, &mut fuel, "a test's");
                    out.fuel = fuel.map(|v| (v, a.span));
                }
                "fieldwise" | "diagnostic" => {
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0108,
                            a.span,
                            format!(
                                "`@{name}` goes on a {}, not a function",
                                if name == "fieldwise" { "trait" } else { "trait or a type" }
                            ),
                        )
                        .with_fix("remove it", a.span, ""),
                    );
                }
                "comptime" | "assert" | "assume" | "escaping" => {
                    let instead = match name {
                        "comptime" => "write the work as a `const`, which the build runs (§13)",
                        "escaping" => {
                            "a closure that captures only values can be stored as it is (§6.7)"
                        }
                        _ => {
                            "state a bound as a method, such as `lipschitz(near)`, and test it (§14)"
                        }
                    };
                    self.diags.push(
                        Diagnostic::new(codes::E0204, a.span, format!("wrela has no `@{name}`"))
                            .with_note("it was decided against (language.md §21)")
                            .with_help(instead),
                    );
                }
                _ => {
                    let known = [
                        "compute",
                        "vertex",
                        "fragment",
                        "gpu",
                        "deterministic",
                        "audio",
                        "test",
                        "testing",
                        "job",
                        "client",
                        "server",
                        "tool",
                    ];
                    let mut d = Diagnostic::new(
                        codes::E0204,
                        a.name.span,
                        format!("there's no attribute `@{name}`"),
                    )
                    .with_note("attributes are a closed set defined by the language (D-037)");
                    if let Some(s) = resolve::closest(name, known.iter().copied()) {
                        d = d.with_fix(format!("did you mean `@{s}`?"), a.name.span, s);
                    } else {
                        let all: Vec<String> = known.iter().map(|k| format!("`@{k}`")).collect();
                        d = d.with_help(format!(
                            "remove it; the attributes are {}, `@fuel`, `@fieldwise` and `@unsafe_call`",
                            all.join(", ")
                        ));
                    }
                    self.diags.push(d);
                }
            }
        }
        // A test of code may set its fuel; a frame test's calls each get a test's, and a tool
        // test has a budget of time instead.
        if let Some((_, at)) = out.fuel
            && (out.test.is_none() || out.test_run.is_some() || out.test_tool.is_some())
        {
            out.fuel = None;
            self.diags.push(
                Diagnostic::new(
                    codes::E0108,
                    at,
                    "`@fuel` goes on a `const` or a `@test` of code, not this function",
                )
                .with_note("a frame test's calls each get a test's fuel, 2 ** 37 (§13)")
                .with_fix("remove it", at, ""),
            );
        }
        out
    }

    /// `code` for an attribute `a` that takes no arguments but is given some.
    fn no_args(&mut self, a: &ast::Attribute, code: wrela_diag::Code) {
        if a.args.is_some() {
            let name = &a.name.name;
            self.diags.push(
                Diagnostic::new(code, a.span, format!("`@{name}` takes no arguments")).with_fix(
                    "remove the arguments",
                    a.span,
                    format!("@{name}"),
                ),
            );
        }
    }

    /// `@effects(nondet, io, ...)`'s effects (std's unsafe core).
    fn declared_effects(&mut self, a: &ast::Attribute) -> Vec<crate::effects::Effect> {
        use crate::effects::Effect;
        let mut out = Vec::new();
        for arg in a.args.iter().flatten() {
            let name = match &arg.value.kind {
                ast::ExprKind::Path(p) if p.is_single() && arg.name.is_none() => {
                    Some(p.segments[0].ident.name.as_str())
                }
                _ => None,
            };
            match name.and_then(Effect::named) {
                Some(e) => out.push(e),
                None => {
                    let names = Effect::ALL.map(Effect::name);
                    let (last, rest) = names.split_last().expect("there are effects");
                    let all = format!("{} or {last}", rest.join(", "));
                    self.diags.push(Diagnostic::new(
                        codes::E0204,
                        arg.span,
                        format!("`@effects` names effects: {all}"),
                    ));
                }
            }
        }
        out
    }

    /// `@test(tool, ...)` (§13.2): a tool test, with its options: `budget: s`, seconds from 0 to
    /// [`MAX_TOOL_TEST_BUDGET`]; `net: ["host", ...]`, the fake sites it reaches; `run:
    /// [("name", "dir"), ...]`, the programs it may run, each a tool package in a directory of
    /// the test's package; and `read: [("name", "dir"), ...]`, the roots it may read, each a
    /// directory relative to the test's package; `browser: true`, Chrome, on the real clock; and
    /// `blobs: "8M,64"`, the blob budget, as `wrela tool --blob-budget` takes it; and `workers:
    /// n`, its helpers, 0 to `MAX_WORKERS` ([`TOOL_TEST_WORKERS`] unless it says). Anything else
    /// is E0222.
    fn tool_test(&mut self, a: &ast::Attribute, args: &[ast::Arg]) -> Option<ToolTest> {
        let mut test = ToolTest {
            budget: TOOL_TEST_BUDGET,
            net: Vec::new(),
            run: Vec::new(),
            read: Vec::new(),
            browser: false,
            blobs: None,
            workers: TOOL_TEST_WORKERS,
        };
        let mut bad: Option<(Span, &str)> = None;
        let mut seen: Vec<&str> = Vec::new();
        for arg in args {
            let Some(name) = &arg.name else {
                if !is_tool_arg(arg) || seen.contains(&"tool") {
                    bad = bad
                        .or(Some((arg.span, "`tool` comes once, and every other option is named")));
                }
                seen.push("tool");
                continue;
            };
            if seen.contains(&name.name.as_str()) {
                bad = bad.or(Some((arg.span, "each option is given once")));
                continue;
            }
            seen.push(name.name.as_str());
            match name.name.as_str() {
                "budget" => {
                    let v = match &arg.value.kind {
                        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Float(v), .. }) => {
                            Some(*v)
                        }
                        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. }) => {
                            v.ok().map(|v| v as f64)
                        }
                        _ => None,
                    };
                    match v.filter(|v| *v > 0.0 && *v <= MAX_TOOL_TEST_BUDGET) {
                        Some(v) => test.budget = v,
                        None => {
                            bad = bad.or(Some((
                                arg.span,
                                "`budget` is seconds, more than 0 and at most 600",
                            )))
                        }
                    }
                }
                "blobs" => match str_lit(&arg.value).filter(|b| is_blob_budget(b)) {
                    Some(b) => test.blobs = Some(b),
                    None => {
                        bad = bad.or(Some((
                            arg.span,
                            "`blobs` is a blob budget, as `wrela tool --blob-budget` takes it: bytes (`8M`), then `,` and handles",
                        )))
                    }
                },
                "workers" => match &arg.value.kind {
                    ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(ast::IntValue::Ok(n)), .. })
                        if *n <= u64::from(wrela_abi::memory::MAX_WORKERS) =>
                    {
                        test.workers = *n as u32;
                    }
                    _ => {
                        bad = bad.or(Some((
                            arg.span,
                            "`workers` is how many helpers it has, a whole number from 0 to 8",
                        )))
                    }
                },
                "browser" => match &arg.value.kind {
                    ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Bool(on), .. }) => {
                        test.browser = *on;
                    }
                    _ => bad = bad.or(Some((arg.span, "`browser` is `true` or `false`"))),
                },
                "net" => {
                    let hosts = match &arg.value.kind {
                        ast::ExprKind::Array(items) => {
                            items.iter().map(str_lit).collect::<Option<Vec<String>>>()
                        }
                        _ => None,
                    };
                    match hosts.filter(|h| h.iter().all(|h| is_host(h))) {
                        Some(h) => test.net = h,
                        None => {
                            bad = bad.or(Some((
                                arg.span,
                                "`net` is an array of hosts, such as `[\"game.example\"]`: lowercase letters, digits, `-` and `.`",
                            )))
                        }
                    }
                }
                "run" | "read" => {
                    let pairs = match &arg.value.kind {
                        ast::ExprKind::Array(items) => items
                            .iter()
                            .map(|i| match &i.kind {
                                ast::ExprKind::Tuple(parts) if parts.len() == 2 => {
                                    Some((str_lit(&parts[0])?, str_lit(&parts[1])?))
                                }
                                _ => None,
                            })
                            .collect::<Option<Vec<(String, String)>>>(),
                        _ => None,
                    };
                    let read = name.name == "read";
                    // A root may lead above the package (`..`); a program is inside it.
                    let dir_ok = |dir: &str| {
                        if read {
                            !dir.is_empty()
                                && !dir.starts_with('/')
                                && !dir.contains(['\\', '\0'])
                                && dir.split('/').all(|p| !p.is_empty() && p != ".")
                        } else {
                            wrela_abi::check::path_problem(dir).is_none()
                        }
                    };
                    let ok = |(name, dir): &(String, String)| {
                        !name.is_empty()
                            && name != "."
                            && name != ".."
                            && name != "scratch"
                            && name.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
                            })
                            && dir_ok(dir)
                    };
                    match pairs.filter(|p| p.iter().all(ok)) {
                        Some(p) if read => test.read = p,
                        Some(p) => test.run = p,
                        None if read => {
                            bad = bad.or(Some((
                                arg.span,
                                "`read` is an array of `(\"name\", \"dir\")`: a root's name (not `scratch`), and its directory, relative to the test's package",
                            )))
                        }
                        None => {
                            bad = bad.or(Some((
                                arg.span,
                                "`run` is an array of `(\"name\", \"dir\")`: a program's name, and the directory in the package of the tool package that stands in for it",
                            )))
                        }
                    }
                }
                _ => {
                    bad = bad.or(Some((
                        arg.span,
                        "a tool test takes `budget`, `net`, `run`, `read`, `browser`, `blobs` and `workers`",
                    )))
                }
            }
        }
        if let Some((span, why)) = bad {
            self.diags.push(
                Diagnostic::new(codes::E0222, span, format!("`@test(tool, ...)`: {why}"))
                    .with_secondary(a.span, "in this tool test")
                    .with_note("`@test(tool, budget: 5, net: [\"game.example\"], run: [(\"wrangler\", \"fakes/wrangler\")])` runs a function on the package's tool build, with fake sites and programs (§13.2)"),
            );
            return None;
        }
        Some(test)
    }

    /// `@test(frames: n)` or `@test(ticks: n)` (§13), `n` from 1 to [`MAX_TEST_FRAMES`], and,
    /// with `input: NAME` (a `std::input::Script` constant, resolved with the signatures) or
    /// `input: "path"` (a file in the package), the script of input events (or the tick log)
    /// they get; with `gpu: true`, frames that run on the native host's GPU, and with
    /// `golden: "frame.png"` too, the PNG the screen after them is compared with, `within` a
    /// mean difference; and with `hold: [("path", k), ...]` on frames, the paths whose fetches
    /// and storage reads wait until frame `k` begins.
    /// Anything else is E0222.
    fn test_run(&mut self, a: &ast::Attribute, args: &[ast::Arg]) -> FrameTest {
        let named =
            |name: &str| args.iter().find(|x| x.name.as_ref().is_some_and(|n| n.name == name));
        let others = args.iter().any(|x| {
            !x.name.as_ref().is_some_and(|n| {
                matches!(
                    n.name.as_str(),
                    "frames"
                        | "ticks"
                        | "input"
                        | "gpu"
                        | "golden"
                        | "within"
                        | "pass"
                        | "hold"
                        | "hosts"
                )
            })
        });
        let count = |name: &str| match named(name).map(|x| &x.value.kind) {
            Some(ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. })) => {
                v.ok().filter(|&n| (1..=u64::from(MAX_TEST_FRAMES)).contains(&n))
            }
            _ => None,
        };
        let (frames, ticks) = (named("frames").is_some(), named("ticks").is_some());
        let n = match (frames, ticks) {
            (true, false) => count("frames"),
            (false, true) => count("ticks"),
            _ => None,
        };
        let path = |name: &str| match named(name) {
            None => Ok(None),
            Some(x) => str_lit(&x.value).map(|path| Some((path, x.value.span))).ok_or(()),
        };
        let input = match named("input").map(|x| (&x.value.kind, x.value.span)) {
            Some((ast::ExprKind::Path(p), span)) => Ok(Some((TestInput::Named(p.clone()), span))),
            _ => path("input").map(|p| p.map(|(path, span)| (TestInput::File(path), span))),
        };
        // `gpu: true`: frames on the GPU (not ticks, which draw nothing).
        let gpu = match named("gpu").map(|x| &x.value.kind) {
            None => Ok(false),
            Some(ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Bool(b), .. })) if frames => {
                Ok(*b)
            }
            Some(_) => Err(()),
        };
        // `golden: "frame.png"` on a GPU frame test, `within` a mean difference from 0 to 255.
        let within = match named("within").map(|x| &x.value.kind) {
            None => Ok(GOLDEN_WITHIN),
            Some(ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Float(v), .. })) => {
                Some(*v).filter(|v| (0.0..=255.0).contains(v)).ok_or(())
            }
            Some(ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. })) => {
                v.ok().filter(|&v| v <= 255).map(|v| v as f64).ok_or(())
            }
            Some(_) => Err(()),
        };
        // `pass: "label"`: a golden of that pass's target, not the screen.
        let pass = match named("pass") {
            None => Ok(None),
            Some(x) => str_lit(&x.value).filter(|l| !l.is_empty()).map(Some).ok_or(()),
        };
        let golden = match (path("golden"), within, gpu, pass) {
            (Ok(None), _, _, Ok(None)) if named("within").is_none() => Ok(None),
            (Ok(Some((path, span))), Ok(within), Ok(true), Ok(pass)) => {
                Ok(Some(Golden { path, span, within, pass }))
            }
            _ => Err(()),
        };
        // `hold: [("far.bin", 120)]` on frames: each path inside the build, and the frame it's
        // let go at.
        let hold = match named("hold").map(|x| &x.value.kind) {
            None => Ok(Vec::new()),
            Some(ast::ExprKind::Array(items)) if frames => items
                .iter()
                .map(|i| match &i.kind {
                    ast::ExprKind::Tuple(parts) if parts.len() == 2 => {
                        let path = str_lit(&parts[0])
                            .filter(|p| wrela_abi::check::path_problem(p).is_none())?;
                        match &parts[1].kind {
                            ast::ExprKind::Lit(ast::Lit {
                                kind: ast::LitKind::Int(ast::IntValue::Ok(k)),
                                ..
                            }) if *k <= u64::from(u32::MAX) => Some((path, *k as u32)),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect::<Option<Vec<(String, u32)>>>()
                .ok_or(()),
            Some(_) => Err(()),
        };
        // `hosts: both` on GPU frames that hold nothing (the browser holds no request).
        let both = match named("hosts").map(|x| &x.value.kind) {
            None => Ok(false),
            Some(ast::ExprKind::Path(p))
                if p.is_single()
                    && p.segments[0].ident.name == "both"
                    && gpu == Ok(true)
                    && named("hold").is_none() =>
            {
                Ok(true)
            }
            Some(_) => Err(()),
        };
        match (n, input, gpu, golden, hold, both) {
            (Some(n), Ok(input), Ok(gpu), Ok(golden), Ok(hold), Ok(both)) if !others => {
                let n = n as u32;
                let run = if frames { TestRun::Frames(n) } else { TestRun::Ticks(n) };
                FrameTest { run: Some(run), input, gpu, golden, hold, both }
            }
            _ => {
                self.diags.push(
                    Diagnostic::new(
                        codes::E0222,
                        a.span,
                        format!("`@test` takes `frames: n` or `ticks: n`, from 1 to {MAX_TEST_FRAMES}: how many of the program's frames, or ticks, run before the test; and may take `input: NAME`, a `std::input::Script` constant of input events for them, or `input: \"path\"`, a file in the package (a script, or with `ticks` a tick log to check them against)"),
                    )
                    .with_note("`@test` alone is a test of code; `@test(frames: 600)` runs the program for 600 frames, then the test with its state; `gpu: true` runs them on the GPU (§13), and `golden: \"frame.png\", within: 0.5` with it compares the screen after them with that PNG, within a mean difference in 8-bit steps (`pass: \"label\"`: that pass's target instead), and `hosts: both` runs them in Chrome too, whose last frame and state hash must be the same; `hold: [(\"far.bin\", 120)]` holds the fetches and storage reads of a path until a frame")
                    .with_help("write `@test`, `@test(frames: 60)` or `@test(ticks: 60)`, with only the options that note lists for that kind"),
                );
                FrameTest::default()
            }
        }
    }

    /// `@compute(x)`, `@compute(x, y)` or `@compute(x, y, z)`, within WebGPU's default limits.
    fn workgroup_size(&mut self, a: &ast::Attribute) -> Option<[u32; 3]> {
        let Some(args) = &a.args else {
            self.diags.push(
                Diagnostic::new(codes::E0602, a.span, "`@compute` needs a workgroup size")
                    .with_help("write `@compute(64)` for 64 invocations per workgroup")
                    .with_fix("use a workgroup of 64", a.span, "@compute(64)"),
            );
            return None;
        };
        if args.is_empty() || args.len() > 3 {
            self.diags.push(Diagnostic::new(
                codes::E0602,
                a.span,
                "`@compute` takes one to three sizes: `@compute(x, y, z)`",
            ));
            return None;
        }
        let mut size = [1u32; 3];
        for (i, arg) in args.iter().enumerate() {
            let v = match &arg.value.kind {
                ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. })
                    if arg.name.is_none() =>
                {
                    v.ok()
                }
                _ => None,
            };
            match v {
                Some(v) if v >= 1 && v <= u32::MAX as u64 => size[i] = v as u32,
                _ => {
                    self.diags.push(Diagnostic::new(
                        codes::E0605,
                        arg.span,
                        "a workgroup size is a positive integer literal",
                    ));
                    return None;
                }
            }
        }
        // The limits a manifest is checked against (WebGPU's defaults).
        use wrela_abi::manifest::{MAX_WORKGROUP_INVOCATIONS, MAX_WORKGROUP_SIZE};
        let limits = MAX_WORKGROUP_SIZE;
        for i in 0..3 {
            if size[i] > limits[i] {
                let axis = ["x", "y", "z"][i];
                self.diags.push(
                    Diagnostic::new(
                        codes::E0605,
                        args[i].span,
                        format!(
                            "a workgroup's {axis} size can be at most {}, not {}",
                            limits[i], size[i]
                        ),
                    )
                    .with_note(format!(
                        "WebGPU's default limits: {}, {} and {}, and {MAX_WORKGROUP_INVOCATIONS} invocations in all",
                        limits[0], limits[1], limits[2]
                    ))
                    .with_help(format!("use a workgroup of at most {} along {axis}, and dispatch more groups", limits[i])),
                );
                return None;
            }
        }
        let total = size[0] as u64 * size[1] as u64 * size[2] as u64;
        if total > u64::from(MAX_WORKGROUP_INVOCATIONS) {
            self.diags.push(
                Diagnostic::new(
                    codes::E0605,
                    a.span,
                    format!(
                        "a workgroup can have at most {MAX_WORKGROUP_INVOCATIONS} invocations, not {total}"
                    ),
                )
                .with_note(format!(
                    "WebGPU's default limit is {MAX_WORKGROUP_INVOCATIONS} invocations per workgroup"
                )),
            );
            return None;
        }
        Some(size)
    }

    // ---- imports ---------------------------------------------------------------------------

    /// The binding made by an item's declaration, rather than an import that happens to name
    /// it. A private local alias does not make a public item private, but a public import must
    /// not expose an item whose declaration itself is private.
    fn declared_binding(&self, res: Res) -> Option<Binding> {
        let (module, name, declared) = match res {
            Res::Adt(a) => {
                let d = self.p.adt(a);
                (d.module, d.name.as_str(), Res::Adt(a))
            }
            Res::Variant(a, _) => {
                let d = self.p.adt(a);
                (d.module, d.name.as_str(), Res::Adt(a))
            }
            Res::Trait(t) => {
                let d = self.p.trait_(t);
                (d.module, d.name.as_str(), Res::Trait(t))
            }
            Res::Fn(f) => {
                let d = self.p.func(f);
                (d.module, d.name.as_str(), Res::Fn(f))
            }
            Res::Const(c) => {
                let d = self.p.const_(c);
                (d.module, d.name.as_str(), Res::Const(c))
            }
            Res::Alias(a) => {
                let d = &self.p.aliases[a.index()];
                (d.module, d.name.as_str(), Res::Alias(a))
            }
            Res::TraitSet(s) => {
                let d = &self.p.trait_sets[s.index()];
                (d.module, d.name.as_str(), Res::TraitSet(s))
            }
            Res::Module(_) | Res::BuiltinTy(_) | Res::BuiltinFn(_) => return None,
        };
        self.p.module(module).scope.get(name).filter(|b| b.res == declared).cloned()
    }

    fn resolve_imports(&mut self) {
        let mut pending: Vec<PendingUse> = std::mem::take(&mut self.uses);
        loop {
            let before = pending.len();
            // How many of the imports still waiting bind each name, and where.
            let mut binds: HashMap<(ModuleId, &str), usize> = HashMap::new();
            for u in &pending {
                *binds.entry((u.module, &u.alias.name)).or_default() += 1;
            }
            // Whether another of them binds each one's first segment.
            let others_bind: Vec<bool> = pending
                .iter()
                .map(|u| {
                    let first = u.path[0].name.as_str();
                    let own = usize::from(u.alias.name == first);
                    binds.get(&(u.module, first)).is_some_and(|&n| n > own)
                })
                .collect();
            // Each import still waiting, with the name it waits for and where.
            let mut still = Vec::new();
            for (u, others_bind) in pending.into_iter().zip(others_bind) {
                // A path's first segment is a name in the module's scope before a top-level
                // module or a global: while another import may still bind it, wait for that
                // one, so the order of the `use` lines doesn't matter.
                let first = &u.path[0].name;
                let bound_later = first != "std"
                    && !self.p.module(u.module).scope.contains_key(first)
                    && others_bind;
                if bound_later {
                    let (m, name) = (u.module, first.clone());
                    still.push((u, m, name));
                    continue;
                }
                match resolve::resolve_module_path_in(&self.p, u.module, &u.path, false) {
                    resolve::PathLookup::Found(res) => {
                        // A public import must not turn a less-visible binding into public API.
                        // The resolver deliberately lets a module name its own private bindings;
                        // imports need this separate check because their new binding can be wider.
                        let widened = if u.vis.public {
                            self.declared_binding(res)
                                .filter(|b| !b.public || !u.vis.package_only && b.package_only)
                        } else {
                            None
                        };
                        if let Some(b) = widened {
                            let visibility = if b.package_only {
                                "visible only inside this package"
                            } else {
                                "private to this module"
                            };
                            self.diags.push(
                                Diagnostic::new(
                                    codes::E0203,
                                    u.alias.span,
                                    format!(
                                        "`{}` can't be re-exported: it is {visibility}",
                                        u.path.last().map_or(&u.alias.name, |name| &name.name)
                                    ),
                                )
                                .with_secondary(b.span, "the less-visible binding is here")
                                .with_help(
                                    "give the original item at least the re-export's visibility",
                                ),
                            );
                            self.p.modules[u.module.index()].broken.insert(u.alias.name.clone());
                            continue;
                        }
                        self.bind(u.module, &u.alias, res, u.vis);
                    }
                    resolve::PathLookup::NotYet(m, name) => still.push((u, m, name)),
                    resolve::PathLookup::Broken => {
                        self.p.modules[u.module.index()].broken.insert(u.alias.name.clone());
                    }
                    resolve::PathLookup::Error(d) => {
                        // Reported once: uses of the name it would have bound stay quiet.
                        self.diags.push(*d);
                        self.p.modules[u.module.index()].broken.insert(u.alias.name.clone());
                    }
                }
            }
            if still.is_empty() {
                break;
            }
            if still.len() == before {
                self.report_stuck_imports(still);
                break;
            }
            pending = still.into_iter().map(|(u, ..)| u).collect();
        }
    }

    /// Imports that made no progress in a pass, each with the name it waits for. One that waits
    /// for another of them is in a cycle of imports, or depends on one: each cycle is reported
    /// once (E0209), and what depends on it stays quiet. The rest wait for names nothing
    /// provides, and are reported as such.
    fn report_stuck_imports(&mut self, stuck: Vec<(PendingUse, ModuleId, String)>) {
        let provider = |m: ModuleId, name: &str| {
            stuck.iter().position(|(v, ..)| v.module == m && v.alias.name == name)
        };
        let next: Vec<Option<usize>> = stuck.iter().map(|(_, m, n)| provider(*m, n)).collect();
        let mut on_cycle = vec![false; stuck.len()];
        for start in 0..stuck.len() {
            // Floyd would do; the chains are short, so walk with a visited list.
            let mut seen = vec![start];
            let mut at = start;
            while let Some(n) = next[at] {
                if let Some(k) = seen.iter().position(|&s| s == n) {
                    for &s in &seen[k..] {
                        on_cycle[s] = true;
                    }
                    break;
                }
                seen.push(n);
                at = n;
            }
        }
        let path_text =
            |u: &PendingUse| u.path.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join("::");
        let mut reported = vec![false; stuck.len()];
        for i in 0..stuck.len() {
            let u = &stuck[i].0;
            if on_cycle[i] {
                if reported[i] {
                    continue;
                }
                // The cycle through `i`, in order.
                let mut cycle = vec![i];
                let mut at = next[i].expect("on a cycle");
                while at != i {
                    cycle.push(at);
                    at = next[at].expect("on a cycle");
                }
                let mut d = Diagnostic::new(
                    codes::E0209,
                    u.path.last().map_or(u.span, |s| s.span),
                    format!(
                        "`{}` can't be resolved: the imports refer to each other in a cycle",
                        path_text(u)
                    ),
                );
                for &k in &cycle {
                    reported[k] = true;
                    if k != i {
                        let v = &stuck[k].0;
                        d = d.with_secondary(
                            v.path.last().map_or(v.span, |s| s.span),
                            format!("`{}` is imported from `{}`", v.alias.name, path_text(v)),
                        );
                    }
                }
                self.diags.push(d.with_help("import the item from the module that declares it"));
            } else if next[i].is_some() {
                // Waits on a cycle, which is reported.
                self.p.modules[u.module.index()].broken.insert(u.alias.name.clone());
            } else {
                match resolve::resolve_module_path_in(&self.p, u.module, &u.path, true) {
                    resolve::PathLookup::Error(d) => self.diags.push(*d),
                    resolve::PathLookup::Broken => {}
                    _ => self.diags.push(Diagnostic::internal(format!(
                        "the import `{}` resolved only in its final pass",
                        path_text(u)
                    ))),
                }
                self.p.modules[u.module.index()].broken.insert(u.alias.name.clone());
            }
        }
    }

    fn find_lang_items(&mut self) {
        let Some(std) = self.p.std_root else { return };
        for &(lang, path) in Lang::PATHS {
            // The compiler relies on every lang item; std not having one is a bug in std. Each
            // is declared where its path says (not re-exported), so the path is followed
            // through modules' children to the item in the last one's scope.
            let mut segs = path.split("::").skip(1).peekable();
            let mut m = std;
            let mut found = None;
            while let Some(seg) = segs.next() {
                let module = self.p.module(m);
                if segs.peek().is_none() {
                    found = module.scope.get(seg).map(|b| b.res);
                } else if let Some(&c) = module.children.get(seg) {
                    m = c;
                } else {
                    break;
                }
            }
            match found {
                Some(Res::Adt(a)) => {
                    self.p.adts[a.index()].lang = Some(lang);
                    self.p.lang.insert(lang, LangRes::Adt(a));
                }
                Some(Res::Trait(t)) => {
                    self.p.traits[t.index()].lang = Some(lang);
                    self.p.lang.insert(lang, LangRes::Trait(t));
                }
                Some(Res::Fn(f)) => {
                    self.p.fns[f.index()].lang = Some(lang);
                    self.p.lang.insert(lang, LangRes::Fn(f));
                }
                _ => self.diags.push(Diagnostic::internal(format!(
                    "std has no `{path}`, which the compiler relies on"
                ))),
            }
        }
        // The unit suffixes: every constant `std::units` declares (§5).
        if let Some(&units) = self.p.module(std).children.get("units") {
            let found: Vec<(String, ConstId)> = self
                .p
                .module(units)
                .scope
                .iter()
                .filter_map(|(name, b)| match b.res {
                    Res::Const(c) if self.p.const_(c).module == units => Some((name.clone(), c)),
                    _ => None,
                })
                .collect();
            self.p.units.extend(found);
        }
    }

    // ---- signatures ------------------------------------------------------------------------

    /// Each `Packed` struct (§3): its fields, each a `Bits<N>`, become bits of its one real
    /// field, a `u32` word, the first in the low bits. E0407 for a field of another type, a
    /// width outside 1 to 32, bits past 32 in all, a default, or an enum.
    fn make_packed(&mut self) {
        let Some(packed) = self.p.lang_trait(Lang::Packed) else { return };
        let bits = self.p.lang_adt(Lang::Bits);
        for i in 0..self.p.adts.len() {
            let a = AdtId(i as u32);
            let adt = &self.p.adts[i];
            let Some((_, at)) = adt.opt_in.iter().find(|(r, _)| r.trait_ == packed) else {
                continue;
            };
            let at = *at;
            let name = adt.name.clone();
            let mut problems = Vec::new();
            let mut out = Vec::new();
            let mut shift = 0u32;
            if adt.is_enum() {
                problems.push((at, format!("`{name}` can't be `Packed`: it's an enum")));
            }
            for f in adt.fields() {
                let width = match self.p.types.kind(f.ty) {
                    TyKind::Adt(b, args) if Some(*b) == bits => {
                        match args.first().map(|&t| self.p.types.kind(t)) {
                            Some(&TyKind::ConstU32(n)) => Some(n),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                match width {
                    Some(n) if (1..=32).contains(&n) => {
                        if f.default.is_some() {
                            problems.push((
                                f.span,
                                format!("`{}` is packed, so it has no default", f.name),
                            ));
                        }
                        if shift + n > 32 {
                            problems.push((f.span, format!("`{}`'s {n} bits are past the word's 32: the fields before it take {shift}", f.name)));
                        }
                        out.push(PackedField {
                            name: f.name.clone(),
                            shift,
                            width: n,
                            public: f.public,
                            span: f.span,
                        });
                        shift += n;
                    }
                    Some(n) => problems.push((
                        f.span,
                        format!("`{}` is {n} bits wide; a packed field is 1 to 32", f.name),
                    )),
                    None => {
                        let shown = self.p.display_ty(f.ty);
                        problems.push((f.span, format!("`{name}` can't be `Packed`: the field `{}` is a `{shown}`, not a `Bits<N>`", f.name)));
                    }
                }
            }
            if !problems.is_empty() {
                for (span, msg) in problems {
                    self.diags.push(
                        Diagnostic::new(codes::E0407, span, msg)
                            .with_note("a `Packed` struct's fields are `Bits<N>`, packed into one `u32`, the first in the low bits (§3)")
                            .with_help("give each field a `Bits<N>` type, N from 1 to 32 and 32 bits in all, with no default; or don't declare `Packed`"),
                    );
                }
                continue;
            }
            let word = FieldDef {
                name: "bits".into(),
                ty: self.p.types.u32,
                public: false,
                package_only: false,
                default: None,
                span: at,
                mode: RetMode::Owned,
            };
            self.p.adts[i].kind = AdtKind::Struct(vec![word]);
            self.p.packed.insert(a, out);
        }
    }

    /// Each entry point's bound type (§18): the borrow struct of its arguments that
    /// `name.bind(...)` makes, generic over the entry point's generics.
    fn make_bound_types(&mut self) {
        for i in 0..self.p.fns.len() {
            let f = FnId(i as u32);
            let def = self.p.func(f);
            if def.attrs.entry.is_none() {
                continue;
            }
            let (name, module, span, name_span) =
                (def.name.clone(), def.module, def.sig_span, def.name_span);
            let fields = crate::gpu::bound_fields(&self.p, f)
                .into_iter()
                .map(|b| {
                    let ps = &self.p.func(f).params[b.param];
                    FieldDef {
                        name: ps.name.clone(),
                        ty: b.ty,
                        public: false,
                        package_only: false,
                        default: None,
                        span: ps.span,
                        mode: b.mode,
                    }
                })
                .collect();
            let id = AdtId(self.p.adts.len() as u32);
            self.p.adts.push(AdtDef {
                name: format!("{name}.bind(...)"),
                module,
                generics: self.p.fn_all_generics(f),
                kind: AdtKind::Struct(fields),
                opt_in: Vec::new(),
                public: false,
                span,
                name_span,
                lang: None,
                diagnostic: None,
                borrow: true,
                entry: Some(f),
                fn_fields: Vec::new(),
            });
            self.p.bound_types.insert(f, id);
        }
    }

    fn resolve_signatures(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        self.resolve_aliases(&pending);
        self.resolve_trait_sets(&pending);
        // Bounds, supertraits and impl headers first, so later lookups (`T::Kind`) can see
        // them. A projection can name a trait declared later, or lean on a bound declared later
        // in its list (`fn f<T: Conv<U::K>, U: Has>`), so these are resolved quietly until that
        // stops helping, then once more to report.
        let mut errors = usize::MAX;
        loop {
            let mut quiet = Vec::new();
            std::mem::swap(self.diags, &mut quiet);
            self.headers(&pending);
            std::mem::swap(self.diags, &mut quiet);
            let n = quiet.iter().filter(|d| d.is_error()).count();
            if n == 0 || n >= errors {
                break;
            }
            errors = n;
        }
        let supertraits = self.headers(&pending);
        self.supertrait_cycles(&supertraits);
        for (m, item) in &pending {
            match item {
                PendingItem::Trait(_, _, methods) | PendingItem::Impl(_, _, methods) => {
                    for &(fid, f) in methods {
                        self.fn_bounds(fid, f);
                        self.fn_signature(fid, f);
                    }
                }
                PendingItem::Fn(id, f) => self.fn_signature(*id, f),
                PendingItem::Adt(id, item) => self.adt_body(*m, *id, item),
                PendingItem::Alias(..) | PendingItem::TraitSet(..) => {}
                PendingItem::Const(id, c) => {
                    if let Some(t) = &c.ty {
                        let scope = Scope::new(*m);
                        let ty =
                            resolve::resolve_type(&self.p, self.diags, &scope, t, TyPos::Normal);
                        self.p.consts[id.index()].ty = Some(ty);
                    }
                }
            }
        }
        // A test's signature names the program's state, which `init`'s signature gives, in any
        // module of the package: so the tests are checked once every signature is resolved,
        // whatever order the modules' files come in.
        for (_, item) in &pending {
            match item {
                PendingItem::Trait(_, _, methods) | PendingItem::Impl(_, _, methods) => {
                    for &(fid, _) in methods {
                        self.test_signature(fid);
                    }
                }
                PendingItem::Fn(id, _) => {
                    self.test_signature(*id);
                    self.test_input(*id);
                    self.testing_export(*id);
                }
                _ => {}
            }
        }
    }

    /// Resolves the bounds, supertraits and impl headers of `pending`; returns each supertrait
    /// edge, with where it's named.
    fn headers(
        &mut self,
        pending: &[(ModuleId, PendingItem<'u>)],
    ) -> Vec<(TraitId, TraitId, Span)> {
        let mut supertraits = Vec::new();
        for (m, item) in pending {
            match item {
                PendingItem::Trait(id, t, _) => {
                    let edges = self.trait_header(*m, *id, t);
                    supertraits.extend(edges.into_iter().map(|(to, span)| (*id, to, span)));
                }
                PendingItem::Adt(id, item) => self.adt_bounds(*m, *id, item),
                PendingItem::Impl(id, i, _) => self.impl_header(*m, *id, i),
                PendingItem::Fn(id, f) => self.fn_bounds(*id, f),
                PendingItem::Const(..) | PendingItem::Alias(..) | PendingItem::TraitSet(..) => {}
            }
        }
        supertraits
    }

    /// Resolves every type alias's type, each after the aliases it names, so an alias can name
    /// another declared after it. One that names itself, directly or through others, is E0318.
    fn resolve_aliases(&mut self, pending: &[(ModuleId, PendingItem<'u>)]) {
        let aliases: Vec<(ModuleId, AliasId, &ast::TypeAliasDecl)> = pending
            .iter()
            .filter_map(|(m, it)| match it {
                PendingItem::Alias(id, t) => Some((*m, *id, *t)),
                _ => None,
            })
            .collect();
        if aliases.is_empty() {
            return;
        }
        let index: HashMap<AliasId, usize> =
            aliases.iter().enumerate().map(|(i, (_, id, _))| (*id, i)).collect();
        // The aliases each one's type names, by the single names in its paths.
        let deps: Vec<Vec<usize>> = aliases
            .iter()
            .map(|(m, _, t)| {
                let mut out = Vec::new();
                alias_refs(&self.p, *m, &t.ty, &t.generics, &mut |a| {
                    if let Some(&i) = index.get(&a) {
                        out.push(i);
                    }
                });
                out
            })
            .collect();
        let mut order = Vec::new();
        let mut bad = vec![false; aliases.len()];
        dfs_cycles(
            &deps,
            |&d| d,
            |v| match v {
                Visit::Cycle(cycle) => {
                    let first = cycle[0].0;
                    let (_, _, t) = aliases[first];
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0318,
                            t.name.span,
                            format!("the type alias `{}` names itself", t.name.name),
                        )
                        .with_help("an alias is another name for a type; give it a type that doesn't name it"),
                    );
                    for &(i, _) in cycle.iter() {
                        bad[i] = true;
                    }
                }
                Visit::Done(i) => order.push(i),
            },
        );
        for i in order {
            if bad[i] {
                continue;
            }
            let (m, id, t) = aliases[i];
            let mut scope = Scope::new(m);
            scope.push_params(&self.p, &self.p.aliases[id.index()].generics);
            // An alias that names traits names the type a function returns (`ty.opaque-alias`).
            let mut bounds = None;
            let pos = TyPos::Alias(&mut bounds);
            let mut ty = resolve::resolve_type(&self.p, self.diags, &scope, &t.ty, pos);
            if let Some(bounds) = bounds {
                ty = self.opaque_alias(m, id, t, bounds, pending);
            }
            self.p.aliases[id.index()].ty = ty;
        }
    }

    /// The type an alias that names `bounds` names: the type its module's first function to
    /// return it returns, hidden behind those traits (§4). That function's body decides the
    /// type; other functions that return the alias return a value of it.
    fn opaque_alias(
        &mut self,
        m: ModuleId,
        id: AliasId,
        t: &ast::TypeAliasDecl,
        bounds: Vec<TraitRef>,
        pending: &[(ModuleId, PendingItem<'u>)],
    ) -> TyId {
        let name = &t.name.name;
        if !t.generics.is_empty() {
            self.diags.push(
                Diagnostic::new(
                    codes::E0410,
                    t.name.span,
                    format!("`{name}` names traits, so it has no parameters"),
                )
                .with_help(
                    "remove the generic arguments: a trait alias stands for its traits as they are",
                )
                .with_note("it names the one type a function returns (§4)"),
            );
            return self.p.types.error;
        }
        let returns_it = |d: &ast::FnDecl| {
            d.ret.as_ref().is_some_and(|r| match &r.ty.kind {
                ast::TypeExprKind::Path(p) => {
                    p.segments.len() == 1
                        && p.segments[0].ident.name == *name
                        && p.segments[0].generics.is_none()
                }
                _ => false,
            })
        };
        let def = pending.iter().find_map(|(pm, it)| match it {
            PendingItem::Fn(f, d) if *pm == m && returns_it(d) => Some(*f),
            _ => None,
        });
        let Some(f) = def else {
            self.diags.push(
                Diagnostic::new(
                    codes::E0410,
                    t.name.span,
                    format!("`{name}` names traits, and no function in this module returns it"),
                )
                .with_note("an alias that names traits names the type a function returns: the first function in its module whose result is the alias (§4)")
                .with_help(format!("return it from a function in this module: `fn make() -> {name} {{ ... }}`")),
            );
            return self.p.types.error;
        };
        self.p.aliases[id.index()].defined_by = Some(f);
        self.alias_fns.insert(f, (id, bounds));
        self.p.types.intern(TyKind::Opaque(f, Vec::new()))
    }

    /// Resolves every trait set's traits, each after the sets it names, so a set can name
    /// another declared after it; a set's traits are flat, with the sets it names expanded.
    /// One that names itself, directly or through others, is E0418.
    fn resolve_trait_sets(&mut self, pending: &[(ModuleId, PendingItem<'u>)]) {
        let sets: Vec<(ModuleId, TraitSetId, &ast::TraitSetDecl)> = pending
            .iter()
            .filter_map(|(m, it)| match it {
                PendingItem::TraitSet(id, t) => Some((*m, *id, *t)),
                _ => None,
            })
            .collect();
        if sets.is_empty() {
            return;
        }
        let index: HashMap<TraitSetId, usize> =
            sets.iter().enumerate().map(|(i, (_, id, _))| (*id, i)).collect();
        let deps: Vec<Vec<usize>> = sets
            .iter()
            .map(|(m, _, t)| {
                t.traits
                    .iter()
                    .filter_map(|b| match &b.kind {
                        ast::TypeExprKind::Path(path) => {
                            match resolve::resolve_value_item(&self.p, *m, path) {
                                Some(Res::TraitSet(s)) => index.get(&s).copied(),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        let mut order = Vec::new();
        let mut bad = vec![false; sets.len()];
        dfs_cycles(
            &deps,
            |&d| d,
            |v| match v {
                Visit::Cycle(cycle) => {
                    let (_, _, t) = sets[cycle[0].0];
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0418,
                            t.name.span,
                            format!("the trait set `{}` names itself", t.name.name),
                        )
                        .with_help("a trait set names traits, and other sets that don't name it"),
                    );
                    for &(i, _) in cycle.iter() {
                        bad[i] = true;
                    }
                }
                Visit::Done(i) => order.push(i),
            },
        );
        for i in order {
            if bad[i] {
                continue;
            }
            let (m, id, t) = sets[i];
            let mut scope = Scope::new(m);
            scope.push_params(&self.p, &self.p.trait_sets[id.index()].generics);
            let mut traits: Vec<TraitRef> = Vec::new();
            for b in &t.traits {
                for r in resolve::resolve_trait_refs(&self.p, self.diags, &scope, b) {
                    if !traits.contains(&r) {
                        traits.push(r);
                    }
                }
            }
            self.implied_clone(&scope, &t.traits);
            self.p.trait_sets[id.index()].traits = traits;
        }
    }

    /// E0222: `@testing` marks an export, a `pub fn` of `main.wrela` (§9), which only a test
    /// build has.
    fn testing_export(&mut self, id: FnId) {
        let def = self.p.func(id);
        let Some(at) = def.attrs.testing else { return };
        // A dependency's own program's test exports (in its `main.wrela`) are about that program,
        // checked when it's the package checked: here they're functions no test build exports
        // (a tool's program may depend on another program).
        let module = self.p.module(def.module);
        if self.p.package_of(def.module).kind != PackageKind::Program
            && module.path.last().is_some_and(|m| m == "main")
        {
            return;
        }
        let export = Some(def.module) == self.p.main
            && def.public
            && def.owner == FnOwner::Free
            && def.attrs.entry.is_none();
        if !export {
            self.diags.push(
                Diagnostic::new(
                    codes::E0222,
                    at,
                    "`@testing` marks an export, a `pub fn` of `main.wrela`, which only a test build has",
                ).with_help("remove `@testing`, or put the function in `main.wrela` and make it `pub fn`")
                .with_note("what only a test export calls is left out of a shipped build with it; elsewhere, `std::mem::test_build()` asks"),
            );
        }
    }

    /// E0222: a `@test` is a free function of no parameters and no result, run on the CPU when
    /// `wrela test` asks (§13).
    fn test_signature(&mut self, id: FnId) {
        let def = self.p.func(id);
        let Some(at) = def.attrs.test else { return };
        let frames = def.attrs.test_run.is_some();
        let program = self.p.package_of(def.module).kind == PackageKind::Program;
        // A dependency's frame test is about its own program, checked when it's the package
        // checked; here its frames aren't this program's. So is a tool test.
        if (frames || def.attrs.test_tool.is_some()) && !program {
            return;
        }
        if def.attrs.test_tool.is_some() {
            return self.tool_test_signature(id, at);
        }
        // The program's state: what `init`, in main.wrela, makes.
        let state = self.p.main.and_then(|main| {
            self.p
                .fns
                .iter()
                .find(|f| f.module == main && f.public && f.name == "init" && f.params.is_empty())
        });
        let takes_state = |ps: &ParamSig| {
            state.is_some_and(|s| s.ret == ps.ty) && ps.mode == Mode::Borrow && !ps.is_self
        };
        let why = if def.owner != FnOwner::Free {
            Some("it's a method, and a test is a function of its own")
        } else if frames && !def.params.is_empty() && state.is_none() {
            Some("it takes a parameter, and the program has no state to pass it (`init` makes one)")
        } else if frames && (def.params.len() > 1 || !def.params.iter().all(takes_state)) {
            Some("a frame or tick test takes nothing, or the program's state, borrowed")
        } else if !frames && !def.params.is_empty() {
            Some("it takes parameters, and nothing passes them")
        } else if def.ret != self.p.types.unit {
            Some("it returns a value, and nothing reads it: check what it computes with `assert`")
        } else if !def.generics.is_empty() {
            Some("it's generic, and nothing chooses its types")
        } else if def.attrs.entry.is_some() || def.attrs.gpu.is_some() || def.attrs.audio.is_some()
        {
            Some("a test runs on the CPU, when `wrela test` asks")
        } else {
            None
        };
        if let Some(why) = why {
            self.diags.push(
                Diagnostic::new(
                    codes::E0222,
                    def.sig_span,
                    format!("`{}` can't be a `@test`: {why}", def.name),
                )
                .with_secondary(at, "declared a test here")
                .with_note("a test is `@test fn name() { ... }`, or `@test(frames: n) fn name(state: State) { ... }` to run the program's frames first (`ticks: n`, its ticks): it passes unless it panics, and a failed `assert` panics")
                .with_help("make it a free function of no parameters and no result, and move the values it needs inside it"),
            );
        }
    }

    /// E0222: a tool test is a free function of no parameters and no result, of a package with
    /// a tool build (main.wrela exports `tool`, §15.5).
    fn tool_test_signature(&mut self, id: FnId, at: Span) {
        let def = self.p.func(id);
        let tool = self.p.main.is_some_and(|main| {
            self.p.fns.iter().any(|f| {
                f.module == main && f.public && f.owner == FnOwner::Free && f.name == "tool"
            })
        });
        let why = if def.owner != FnOwner::Free {
            Some("it's a method, and a test is a function of its own")
        } else if !def.params.is_empty() {
            Some("it takes parameters, and nothing passes them")
        } else if def.ret != self.p.types.unit {
            Some("it returns a value, and nothing reads it: check what it computes with `assert`")
        } else if !def.generics.is_empty() {
            Some("it's generic, and nothing chooses its types")
        } else if !tool {
            Some("its package has no tool build: main.wrela exports no `tool` (§15.5)")
        } else {
            None
        };
        if let Some(why) = why {
            self.diags.push(
                Diagnostic::new(
                    codes::E0222,
                    def.sig_span,
                    format!("`{}` can't be a tool test: {why}", def.name),
                )
                .with_secondary(at, "declared a tool test here")
                .with_note("a tool test is `@test(tool) fn name() { ... }`, run on the package's tool build: it passes if it returns, and fails on a panic, a trap or its budget (§13.2)"),
            );
        }
    }

    /// E0222: `@test(..., input: NAME)` names a constant from the test's module. Its type, a
    /// `std::input::Script`, is checked once constants' types are known
    /// ([`check_test_inputs`]).
    fn test_input(&mut self, id: FnId) {
        let def = self.p.func(id);
        let Some((TestInput::Named(path), span)) = &def.attrs.test_input else { return };
        let (span, module) = (*span, def.module);
        let r = resolve::resolve_value_item(&self.p, module, path);
        let input = match r {
            Some(Res::Const(c)) if !self.p.const_(c).default => Some((TestInput::Const(c), span)),
            _ => {
                let name = self.p.text(span).unwrap_or("it").to_string();
                let what = r.map_or("nothing".to_string(), |r| resolve::describe_res(&self.p, r));
                self.diags.push(
                    Diagnostic::new(
                        codes::E0222,
                        span,
                        format!("a test's input `{name}` names {what}, not a constant"),
                    ).with_help("name a `std::input::Script` constant, or give the path of a file in the package as a string")
                    .with_note(INPUT_NOTE),
                );
                None
            }
        };
        self.p.fns[id.index()].attrs.test_input = input;
    }

    /// W0004: `Clone` written beside `Copy`, which implies it (§3).
    fn implied_clone(&mut self, scope: &Scope, written: &[ast::TypeExpr]) {
        let p = &self.p;
        let is = |b: &ast::TypeExpr, l: Lang| -> bool {
            let ast::TypeExprKind::Path(path) = &b.kind else { return false };
            match resolve::resolve_value_item(p, scope.module, path) {
                Some(Res::Trait(t)) => p.trait_(t).lang == Some(l),
                Some(Res::TraitSet(s)) => {
                    l == Lang::Copy
                        && p.trait_sets[s.index()]
                            .traits
                            .iter()
                            .any(|r| p.trait_(r.trait_).lang == Some(l))
                }
                _ => false,
            }
        };
        if !written.iter().any(|b| is(b, Lang::Copy)) {
            return;
        }
        let Some(k) = written.iter().position(|b| is(b, Lang::Clone)) else { return };
        let span = written[k].span;
        // ` + Clone` after another trait, or `Clone + ` before one.
        let cut = if k > 0 {
            Span::new(span.file, written[k - 1].span.end, span.end)
        } else {
            Span::new(span.file, span.start, written[1].span.start)
        };
        self.diags.push(
            Diagnostic::new(
                codes::W0004,
                span,
                "`Copy` implies `Clone`, so it needn't be declared",
            )
            .with_note("a type that declares `Copy` is `Clone` too (§3)")
            .with_fix("remove `Clone`", cut, ""),
        );
    }

    /// What's in scope in a struct's or an enum's declaration: its parameters. Its fields and
    /// variants also see `Self`.
    fn adt_scope(&self, m: ModuleId, id: AdtId) -> Scope {
        let mut scope = Scope::new(m);
        scope.push_params(&self.p, &self.p.adt(id).generics);
        scope
    }

    fn bounds_of(&mut self, scope: &Scope, bounds: &[ast::TypeExpr]) -> Vec<TraitRef> {
        bounds
            .iter()
            .flat_map(|b| resolve::resolve_trait_refs(&self.p, self.diags, scope, b))
            .collect()
    }

    fn set_param_bounds(
        &mut self,
        scope: &Scope,
        params: &[ParamId],
        generics: &[ast::GenericParam],
    ) {
        for (p, g) in params.iter().zip(generics) {
            // A function type bounds the parameter's values; the rest are traits.
            let (fns, traits): (Vec<ast::TypeExpr>, Vec<ast::TypeExpr>) =
                g.bounds.iter().cloned().partition(|b| matches!(b.kind, ast::TypeExprKind::Fn(_)));
            let bounds = self.bounds_of(scope, &traits);
            self.p.params[p.index()].bounds = bounds;
            if let Some(f) = fns.first() {
                let t = resolve::resolve_type(&self.p, self.diags, scope, f, TyPos::FnBound);
                self.p.params[p.index()].fn_bound = Some(t);
            }
            if let Some(extra) = fns.get(1) {
                self.diags.push(Diagnostic::new(
                    codes::E0510,
                    extra.span,
                    "a parameter takes one function type: its values are called as that type",
                ));
            }
        }
    }

    /// Resolves a trait's own parameters' bounds, its supertraits and its associated types;
    /// returns each supertrait it names, with where.
    fn trait_header(
        &mut self,
        m: ModuleId,
        id: TraitId,
        t: &ast::TraitDecl,
    ) -> Vec<(TraitId, Span)> {
        let self_param = self.p.trait_(id).self_param;
        let gens = self.p.trait_(id).generics.clone();
        let scope = Scope::of_owner(&self.p, m, FnOwner::Trait(id));
        self.set_param_bounds(&scope, &gens, &t.generics);
        let mut supers = Vec::new();
        let mut edges = Vec::new();
        for b in &t.supertraits {
            for r in resolve::resolve_trait_refs(&self.p, self.diags, &scope, b) {
                edges.push((r.trait_, b.span));
                supers.push(r);
            }
        }
        self.p.traits[id.index()].supertraits = supers.clone();
        // `Self` is bounded by the trait itself and its supertraits.
        let own =
            TraitRef { trait_: id, args: gens.iter().map(|&g| self.p.types.param(g)).collect() };
        let mut self_bounds = vec![own];
        self_bounds.extend(supers);
        // A field a walk visits has the trait, as `Self` does (`trait.fieldwise-walk`).
        if let Some(g) = self.p.trait_(id).field_param {
            self.p.params[g.index()].bounds = self_bounds.clone();
        }
        self.p.params[self_param.index()].bounds = self_bounds;
        let mut assoc = Vec::new();
        for member in &t.members {
            if let ast::TraitMemberKind::Type { name, bounds } = &member.kind {
                if assoc.iter().any(|a: &AssocTypeDef| a.name == name.name) {
                    self.diags.push(Diagnostic::new(
                        codes::E0201,
                        name.span,
                        format!("`{}` is declared twice in this trait", name.name),
                    ));
                    continue;
                }
                let bounds = self.bounds_of(&scope, bounds);
                assoc.push(AssocTypeDef { name: name.name.clone(), bounds, span: name.span });
            }
        }
        self.p.traits[id.index()].assoc_types = assoc;
        edges
    }

    /// Reports each cycle of supertraits once (E0411), at the bound that closes it, and cuts it:
    /// what follows walks supertraits transitively, and would loop.
    fn supertrait_cycles(&mut self, edges: &[(TraitId, TraitId, Span)]) {
        let mut out: Vec<Vec<(TraitId, Span)>> = vec![Vec::new(); self.p.traits.len()];
        for &(from, to, span) in edges {
            out[from.index()].push((to, span));
        }
        let p = &self.p;
        let mut cut: Vec<(TraitId, TraitId)> = Vec::new();
        dfs_cycles(
            &out,
            |&(to, _)| to.index(),
            |v| {
                let Visit::Cycle(cycle) = v else { return };
                let (from, &(to, span)) = cycle[cycle.len() - 1];
                let from = TraitId(from as u32);
                cut.push((from, to));
                let name = &p.trait_(to).name;
                let mut d = if to == from {
                    Diagnostic::new(
                        codes::E0411,
                        span,
                        format!("`{name}` can't be its own supertrait"),
                    ).with_help("remove it from its own supertraits; a trait's supertraits are other traits")
                } else {
                    let through: Vec<String> = cycle[1..]
                        .iter()
                        .map(|&(t, _)| format!("`{}`", p.traits[t].name))
                        .collect();
                    Diagnostic::new(
                        codes::E0411,
                        span,
                        format!("`{name}` is its own supertrait, through {}", and_list(&through)),
                    )
                    .with_help("remove one of the supertraits that leads back to it")
                };
                for &(_, &(_, s)) in &cycle[..cycle.len() - 1] {
                    d = d.with_secondary(s, "a supertrait in the cycle");
                }
                self.diags.push(d);
            },
        );
        for (from, to) in cut {
            let t = &mut self.p.traits[from.index()];
            t.supertraits.retain(|r| r.trait_ != to);
            let sp = t.self_param;
            self.p.params[sp.index()].bounds.retain(|r| r.trait_ != to || to == from);
        }
    }

    fn adt_bounds(&mut self, m: ModuleId, id: AdtId, item: &ast::Item) {
        let gens = self.p.adt(id).generics.clone();
        let scope = self.adt_scope(m, id);
        let ast_generics = match &item.kind {
            ast::ItemKind::Struct(s) => &s.generics,
            ast::ItemKind::Enum(e) => &e.generics,
            _ => return,
        };
        self.set_param_bounds(&scope, &gens, ast_generics);
        // A function field's type bounds its parameter: what the field holds is called as it.
        if let ast::ItemKind::Struct(s) = &item.kind {
            for (name, g) in self.p.adt(id).fn_fields.clone() {
                let Some(f) = s.fields.iter().find(|f| f.name.name == name) else { continue };
                let t = resolve::resolve_type(&self.p, self.diags, &scope, &f.ty, TyPos::FnBound);
                self.p.params[g.index()].fn_bound = Some(t);
            }
        }
    }

    fn adt_body(&mut self, m: ModuleId, id: AdtId, item: &ast::Item) {
        let mut scope = self.adt_scope(m, id);
        let self_args: Vec<TyId> =
            self.p.adt(id).generics.iter().map(|&g| self.p.types.param(g)).collect();
        scope.self_ty = Some(self.p.types.adt(id, self_args));
        let (traits, kind) = match &item.kind {
            ast::ItemKind::Struct(s) => {
                let fields = self.field_defs(&scope, &s.fields, true);
                (&s.traits, AdtKind::Struct(fields))
            }
            ast::ItemKind::Enum(e) => {
                let mut variants: Vec<VariantDef> = Vec::new();
                let mut next = Some(0u32);
                // An enum with discriminants is an enum of fieldless variants (§3).
                let written = e.variants.iter().find_map(|v| v.discriminant.as_ref());
                if let Some(d) = written
                    && let Some(v) = e.variants.iter().find(|v| v.kind != ast::VariantKind::Unit)
                {
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0334,
                            d.span,
                            format!("`{}` holds fields, so its enum's variants have no discriminants", v.name.name),
                        )
.with_help("remove the discriminants, or make the variants hold nothing")
                        .with_secondary(v.span, "holds fields")
                        .with_note("a discriminant is a variant's tag and its `u32`: an enum whose variants hold nothing has them (§3)"),
                    );
                }
                for v in &e.variants {
                    let discriminant = match &v.discriminant {
                        Some(lit) => match lit.kind {
                            ast::LitKind::Int(ast::IntValue::Ok(x)) if x <= u64::from(u32::MAX) => {
                                Some(x as u32)
                            }
                            _ => {
                                self.diags.push(Diagnostic::new(
                                    codes::E0334,
                                    lit.span,
                                    format!(
                                        "`{}`'s discriminant is a `u32`, and `{}` isn't one",
                                        v.name.name, lit.text
                                    ),
                                )
.with_help("write the discriminant as a whole number from 0 to 4294967295"));
                                None
                            }
                        },
                        None => next,
                    };
                    let discriminant = match discriminant {
                        Some(d) => d,
                        None => {
                            if v.discriminant.is_none() {
                                self.diags.push(Diagnostic::new(
                                    codes::E0334,
                                    v.name.span,
                                    format!("`{}`'s discriminant, one past the variant's before it, is past `u32`'s range", v.name.name),
                                )
.with_help("give the variants before it smaller discriminants"));
                            }
                            0
                        }
                    };
                    next = discriminant.checked_add(1);
                    // Discriminants increase, so the variants compare as they're declared.
                    if let (Some(lit), Some(prev)) = (&v.discriminant, variants.last())
                        && discriminant <= prev.discriminant
                    {
                        let (ps, pd) = (prev.span, prev.discriminant);
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0334,
                                lit.span,
                                format!(
                                    "`{}`'s discriminant, {discriminant}, isn't past `{}`'s, {pd}",
                                    v.name.name, prev.name
                                ),
                            )
.with_help(format!("give `{}` a discriminant larger than {pd}, or reorder the variants", v.name.name))
                            .with_secondary(ps, "the variant before it")
                            .with_note("discriminants increase down the enum, so its variants are ordered as they're declared, and each has its own (§3)"),
                        );
                    }
                    if let Some(prev) = variants.iter().find(|p| p.name == v.name.name) {
                        let ps = prev.span;
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0201,
                                v.name.span,
                                format!("the variant `{}` is declared twice", v.name.name),
                            )
                            .with_secondary(ps, "first declared here"),
                        );
                        continue;
                    }
                    let (shape, fields) = match &v.kind {
                        ast::VariantKind::Unit => (VariantShape::Unit, Vec::new()),
                        ast::VariantKind::Tuple(tys) => (
                            VariantShape::Tuple,
                            tys.iter()
                                .enumerate()
                                .map(|(i, t)| FieldDef {
                                    name: i.to_string(),
                                    ty: resolve::resolve_type(
                                        &self.p,
                                        self.diags,
                                        &scope,
                                        t,
                                        TyPos::Normal,
                                    ),
                                    public: true,
                                    package_only: false,
                                    default: None,
                                    span: t.span,
                                    mode: RetMode::Owned,
                                })
                                .collect(),
                        ),
                        ast::VariantKind::Struct(fs) => {
                            (VariantShape::Struct, self.field_defs(&scope, fs, false))
                        }
                    };
                    variants.push(VariantDef {
                        name: v.name.name.clone(),
                        shape,
                        fields,
                        span: v.span,
                        discriminant,
                    });
                }
                (&e.traits, AdtKind::Enum(variants))
            }
            _ => return,
        };
        self.p.adts[id.index()].kind = kind;
        let mut opt_in: Vec<(TraitRef, Span)> = Vec::new();
        for t in traits {
            for r in resolve::resolve_trait_refs(&self.p, self.diags, &scope, t) {
                if !opt_in.iter().any(|(o, _)| *o == r) {
                    opt_in.push((r, t.span));
                }
            }
        }
        self.implied_clone(&scope, traits);
        self.p.adts[id.index()].opt_in = opt_in;
    }

    /// A parameter's or field's default (§3): a literal value stays as written; any other
    /// constant expression becomes a constant the build computes once (§13), which the default
    /// names. A default whose type is generic stays as written, and must be a literal (E0324):
    /// a constant has one type.
    fn default_value(&mut self, m: ModuleId, of: &str, d: &ast::Expr, ty: TyId) -> ast::Expr {
        let literal = match &d.kind {
            ast::ExprKind::Lit(_) | ast::ExprKind::Path(_) => true,
            ast::ExprKind::Unary(ast::UnOp::Neg, x) => matches!(x.kind, ast::ExprKind::Lit(_)),
            _ => false,
        };
        if literal || self.p.types.has_params(ty) {
            return d.clone();
        }
        let id = ConstId(self.p.consts.len() as u32);
        // `·` can't be in a name, so this one can't clash with an item's.
        let name = ast::Ident { name: format!("default·{of}·{}", id.index()), span: d.span };
        let eval = self.const_eval_fn(id, of, m, d.span, d.span);
        self.p.consts.push(ConstDef {
            name: of.to_string(),
            module: m,
            ty: Some(ty),
            annotation: None,
            value: d.clone(),
            public: false,
            span: d.span,
            eval,
            default: true,
            fuel: None,
        });
        self.bind(m, &name, Res::Const(id), Vis::PRIVATE);
        let segment = ast::PathSegment { ident: name, generics: None };
        ast::Expr {
            kind: ast::ExprKind::Path(ast::Path { segments: vec![segment], span: d.span }),
            span: d.span,
        }
    }

    fn field_defs(
        &mut self,
        scope: &Scope,
        fields: &[ast::FieldDecl],
        allow_default: bool,
    ) -> Vec<FieldDef> {
        let mut out: Vec<FieldDef> = Vec::new();
        for f in fields {
            if let Some(prev) = out.iter().find(|p| p.name == f.name.name) {
                let ps = prev.span;
                self.diags.push(
                    Diagnostic::new(
                        codes::E0201,
                        f.name.span,
                        format!("the field `{}` is declared twice", f.name.name),
                    )
                    .with_secondary(ps, "first declared here"),
                );
                continue;
            }
            // A borrow struct's field may be a run or a projection (§6.6).
            let borrow = scope.self_ty.is_some_and(|t| self.p.is_borrow_struct(t));
            let pos = if borrow { TyPos::Local } else { TyPos::Normal };
            // A function field holds a value of its own parameter (`fn.fields`).
            let fn_param = match scope.self_ty.map(|t| self.p.types.kind(t)) {
                Some(TyKind::Adt(a, _)) => self
                    .p
                    .adt(*a)
                    .fn_fields
                    .iter()
                    .find(|(n, _)| *n == f.name.name)
                    .map(|&(_, g)| g),
                _ => None,
            };
            let ty = match fn_param {
                Some(g) if matches!(f.ty.kind, ast::TypeExprKind::Fn(_)) => self.p.types.param(g),
                _ => resolve::resolve_type(&self.p, self.diags, scope, &f.ty, pos),
            };
            if f.mode != RetMode::Owned && !borrow {
                let mode = if f.mode == RetMode::Mut { "mut" } else { "borrow" };
                self.diags.push(
                    Diagnostic::new(
                        codes::E0519,
                        f.ty.span,
                        format!("`{mode} {}` is a projection, so it can't be a field of an ordinary type", self.p.display_ty(ty)),
                    )
                    .with_note("a projection lives only as long as the call that made it (§6.6)")
                    .with_help("store a handle (`Handle<T>`) or an owned copy, make the type a `borrow struct`, or pass a closure that does the work where the place is"),
                );
            }
            let default = match &f.default {
                Some(d) if allow_default => {
                    Some(self.default_value(scope.module, &f.name.name, d, ty))
                }
                _ => None,
            };
            if !allow_default && let Some(d) = &f.default {
                self.diags.push(Diagnostic::new(
                    codes::E0324,
                    d.span,
                    "variant fields can't have defaults",
                )
.with_help("remove the default: a variant's fields are given wherever the variant is built"));
            }
            out.push(FieldDef {
                name: f.name.name.clone(),
                ty,
                public: f.vis.is_some(),
                package_only: f.vis.is_some_and(|v| v.package),
                default,
                span: f.span,
                mode: f.mode,
            });
        }
        out
    }

    fn impl_header(&mut self, m: ModuleId, id: ImplId, i: &ast::ImplDecl) {
        let gens = self.p.impl_(id).generics.clone();
        let mut scope = Scope::new(m);
        scope.push_params(&self.p, &gens);
        self.set_param_bounds(&scope, &gens, &i.generics);
        // `impl str`: a run's methods take it as `self`, a parameter.
        let self_ty = resolve::resolve_type(&self.p, self.diags, &scope, &i.self_ty, TyPos::Local);
        self.p.impls[id.index()].self_ty = self_ty;
        scope.self_ty = Some(self_ty);
        scope.impl_ = Some(id);
        if let Some(t) = &i.trait_ {
            let mut found = Vec::new();
            let r = resolve::resolve_trait_ref(&self.p, &mut found, &scope, t);
            // `impl Drop`, written as in Rust, with no `Drop` in scope: the habit, not the name.
            let drop = matches!(&t.kind, ast::TypeExprKind::Path(p)
                if p.segments.len() == 1 && p.segments[0].ident.name == "Drop");
            if r.is_none() && drop {
                self.diags.push(
                    Diagnostic::new(codes::E0413, t.span, "wrela has no destructors for a program's types")
                        .with_note("a value's fields are dropped when it is; only std's core defines destructors (§21)")
                        .with_help("to run code when something ends, call it where it ends"),
                );
            } else {
                self.diags.extend(found);
            }
            self.p.impls[id.index()].trait_ref = r;
        }
        let mut assoc = BTreeMap::new();
        for member in &i.members {
            if let ast::ImplMemberKind::Type { name, ty } = &member.kind {
                let t = resolve::resolve_type(&self.p, self.diags, &scope, ty, TyPos::Normal);
                if assoc.insert(name.name.clone(), t).is_some() {
                    self.diags.push(Diagnostic::new(
                        codes::E0201,
                        name.span,
                        format!("`{}` is defined twice in this impl", name.name),
                    ));
                }
            }
        }
        self.p.impls[id.index()].assoc_types = assoc;
    }

    fn fn_bounds(&mut self, id: FnId, f: &ast::FnDecl) {
        let scope = Scope::of_fn(&self.p, id);
        let gens = self.p.func(id).generics.clone();
        self.set_param_bounds(&scope, &gens, &f.generics);
    }

    fn fn_signature(&mut self, id: FnId, f: &ast::FnDecl) {
        let scope = Scope::of_fn(&self.p, id);
        let mut implicit = resolve::ImplicitParams::new(&self.p);
        let mut params: Vec<ParamSig> = Vec::new();
        let owner = self.p.func(id).owner;
        for (i, p) in f.params.iter().enumerate() {
            match p {
                ast::Param::SelfParam { mode, span } => {
                    if owner == FnOwner::Free || i != 0 {
                        self.diags.push(Diagnostic::new(
                            codes::E0100,
                            *span,
                            if owner == FnOwner::Free {
                                "only methods in an `impl` or `trait` take `self`"
                            } else {
                                "`self` must be the first parameter"
                            },
                        ));
                        continue;
                    }
                    let ty = scope.self_ty.unwrap_or(self.p.types.error);
                    params.push(ParamSig {
                        name: "self".into(),
                        mode: *mode,
                        ty,
                        default: None,
                        span: *span,
                        is_self: true,
                    });
                }
                ast::Param::Named { name, mode, ty, default, span } => {
                    if let Some(prev) = params.iter().find(|q| q.name == name.name) {
                        let ps = prev.span;
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0201,
                                name.span,
                                format!("the parameter `{}` is declared twice", name.name),
                            )
                            .with_secondary(ps, "first declared here"),
                        );
                        continue;
                    }
                    // A `take` parameter of a function type is kept, so it's a generic
                    // parameter's value, as a struct's function field is (`fn.fields`).
                    let t = if *mode == Mode::Take && matches!(ty.kind, ast::TypeExprKind::Fn(_)) {
                        let bound =
                            resolve::resolve_type(&self.p, self.diags, &scope, ty, TyPos::FnBound);
                        let param = ParamDef::new(format!("take {}", name.name), ty.span);
                        let g = implicit.add_def(ParamDef { fn_bound: Some(bound), ..param });
                        self.p.types.param(g)
                    } else {
                        resolve::resolve_type(
                            &self.p,
                            self.diags,
                            &scope,
                            ty,
                            TyPos::Param(&mut implicit),
                        )
                    };
                    let default = default
                        .as_ref()
                        .map(|d| self.default_value(scope.module, &name.name, d, t));
                    params.push(ParamSig {
                        name: name.name.clone(),
                        mode: *mode,
                        ty: t,
                        default,
                        span: *span,
                        is_self: false,
                    });
                }
            }
        }
        // The parameters `x: Trait` introduced: added in order, they get the ids
        // `ImplicitParams` promised.
        let implicit: Vec<ParamId> =
            implicit.defs.into_iter().map(|d| self.p.new_param(d)).collect();
        let mut opaque = None;
        let (ret, ret_mode) = match &f.ret {
            Some(r) => {
                let t = resolve::resolve_type(
                    &self.p,
                    self.diags,
                    &scope,
                    &r.ty,
                    TyPos::Return(&mut opaque),
                );
                // A run is a projection: returning one borrows the arguments (§6.6).
                let run = self.p.types.is_run(t);
                (t, if run && r.mode == RetMode::Owned { RetMode::Borrow } else { r.mode })
            }
            None => (self.p.types.unit, RetMode::Owned),
        };
        // The function that defines an alias naming traits returns that alias's hidden type: its
        // body decides it, as a trait in return position's does. It isn't generic: the alias
        // names one type.
        if opaque.is_none()
            && let Some((alias, bounds)) = self.alias_fns.get(&id).cloned()
        {
            if !self.p.fns[id.index()].generics.is_empty() || !implicit.is_empty() {
                let a = &self.p.aliases[alias.index()];
                self.diags.push(
                    Diagnostic::new(
                        codes::E0410,
                        f.name.span,
                        format!("`{}` decides what `{}` names, so it can't be generic", f.name.name, a.name),
                    ).with_help("remove its generic parameters: what it names is the same for every use")
                    .with_note("an alias that names traits names one type: the first function in its module that returns it decides it (§4)"),
                );
            } else {
                opaque = Some(bounds);
            }
        }
        let ret = if opaque.is_some() {
            // The return type names traits: it's this function's hidden concrete type.
            let all = self.p.fn_all_generics(id);
            let args = all.iter().chain(&implicit).map(|&g| self.p.types.param(g)).collect();
            self.p.types.intern(TyKind::Opaque(id, args))
        } else {
            ret
        };
        let def = &mut self.p.fns[id.index()];
        def.generics.extend(implicit);
        def.params = params;
        def.ret = ret;
        def.ret_mode = ret_mode;
        def.opaque = opaque;
    }

    // ---- impls -----------------------------------------------------------------------------

    /// A struct or enum that holds itself, directly or through other types, would be infinitely
    /// large: there's no indirection in tier 0. Each cycle is reported once (E0318), and the
    /// field that closes it gets the error type, so nothing later walks it forever.
    fn check_recursive_types(&mut self) {
        /// What a type holds by value.
        enum Held {
            Adt(usize),
            Param(ParamId),
        }
        // What a type holds by value, given which generic parameters each ADT holds.
        fn held(p: &Program, holds: &[Vec<bool>], t: TyId, out: &mut Vec<Held>) {
            held_in(p, holds, t, out, 0)
        }
        fn held_in(p: &Program, holds: &[Vec<bool>], t: TyId, out: &mut Vec<Held>, depth: u32) {
            match p.types.kind(t) {
                TyKind::Adt(b, args) => {
                    out.push(Held::Adt(b.index()));
                    for (i, &arg) in args.iter().enumerate() {
                        if holds[b.index()].get(i).copied().unwrap_or(true) {
                            held_in(p, holds, arg, out, depth);
                        }
                    }
                    // A field typed by a projection holds what the projection is for these
                    // arguments: `k: T::K` with `T = S` and `type K = S` holds an `S`. The
                    // depth bounds impls whose associated types grow.
                    let adt = p.adt(*b);
                    if args.is_empty() || depth >= MAX_PROJECTION_DEPTH {
                        return;
                    }
                    let subst = Subst::from_pairs(&adt.generics, args);
                    for f in adt.all_fields() {
                        if p.types.has_projections(f.ty) {
                            let ty = crate::traits::normalize(p, p.types.subst(f.ty, &subst), None);
                            held_in(p, holds, ty, out, depth + 1);
                        }
                    }
                }
                TyKind::Param(q) => out.push(Held::Param(*q)),
                TyKind::Tuple(ts) => ts.iter().for_each(|&x| held_in(p, holds, x, out, depth)),
                TyKind::Array(e, _) | TyKind::ArrayN(e, _) => held_in(p, holds, *e, out, depth),
                _ => {}
            }
        }
        const MAX_PROJECTION_DEPTH: u32 = 8;
        let p = &self.p;
        let mut out = Vec::new();
        // Which of its generic parameters each ADT holds by value: a fixpoint.
        let mut holds: Vec<Vec<bool>> =
            p.adts.iter().map(|a| vec![false; a.generics.len()]).collect();
        loop {
            let mut changed = false;
            for (a, adt) in p.adts.iter().enumerate() {
                for f in adt.all_fields() {
                    out.clear();
                    held(p, &holds, f.ty, &mut out);
                    for k in &out {
                        if let Held::Param(q) = *k
                            && let Some(i) = adt.generics.iter().position(|&g| g == q)
                            && !holds[a][i]
                        {
                            holds[a][i] = true;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        // Edges: the ADTs each field holds, with the field's index and span.
        let mut edges: Vec<Vec<(usize, usize, Span)>> = Vec::new();
        for adt in &p.adts {
            let mut e = Vec::new();
            for (fi, f) in adt.all_fields().enumerate() {
                out.clear();
                held(p, &holds, f.ty, &mut out);
                for k in &out {
                    if let Held::Adt(b) = *k {
                        e.push((b, fi, f.span));
                    }
                }
            }
            edges.push(e);
        }
        let mut broken: Vec<(usize, usize)> = Vec::new();
        dfs_cycles(
            &edges,
            |&(b, ..)| b,
            |v| {
                let Visit::Cycle(cycle) = v else { return };
                let (a, &(_, fi, field_span)) = cycle[cycle.len() - 1];
                let adts: Vec<usize> = cycle.iter().map(|&(x, _)| x).collect();
                self.diags.push(recursive_type_error(p, &adts, a, field_span));
                broken.push((a, fi));
            },
        );
        let error = self.p.types.error;
        for (a, fi) in broken {
            if let Some(f) = self.p.adts[a].all_fields_mut().nth(fi) {
                f.ty = error;
            }
        }
        // An ADT's own fields don't show a cycle through a projection only an instance resolves
        // (`k: T::K` in `S<T>`, with `type K = S<X>` in X's impl): each associated type is
        // checked for holding itself once its projections are resolved.
        let mut cyclic = Vec::new();
        for (i, imp) in self.p.impls.iter().enumerate() {
            for (name, &ty) in &imp.assoc_types {
                let ty = crate::traits::normalize(&self.p, ty, Some(imp));
                if holds_itself(&self.p, ty, &mut Vec::new(), &mut HashSet::new()) {
                    self.diags.push(Diagnostic::new(
                        codes::E0318,
                        imp.span,
                        format!(
                            "`type {name} = {}` holds itself through its fields, so it would be infinitely large",
                            self.p.display_ty(ty)
                        ),
                    ).with_help("hold the inner values in a `Vec`, or an index into one, instead of the value itself"));
                    cyclic.push((i, name.clone()));
                }
            }
        }
        for (i, name) in cyclic {
            self.p.impls[i].assoc_types.insert(name, error);
        }
    }

    /// The methods a `@fieldwise` trait derives for impl `id`'s type (§3): each trait method
    /// but its hooks, with `Self` as the type. A method that can't be derived and has a default
    /// keeps the default.
    fn derived_methods(&mut self, id: ImplId, imp: &ImplDef) -> Vec<FnId> {
        let Some(r) = imp.trait_ref.clone() else { return Vec::new() };
        let tr = self.p.trait_(r.trait_);
        if !tr.fieldwise || tr.lang.is_some_and(Lang::is_structural) {
            return Vec::new();
        }
        let mut subst = Subst::from_pairs(&tr.generics, &r.args);
        subst.insert(tr.self_param, imp.self_ty);
        let mut out = Vec::new();
        for m in tr.methods.clone() {
            if crate::fieldwise::is_hook(&self.p, r.trait_, m)
                || crate::fieldwise::shape(&self.p, r.trait_, m).is_err()
            {
                continue;
            }
            let def = self.p.func(m);
            let params = def
                .params
                .iter()
                .map(|ps| ParamSig {
                    ty: self.p.types.subst(ps.ty, &subst),
                    default: None,
                    ..ps.clone()
                })
                .collect();
            let ret = self.p.types.subst(def.ret, &subst);
            let (name, generics, ret_mode) = (def.name.clone(), def.generics.clone(), def.ret_mode);
            self.p.fns.push(FnDef {
                name,
                module: imp.module,
                owner: FnOwner::Impl(id),
                generics,
                params,
                ret,
                ret_mode,
                opaque: None,
                attrs: FnAttrs::default(),
                body: None,
                public: true,
                package_only: false,
                span: imp.span,
                name_span: imp.span,
                sig_span: imp.span,
                lang: None,
                derived: Some(DerivedFn { method: m, trait_ref: r.clone() }),
            });
            out.push(FnId(self.p.fns.len() as u32 - 1));
        }
        out
    }

    /// A `@fieldwise` trait's impls for tuples of up to [`MAX_DERIVED_TUPLE`] elements and for
    /// arrays: derived element by element, and conditional on the elements, as an opt-in's are
    /// on the fields (`traits::implements_fieldwise`).
    fn tuple_and_array_impls(&mut self) {
        for t in 0..self.p.traits.len() {
            let tr = self.p.traits[t].clone();
            // Its supertraits must be derived for them too, or structural.
            let derivable = |r: &TraitRef| {
                let s = self.p.trait_(r.trait_);
                s.fieldwise || s.lang.is_some_and(Lang::is_structural)
            };
            if !tr.fieldwise
                || tr.lang.is_some_and(Lang::is_structural)
                || !tr.assoc_types.is_empty()
                || !tr.supertraits.iter().all(derivable)
            {
                continue;
            }
            // An impl written for arrays, or for tuples of an arity, replaces the derived one:
            // a list can say better than a walk how to find its element (an array's, by
            // dividing).
            let written = |p: &Program, arity: Option<usize>| {
                p.impls.iter().any(|imp| {
                    !imp.from_opt_in
                        && imp.trait_ref.as_ref().is_some_and(|r| r.trait_ == TraitId(t as u32))
                        && match (p.types.kind(imp.self_ty), arity) {
                            (TyKind::ArrayN(..) | TyKind::Array(..), None) => true,
                            (TyKind::Tuple(ts), Some(k)) => ts.len() == k,
                            _ => false,
                        }
                })
            };
            // `None` for the array.
            for arity in std::iter::once(None).chain((2..=MAX_DERIVED_TUPLE).map(Some)) {
                if written(&self.p, arity) {
                    continue;
                }
                let param = |p: &mut Program, name: String, is_const: bool| {
                    p.new_param(ParamDef { is_const, ..ParamDef::new(name, tr.span) })
                };
                let trait_params: Vec<ParamId> = tr
                    .generics
                    .iter()
                    .map(|&g| {
                        let d = self.p.param(g).clone();
                        param(&mut self.p, d.name, d.is_const)
                    })
                    .collect();
                let (elems, self_ty) = match arity {
                    None => {
                        let e = param(&mut self.p, "T".into(), false);
                        let n = param(&mut self.p, "N".into(), true);
                        let (et, nt) = (self.p.types.param(e), self.p.types.param(n));
                        (vec![e, n], self.p.types.intern(TyKind::ArrayN(et, nt)))
                    }
                    Some(k) => {
                        let es: Vec<ParamId> =
                            (0..k).map(|i| param(&mut self.p, format!("T{i}"), false)).collect();
                        let ts = es.iter().map(|&e| self.p.types.param(e)).collect();
                        (es, self.p.types.tuple(ts))
                    }
                };
                let args = trait_params.iter().map(|&g| self.p.types.param(g)).collect();
                let imp = ImplDef {
                    module: tr.module,
                    generics: [trait_params, elems].concat(),
                    trait_ref: Some(TraitRef { trait_: TraitId(t as u32), args }),
                    self_ty,
                    assoc_types: BTreeMap::new(),
                    methods: Vec::new(),
                    span: tr.span,
                    from_opt_in: true,
                };
                self.push_derived_impl(imp);
            }
        }
    }

    /// Adds `imp` to the program, with the methods its trait derives for its type.
    fn push_derived_impl(&mut self, imp: ImplDef) {
        let id = ImplId(self.p.impls.len() as u32);
        let methods = self.derived_methods(id, &imp);
        self.p.impls.push(ImplDef { methods, ..imp });
    }

    /// Adds the impls that opting in implies, and indexes every impl by its trait or type.
    fn index_impls(&mut self) {
        // Opting in to a trait in a declaration implies an impl of it.
        let p = &self.p;
        let mut implied = Vec::new();
        for (a, adt) in p.adts.iter().enumerate() {
            if adt.opt_in.is_empty() {
                continue;
            }
            let args = adt.generics.iter().map(|&g| p.types.param(g)).collect();
            let self_ty = p.types.adt(AdtId(a as u32), args);
            for (r, span) in &adt.opt_in {
                let tr = p.trait_(r.trait_);
                // A `@fieldwise` trait's methods are derived; a structural one has none.
                if !tr.lang.is_some_and(Lang::is_structural) && !tr.fieldwise {
                    let required: Vec<String> = tr
                        .methods
                        .iter()
                        .filter(|&&f| p.func(f).body.is_none())
                        .map(|&f| format!("`fn {}`", p.func(f).name))
                        .chain(tr.assoc_types.iter().map(|t| format!("`type {}`", t.name)))
                        .collect();
                    if !required.is_empty() {
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0401,
                                *span,
                                format!(
                                    "`{}` can't opt in to `{}`: the trait requires {}",
                                    adt.name,
                                    tr.name,
                                    required.join(", ")
                                ),
                            )
                            .with_help(format!(
                                "write `impl {} for {} {{ ... }}` instead",
                                tr.name, adt.name
                            )),
                        );
                        continue;
                    }
                }
                implied.push(ImplDef {
                    module: adt.module,
                    generics: adt.generics.clone(),
                    trait_ref: Some(r.clone()),
                    self_ty,
                    assoc_types: BTreeMap::new(),
                    methods: Vec::new(),
                    span: *span,
                    from_opt_in: true,
                });
            }
        }
        // A job's value's traits are its parts', which depend on the job: `Job<F>` declares
        // them, and each `@job fn` gets an impl of its own, derived from its parts
        // (`fn.job-values`).
        let job = self.p.lang_adt(Lang::Job);
        let mut each_job = Vec::new();
        for imp in implied {
            let of_job =
                matches!(self.p.types.kind(imp.self_ty), TyKind::Adt(a, _) if Some(*a) == job);
            if !of_job {
                self.push_derived_impl(imp);
                continue;
            }
            for f in 0..self.p.fns.len() as u32 {
                let f = FnId(f);
                if self.p.func(f).attrs.job.is_none() {
                    continue;
                }
                let generics = self.p.fn_all_generics(f);
                let args = generics.iter().map(|&g| self.p.types.param(g)).collect();
                let fn_ty = self.p.types.intern(TyKind::FnDef(f, args));
                let self_ty = self.p.types.adt(job.expect("a job"), vec![fn_ty]);
                each_job.push(ImplDef { generics, self_ty, ..imp.clone() });
            }
        }
        for imp in each_job {
            self.push_derived_impl(imp);
        }
        self.tuple_and_array_impls();
        for (i, imp) in self.p.impls.iter().enumerate() {
            let id = ImplId(i as u32);
            match &imp.trait_ref {
                Some(r) => self.p.impls_of_trait.entry(r.trait_).or_default().push(id),
                None => match *self.p.types.kind(imp.self_ty) {
                    TyKind::Adt(a, _) => self.p.inherent_impls.entry(a).or_default().push(id),
                    TyKind::Error => {}
                    // std's methods of built-in types: `impl str`.
                    _ if self.p.is_std(imp.module) => self.p.builtin_inherent.push(id),
                    _ => {
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0404,
                                imp.span,
                                format!(
                                    "an inherent impl needs a struct or enum, not `{}`",
                                    self.p.display_ty(imp.self_ty)
                                ),
                            )
                            .with_help("define a trait and implement it instead"),
                        );
                    }
                },
            }
        }
    }

    fn check_impls(&mut self) {
        for imp in &self.p.impls {
            check_impl(&self.p, self.diags, imp);
        }
        check_inherent_names(&self.p, self.diags);
        check_overlap(&self.p, self.diags);
    }
}

/// The error for a cycle of structs and enums that hold each other (E0318): `cycle` in order,
/// and `last`'s field that closes it.
fn recursive_type_error(p: &Program, cycle: &[usize], last: usize, field_span: Span) -> Diagnostic {
    let first = &p.adts[cycle[0]];
    let msg = if cycle.len() == 1 {
        format!(
            "`{}` holds a `{}` inside itself, so it would be infinitely large",
            first.name, first.name
        )
    } else {
        let names: Vec<String> = cycle.iter().map(|&a| format!("`{}`", p.adts[a].name)).collect();
        format!("{} hold each other, so they would be infinitely large", names.join(", "))
    };
    Diagnostic::new(codes::E0318, first.name_span, msg)
        .with_secondary(
            field_span,
            format!("this field of `{}` closes the cycle", p.adts[last].name),
        )
        .with_note("a value holds its fields directly; there are no pointers in tier 0")
        .with_help("hold an index into an array instead")
}

fn check_impl(p: &Program, diags: &mut Vec<Diagnostic>, imp: &ImplDef) {
    let Some(r) = &imp.trait_ref else {
        // Inherent: methods must have distinct names, and the type must be local.
        if let TyKind::Adt(a, _) = *p.types.kind(imp.self_ty) {
            let adt_mod = p.adt(a).module;
            if !p.same_package(adt_mod, imp.module) {
                diags.push(
                    Diagnostic::new(
                        codes::E0404,
                        imp.span,
                        format!(
                            "`{}` is defined in the package `{}`, so only that package can add methods to it",
                            p.adt(a).name,
                            p.package_of(adt_mod).name
                        ),
                    )
                    .with_help("define a trait with the methods and implement it for the type"),
                );
            }
        }
        report_duplicate_methods(p, diags, &imp.methods);
        for name in imp.assoc_types.keys() {
            diags.push(
                Diagnostic::new(
                    codes::E0402,
                    imp.span,
                    format!("an inherent impl can't define the associated type `{name}`"),
                ).with_help("put it in the impl of the trait that declares it, or use a `const` or a method here")
                .with_note("associated types belong to traits"),
            );
        }
        return;
    };
    let tr = p.trait_(r.trait_);
    let structural = tr.lang.is_some_and(Lang::is_structural);
    // Destructors are std's core's alone (§21).
    if tr.lang == Some(Lang::Drop) && !p.is_std(imp.module) {
        diags.push(
            Diagnostic::new(
                codes::E0413,
                imp.span,
                "only std's core defines destructors",
            )
            .with_note("wrela has no user-defined destructors: a value's fields are dropped when it is (§21)")
            .with_help("to run code when something ends, call it where it ends"),
        );
        return;
    }
    // A bound entry point's traits are its own (§18).
    if let Some(l @ (Lang::Kernel | Lang::VertexShader | Lang::FragmentShader)) = tr.lang {
        let what = match l {
            Lang::Kernel => "a kernel",
            Lang::VertexShader => "a vertex shader",
            _ => "a fragment shader",
        };
        diags.push(
            Diagnostic::new(
                codes::E0415,
                imp.span,
                format!("`{}` isn't implemented with an `impl`: {what} bound to its arguments has it", tr.name),
            ).with_help("remove the `impl`: binding the entry point (`name.bind(...)`) gives the trait")
            .with_note("`name.bind(...)` makes a value of the entry point's bound type, which has the trait (§18)"),
        );
        return;
    }
    // A pass's trait is std's passes' own (§18).
    if tr.lang == Some(Lang::RenderPass) && !p.is_std(imp.module) {
        diags.push(
            Diagnostic::new(
                codes::E0415,
                imp.span,
                format!("`{}` isn't implemented with an `impl`: std's passes have it", tr.name),
            ).with_help("remove the `impl`: std's passes have this trait already")
            .with_note("a draw draws into the pass a `begin_` function gives, and its type says the pass's targets (§18)"),
        );
        return;
    }
    // `Clone`'s leaves: std's core clones what owns memory by hand (`Vec`, `Box`), for a type
    // that declares `Clone`.
    let clone_leaf = tr.lang == Some(Lang::Clone)
        && p.is_std(imp.module)
        && matches!(p.types.kind(imp.self_ty), TyKind::Adt(a, _)
            if crate::traits::implements_builtin_declared(p, *a, Lang::Clone));
    if clone_leaf {
        check_members(p, diags, imp, tr, r);
        return;
    }
    // Builtin traits are structural: only by opting in.
    if structural && !imp.from_opt_in {
        let mut d = Diagnostic::new(
            codes::E0415,
            imp.span,
            format!("`{}` isn't implemented with an `impl`: a type declares it", tr.name),
        )
        .with_note("its implementation is the compiler's, field by field (§3)");
        // The fix: declare it where the type is declared, and drop the impl.
        if let TyKind::Adt(a, _) = *p.types.kind(imp.self_ty) {
            let adt = p.adt(a);
            let has_list = !adt.opt_in.is_empty();
            let insert =
                if has_list { format!(" + {}", tr.name) } else { format!(": {}", tr.name) };
            let at = match adt.opt_in.last() {
                Some((_, span)) => span.shrink_to_end(),
                None => adt.name_span.shrink_to_end(),
            };
            // A generic type's list goes after its `<...>`, which has no span here.
            if !has_list && !adt.generics.is_empty() {
                d = d.with_help(format!(
                    "declare it on the type: `struct {}<...>: {} {{ ... }}`, and remove this `impl`",
                    adt.name, tr.name
                ));
            } else {
                d = d.with_fix_edits(
                    format!("declare `{}` on `{}` and remove this `impl`", tr.name, adt.name),
                    vec![
                        wrela_diag::Edit { span: at, replacement: insert },
                        wrela_diag::Edit { span: imp.span, replacement: String::new() },
                    ],
                );
            }
        } else {
            d = d.with_help(format!(
                "remove the `impl`: a struct or an enum declares `{}` where it's declared (`struct S: {}`)",
                tr.name, tr.name
            ));
        }
        diags.push(d);
        return;
    }
    if structural {
        check_structural_opt_in(p, diags, imp, tr);
        return;
    }
    if imp.from_opt_in && tr.fieldwise {
        check_fieldwise_opt_in(p, diags, imp, tr);
    }
    // Orphan rule (D-071): the trait or the type must be this package's. std is a package too.
    let trait_local = p.same_package(tr.module, imp.module);
    let type_local = match *p.types.kind(imp.self_ty) {
        TyKind::Adt(a, _) => p.same_package(p.adt(a).module, imp.module),
        _ => false,
    };
    if !trait_local && !type_local {
        let type_pkg = match *p.types.kind(imp.self_ty) {
            TyKind::Adt(a, _) => format!("`{}`'s", p.package_of(p.adt(a).module).name),
            _ => "a built-in".to_string(),
        };
        diags.push(
            Diagnostic::new(
                codes::E0404,
                imp.span,
                format!(
                    "this package can't implement `{}`'s `{}` for {type_pkg} `{}`",
                    p.package_of(tr.module).name,
                    tr.name,
                    p.display_ty(imp.self_ty)
                ),
            )
            .with_note("an impl lives with its trait or with its type (D-071)")
            .with_help("wrap the type in a struct of your own, or define your own trait"),
        );
    }
    check_members(p, diags, imp, tr, r);
}

/// An impl's members against its trait's: every required item, nothing extra, matching
/// signatures, and the supertraits.
fn check_members(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    imp: &ImplDef,
    tr: &TraitDef,
    r: &TraitRef,
) {
    let trait_subst = {
        let mut s = Subst::from_pairs(&tr.generics, &r.args);
        s.insert(tr.self_param, imp.self_ty);
        s
    };
    for at in &tr.assoc_types {
        if !imp.assoc_types.contains_key(&at.name) && !imp.from_opt_in {
            diags.push(
                Diagnostic::new(
                    codes::E0401,
                    imp.span,
                    format!("this impl of `{}` is missing `type {}`", tr.name, at.name),
                )
                .with_help(format!("add `type {} = ...`", at.name)),
            );
        }
    }
    for name in imp.assoc_types.keys() {
        if !tr.assoc_types.iter().any(|a| &a.name == name) {
            diags.push(Diagnostic::new(
                codes::E0402,
                imp.span,
                format!("`{}` has no associated type `{name}`", tr.name),
            ));
        }
    }
    // Each associated type has the traits the trait declares it with.
    for at in &tr.assoc_types {
        let Some(&ty) = imp.assoc_types.get(&at.name) else { continue };
        let ty = crate::traits::normalize(p, ty, Some(imp));
        if let Some(cycle) = crate::traits::cyclic_projection(p, ty) {
            diags.push(
                Diagnostic::new(
                    codes::E0318,
                    imp.span,
                    format!(
                        "`type {}` is defined in terms of itself: `{}` never resolves to a type",
                        at.name,
                        p.display_ty(cycle)
                    ),
                )
                .with_help("give the associated type a concrete type, as in `type Out = f32`"),
            );
            continue;
        }
        for b in &at.bounds {
            let b = b.subst(&p.types, &trait_subst);
            let b = TraitRef {
                trait_: b.trait_,
                args: b.args.iter().map(|&a| crate::traits::normalize(p, a, Some(imp))).collect(),
            };
            if !crate::traits::implements(p, ty, &b) {
                diags.push(Diagnostic::new(
                    codes::E0400,
                    imp.span,
                    format!(
                        "`type {} = {}` doesn't implement `{}`, which `{}` declares it with",
                        at.name,
                        p.display_ty(ty),
                        p.display_trait_ref(&b),
                        tr.name
                    ),
                ).with_help("give it a type that implements the bound, or implement the trait for this type"));
            }
        }
    }
    // The type has the trait's supertraits too. A declared one that's derived holds where its
    // fields have it, and so does a supertrait the type declares too. A tuple's or an array's
    // derived impl holds where its elements have the trait, so they have its supertraits, which
    // are derived for it too (`tuple_and_array_impls`).
    let elementwise = imp.from_opt_in
        && matches!(*p.types.kind(imp.self_ty), TyKind::Tuple(_) | TyKind::ArrayN(..));
    for sup in &tr.supertraits {
        let sup = sup.subst(&p.types, &trait_subst);
        let declared_too = imp.from_opt_in
            && matches!(*p.types.kind(imp.self_ty), TyKind::Adt(a, _) if p.adt(a).opt_in.iter().any(|(o, _)| *o == sup));
        if !declared_too && !elementwise && !crate::traits::implements(p, imp.self_ty, &sup) {
            diags.push(
                Diagnostic::new(
                    codes::E0400,
                    imp.span,
                    format!(
                        "`{}` doesn't implement `{}`, which `{}` needs",
                        p.display_ty(imp.self_ty),
                        p.display_trait_ref(&sup),
                        tr.name
                    ),
                )
                .with_note(format!(
                    "`{}` is a supertrait of `{}`",
                    p.display_trait_ref(&sup),
                    tr.name
                ))
                .with_help(format!(
                    "implement `{}` for `{}` too",
                    p.display_trait_ref(&sup),
                    p.display_ty(imp.self_ty)
                )),
            );
        }
    }
    report_duplicate_methods(p, diags, &imp.methods);
    for &tm in &tr.methods {
        let tdef = p.func(tm);
        let found = imp.methods.iter().copied().find(|&f| p.func(f).name == tdef.name);
        // A method that walks the fields is derived for a type that declares the trait; an impl
        // written by hand gives it (`trait.fieldwise-walk`).
        let walks = tr.fieldwise && crate::fieldwise::walks_fields(p, tm);
        match found {
            None if walks && !imp.from_opt_in => {
                diags.push(
                    Diagnostic::new(
                        codes::E0401,
                        imp.span,
                        format!("this impl of `{}` is missing `fn {}`", tr.name, tdef.name),
                    )
                    .with_secondary(tdef.sig_span, "declared here")
                    .with_note("its body walks the fields of a type that declares the trait, so an impl written by hand gives its own (§3)")
                    .with_help(format!("add `fn {}` to the impl, or remove the impl and declare the trait on the type", tdef.name)),
                );
            }
            None if tdef.body.is_none() && !imp.from_opt_in => {
                diags.push(
                    Diagnostic::new(
                        codes::E0401,
                        imp.span,
                        format!("this impl of `{}` is missing `fn {}`", tr.name, tdef.name),
                    )
                    .with_secondary(tdef.sig_span, "declared here")
                    .with_help(match p.text(tdef.sig_span) {
                        Some(sig) => format!("add it to the impl: `{sig} {{ ... }}`"),
                        None => format!("add `fn {}` to the impl", tdef.name),
                    }),
                );
            }
            None => {}
            Some(f) => compare_signatures(p, diags, f, tm, &trait_subst, imp),
        }
    }
    for &f in &imp.methods {
        let def = p.func(f);
        if !tr.methods.iter().any(|&m| p.func(m).name == def.name) {
            diags.push(
                Diagnostic::new(
                    codes::E0402,
                    def.name_span,
                    format!("`{}` has no method `{}`", tr.name, def.name),
                )
                .with_help("move it to an inherent `impl` block"),
            );
        }
    }
}

/// E0201 for each method of one impl block whose name an earlier one has.
fn report_duplicate_methods(p: &Program, diags: &mut Vec<Diagnostic>, methods: &[FnId]) {
    let mut seen: Vec<(&str, Span)> = Vec::new();
    for &f in methods {
        let d = p.func(f);
        if let Some(&(_, ps)) = seen.iter().find(|(n, _)| *n == d.name) {
            diags.push(
                Diagnostic::new(
                    codes::E0201,
                    d.name_span,
                    format!("`{}` is defined twice", d.name),
                )
                .with_help("rename one of them")
                .with_secondary(ps, "first defined here"),
            );
        } else {
            seen.push((&d.name, d.name_span));
        }
    }
}

/// Two inherent impl blocks for one type can't both define a method: a call would take the
/// first. Blocks for types that can't be the same (`W<i32>` and `W<f32>`) can.
fn check_inherent_names(p: &Program, diags: &mut Vec<Diagnostic>) {
    let mut adts: Vec<&AdtId> = p.inherent_impls.keys().collect();
    adts.sort();
    for a in adts {
        let impls = &p.inherent_impls[a];
        for (i, &x) in impls.iter().enumerate() {
            for &y in &impls[i + 1..] {
                let (a, b) = (p.impl_(x), p.impl_(y));
                if !crate::traits::could_unify(p, a.self_ty, b.self_ty) {
                    continue;
                }
                for &f in &b.methods {
                    let d = p.func(f);
                    if let Some(&g) = a.methods.iter().find(|&&g| p.func(g).name == d.name) {
                        diags.push(
                            Diagnostic::new(
                                codes::E0201,
                                d.name_span,
                                format!(
                                    "`{}` is defined twice for `{}`",
                                    d.name,
                                    p.display_ty(b.self_ty)
                                ),
                            ).with_help("rename one of them, or remove one: a type has one item of each name across its impls")
                            .with_secondary(p.func(g).name_span, "first defined here"),
                        );
                    }
                }
            }
        }
    }
}

/// An impl method's signature must be the trait's, with `Self` and the trait's parameters
/// substituted.
fn compare_signatures(
    p: &Program,
    diags: &mut Vec<Diagnostic>,
    impl_fn: FnId,
    trait_fn: FnId,
    trait_subst: &Subst,
    imp: &ImplDef,
) {
    let (a, b) = (p.func(impl_fn), p.func(trait_fn));
    let mut subst = trait_subst.clone();
    let mismatch = |what: String| {
        Diagnostic::new(
            codes::E0405,
            a.sig_span,
            format!("`{}` doesn't match the trait's signature: {what}", a.name),
        )
        .with_secondary(b.sig_span, "the trait declares it here")
        .with_help(match p.text(b.sig_span) {
            Some(sig) => format!("declare it as the trait does: `{sig}`"),
            None => "declare it as the trait does".into(),
        })
    };
    if a.generics.len() != b.generics.len() {
        return diags.push(mismatch(format!(
            "it has {} generic parameters, the trait's has {}",
            a.generics.len(),
            b.generics.len()
        )));
    }
    for (&tp, &ip) in b.generics.iter().zip(&a.generics) {
        if p.param(tp).is_const != p.param(ip).is_const {
            let kind = |g| if p.param(g).is_const { "constant" } else { "type" };
            return diags.push(mismatch(format!(
                "`{}` is a {} parameter here, but a {} parameter in the trait",
                p.param(ip).name,
                kind(ip),
                kind(tp),
            )));
        }
        subst.insert(tp, p.types.param(ip));
    }
    // A generic parameter can't need more than the trait's does: callers check the trait's.
    for (&tp, &ip) in b.generics.iter().zip(&a.generics) {
        let mut allowed = Vec::new();
        for r in &p.param(tp).bounds {
            resolve::add_with_supertraits(
                p,
                p.types.param(ip),
                r.subst(&p.types, &subst),
                &mut allowed,
            );
        }
        if let Some(extra) = p.param(ip).bounds.iter().find(|r| !allowed.contains(r)) {
            return diags.push(mismatch(format!(
                "`{}` needs `{}` here, but not in the trait",
                p.param(ip).name,
                p.display_trait_ref(extra)
            )));
        }
        if let Some(needed) = p.param(ip).fn_bound {
            let accepts = p.param(tp).fn_bound.is_some_and(|allowed| {
                let allowed =
                    crate::traits::normalize(p, p.types.subst(allowed, &subst), Some(imp));
                let needed = crate::traits::normalize(p, needed, Some(imp));
                match (p.types.kind(allowed), p.types.kind(needed)) {
                    (TyKind::FnPtr(ap, ar, af), TyKind::FnPtr(np, nr, nf)) => {
                        ap == np
                            && ar == nr
                            && af.modes == nf.modes
                            && nf
                                .attrs()
                                .into_iter()
                                .zip(af.attrs())
                                .all(|((_, need), (_, allowed))| !need || allowed)
                    }
                    (TyKind::Error, _) | (_, TyKind::Error) => true,
                    _ => false,
                }
            });
            if !accepts {
                return diags.push(mismatch(format!(
                    "`{}` needs `{}` here, but the trait's bound does not require it",
                    p.param(ip).name,
                    p.display_ty(needed),
                )));
            }
        }
    }
    if a.params.len() != b.params.len() {
        return diags.push(mismatch(format!(
            "it takes {} parameters, the trait's takes {}",
            a.params.len(),
            b.params.len()
        )));
    }
    for (pa, pb) in a.params.iter().zip(&b.params) {
        if pa.is_self != pb.is_self {
            let (here, there) = if pa.is_self {
                ("takes `self`", "takes a parameter there")
            } else {
                ("takes a parameter there", "takes `self`")
            };
            return diags.push(mismatch(format!("it {here}, but the trait's {there}")));
        }
        if pa.mode != pb.mode {
            return diags.push(mismatch(format!(
                "`{}` is `{}` here but `{}` in the trait",
                pa.name,
                pa.mode.keyword(),
                pb.mode.keyword()
            )));
        }
        let expected = p.types.subst(pb.ty, &subst);
        let expected = crate::traits::normalize(p, expected, Some(imp));
        if pa.ty != expected && !pa.is_self {
            let (x, y) = (p.display_ty(pa.ty), p.display_ty(expected));
            return diags
                .push(mismatch(format!("`{}` has type `{x}`, but the trait says `{y}`", pa.name)));
        }
        // A call through the trait (or a generic) only knows the trait's defaults.
        if let Some(d) = &pa.default {
            return diags.push(
                Diagnostic::new(
                    codes::E0405,
                    d.span,
                    format!("`{}` has a default here; an impl's method takes the trait's", pa.name),
                )
                .with_secondary(b.sig_span, "the trait declares it here")
                .with_help("give the default in the trait"),
            );
        }
    }
    if a.ret_mode != b.ret_mode {
        return diags.push(mismatch("the return mode differs".into()));
    }
    match (&a.opaque, &b.opaque) {
        (None, None) => {
            let expected = p.types.subst(b.ret, &subst);
            let expected = crate::traits::normalize(p, expected, Some(imp));
            if a.ret != expected {
                let (x, y) = (p.display_ty(a.ret), p.display_ty(expected));
                diags.push(mismatch(format!("it returns `{x}`, but the trait says `{y}`")));
            }
        }
        // Both name traits: the same ones.
        (Some(x), Some(y)) => {
            let y: Vec<TraitRef> = y.iter().map(|r| r.subst(&p.types, &subst)).collect();
            if *x != y {
                let (x, y) = (p.display_ty(a.ret), p.display_ty(b.ret));
                diags.push(mismatch(format!("it returns `{x}`, but the trait says `{y}`")));
            }
        }
        _ => {
            let expected = p.types.subst(b.ret, &subst);
            let (x, y) = (p.display_ty(a.ret), p.display_ty(expected));
            diags.push(mismatch(format!("it returns `{x}`, but the trait says `{y}`")));
        }
    }
}

/// `Copy`, `Clone` and `GpuData` are implemented structurally: every field must have them too
/// (field types that use the type's parameters make the impl conditional).
fn check_structural_opt_in(p: &Program, diags: &mut Vec<Diagnostic>, imp: &ImplDef, tr: &TraitDef) {
    let TyKind::Adt(a, _) = *p.types.kind(imp.self_ty) else { return };
    let adt = p.adt(a);
    let lang = tr.lang;
    // A `Clone` std's core writes by hand checks the fields itself.
    if lang == Some(Lang::Clone) && crate::traits::explicit_clone(p, a).is_some() {
        return;
    }
    // `Packed` is checked where its fields become its word's bits (`make_packed`).
    if lang == Some(Lang::Packed) {
        return;
    }
    // `Fieldless` is an enum's whose variants hold nothing (§3).
    if lang == Some(Lang::Fieldless) {
        if !adt.is_fieldless_enum() {
            let why = match adt.variants().iter().find(|v| !v.fields.is_empty()) {
                Some(v) => format!("its variant `{}` holds fields", v.name),
                None => "it's a struct".to_string(),
            };
            diags.push(
                Diagnostic::new(
                    codes::E0407,
                    imp.span,
                    format!("`{}` can't be `Fieldless`: {why}", adt.name),
                ).with_help("`Fieldless` is declared by an enum whose variants hold nothing: remove it from this declaration, or remove the variants' fields")
                .with_note("`Fieldless` is an enum's whose variants hold nothing: each is its discriminant (§3)"),
            );
        }
        return;
    }
    // An enum is its `u32` tag and its payloads' bytes, laid out as the CPU lays it out, on
    // both targets (`ir::gpu_memory`): its payloads must be `GpuData`, as a struct's fields.
    for f in adt.all_fields() {
        if p.types.has_params(f.ty) {
            continue; // conditional on the arguments; checked where it's used
        }
        // A job's value: its parts are its locals, checked once its body is
        // (`check_job_fields`).
        if is_job(p, f.ty) {
            continue;
        }
        let ok = crate::traits::implements_builtin(p, f.ty, lang.unwrap_or(Lang::Clone));
        if !ok {
            let why = match lang {
                Some(Lang::GpuData) => {
                    "WGSL can't hold it in a buffer (64-bit and 8/16-bit types and empty arrays can't cross to the GPU)"
                }
                _ => "it isn't",
            };
            diags.push(
                Diagnostic::new(
                    codes::E0407,
                    f.span,
                    format!(
                        "`{}` can't be `{}`: the field `{}` has type `{}`, and {why}",
                        adt.name,
                        tr.name,
                        f.name,
                        p.display_ty(f.ty)
                    ),
                )
                .with_secondary(imp.span, format!("`{}` opted in here", tr.name))
                .with_help(format!(
                    "remove `{}` from `{}`, or give `{}` a type that is `{}` (`Text` rather than `String`, a handle rather than a `Vec`)",
                    tr.name, adt.name, f.name, tr.name
                )),
            );
        }
    }
}

/// Whether `t` is a job's value (`Job<f>`), whose parts are known once its body is checked.
fn is_job(p: &Program, t: TyId) -> bool {
    matches!(p.types.kind(t), TyKind::Adt(a, _) if p.is_lang_adt(*a, Lang::Job))
}

/// What a test's input is (§13.2), for its errors.
const INPUT_NOTE: &str = "a test's input is a `std::input::Script` constant (`input: SCRIPT`), or a file in the package (`input: \"path\"`): a script in runtime/abi `input`'s format, or with `ticks` a tick log";

/// The text of `e`, a string literal.
/// A frame or tick test's options, as `@test(frames: n, ...)` gives them (`test_run`).
#[derive(Default)]
struct FrameTest {
    run: Option<TestRun>,
    input: Option<(TestInput, Span)>,
    gpu: bool,
    golden: Option<Golden>,
    hold: Vec<(String, u32)>,
    both: bool,
}

fn str_lit(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Lit(l) if l.kind == ast::LitKind::Str => {
            Some(wrela_syntax::lexer::string_value(&l.text))
        }
        _ => None,
    }
}

/// Whether `arg` is `tool`, alone, which makes a `@test` a tool test (§13.2).
fn is_tool_arg(arg: &ast::Arg) -> bool {
    arg.name.is_none()
        && matches!(&arg.value.kind, ast::ExprKind::Path(p) if p.is_single() && p.segments[0].ident.name == "tool")
}

/// Whether `host` can name a host: lowercase letters, digits, `-` and `.`, with no part empty.
fn is_host(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|part| !part.is_empty())
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// E0222: each `@test(..., input: NAME)` names a constant of type `std::input::Script`, given
/// the constants' types (`tys`).
pub fn check_test_inputs(p: &Program, tys: &crate::check::ConstTypes) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for f in &p.fns {
        let Some((TestInput::Const(c), span)) = &f.attrs.test_input else { continue };
        // A constant whose type isn't known has an error of its own.
        let Some(&t) = tys.get(c) else { continue };
        if p.lang_of_ty(t) != Some(Lang::Script) {
            out.push(
                Diagnostic::new(
                    codes::E0222,
                    *span,
                    format!(
                        "a test's input {} is a `{}`, not a `std::input::Script`",
                        p.const_(*c).short(),
                        p.display_ty(t)
                    ),
                ).with_help("make the constant a `std::input::Script`, or give the path of a file in the package as a string")
                .with_note(INPUT_NOTE),
            );
        }
    }
    out
}

/// E0407 for a struct that declares `Clone` or a `@fieldwise` trait whose field is a job's value
/// without it: a part of the job (a local it holds) lacks it. Checked once the jobs' bodies
/// are, since a job's parts are its locals (`fn.job-values`).
pub fn check_job_fields(p: &Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for imp in &p.impls {
        let Some(r) = &imp.trait_ref else { continue };
        if !imp.from_opt_in {
            continue;
        }
        let tr = p.trait_(r.trait_);
        let structural = tr.lang == Some(Lang::Clone);
        if !structural && !tr.fieldwise {
            continue;
        }
        let TyKind::Adt(a, _) = *p.types.kind(imp.self_ty) else { continue };
        for f in p.adt(a).all_fields() {
            let TyKind::Adt(j, args) = p.types.kind(f.ty) else { continue };
            if !p.is_lang_adt(*j, Lang::Job) || p.types.has_params(f.ty) {
                continue;
            }
            let Some(parts) = p.job_parts(*j, args) else { continue };
            let lacks = parts.iter().skip(1).find(|&&(_, t)| {
                if structural {
                    !crate::traits::implements_builtin(p, t, Lang::Clone)
                } else {
                    !crate::traits::implements(p, t, r)
                }
            });
            if let Some((name, t)) = lacks {
                out.push(
                    Diagnostic::new(
                        codes::E0407,
                        f.span,
                        format!(
                            "`{}` can't be `{}`: the field `{}` is a job, which holds `{name}`, a `{}`, and that isn't",
                            p.adt(a).name,
                            tr.name,
                            f.name,
                            p.display_ty(*t)
                        ),
                    )
                    .with_secondary(imp.span, format!("`{}` opted in here", tr.name))
                    .with_note("a job's value holds its body's locals, and has a trait when they do (§16.3)")
                    .with_help(format!("remove `{}` from the type, or keep the job's locals `{}`", tr.name, tr.name)),
                );
            }
        }
    }
    out
}

/// A declared `@fieldwise` trait is derived from the fields, so every field must have it too
/// (field types that use the type's parameters make the impl conditional).
fn check_fieldwise_opt_in(p: &Program, diags: &mut Vec<Diagnostic>, imp: &ImplDef, tr: &TraitDef) {
    let TyKind::Adt(a, _) = *p.types.kind(imp.self_ty) else { return };
    let Some(r) = &imp.trait_ref else { return };
    let adt = p.adt(a);
    for f in adt.all_fields() {
        if p.types.has_params(f.ty) || crate::traits::implements(p, f.ty, r) {
            continue;
        }
        // A library's `@diagnostic` speaks for the field's type (§7). When another trait the
        // type opts into is one the field lacks too, and its library speaks for the field's
        // type, that error says what's wrong, and this one would only hide it (a presentation
        // type in sim state lacks `StateHash` too, and `SimState`'s message explains why).
        let custom = p.custom_message(f.ty, r);
        if custom.is_none() && spoken_for_elsewhere(p, adt, f.ty) {
            continue;
        }
        let message = match custom {
            Some(m) => m,
            None => format!(
                "`{}` can't derive `{}`: the field `{}` has type `{}`, which doesn't have it",
                adt.name,
                tr.name,
                f.name,
                p.display_ty(f.ty)
            ),
        };
        diags.push(
            Diagnostic::new(codes::E0417, f.span, message)
                .with_secondary(imp.span, format!("`{}` declared here", tr.name))
                .with_note(format!(
                    "`{}` declares `{}`, which is derived field by field, so the field `{}` needs it too (§3)",
                    adt.name, tr.name, f.name
                ))
                .with_help(format!(
                    "declare `{}` on the field's type too, keep the field out of `{}`, or remove `{}` from `{}`",
                    tr.name, adt.name, tr.name, adt.name
                )),
        );
    }
}

/// Whether `adt` opts into a `@fieldwise` trait that `field` lacks and whose library gives a
/// message for `field`'s type (`check_fieldwise_opt_in`).
fn spoken_for_elsewhere(p: &Program, adt: &AdtDef, field: TyId) -> bool {
    adt.opt_in.iter().any(|(r, _)| {
        p.trait_(r.trait_).fieldwise
            && !crate::traits::implements(p, field, r)
            && p.custom_message(field, r).is_some()
    })
}

/// What a type is at its top, for grouping impls: two whose self types have different heads
/// can't overlap. `None` for a generic parameter, which could be any type.
fn ty_head(p: &Program, t: TyId) -> Option<(u8, u32)> {
    Some(match p.types.kind(t) {
        // An array of any length could be any array.
        TyKind::Param(_) | TyKind::Projection { .. } | TyKind::ArrayN(..) | TyKind::Error => {
            return None;
        }
        TyKind::Adt(a, _) => (0, a.0),
        TyKind::Tuple(ts) => (1, ts.len() as u32),
        TyKind::Array(_, n) => (2, *n),
        TyKind::Slice(_) => (3, 0),
        // Anything else unifies only with itself.
        _ => (4, t.0),
    })
}

/// Two impls of one trait whose self types could be the same type conflict (E0403). Only impls
/// with the same head are compared, and those with a generic self type with every other.
fn check_overlap(p: &Program, diags: &mut Vec<Diagnostic>) {
    let mut traits: Vec<&TraitId> = p.impls_of_trait.keys().collect();
    traits.sort();
    for t in traits {
        let impls = &p.impls_of_trait[t];
        let mut by_head: BTreeMap<(u8, u32), Vec<ImplId>> = BTreeMap::new();
        let mut generic = Vec::new();
        for &i in impls {
            match ty_head(p, p.impl_(i).self_ty) {
                Some(h) => by_head.entry(h).or_default().push(i),
                None => generic.push(i),
            }
        }
        let mut pairs: Vec<(ImplId, ImplId)> = Vec::new();
        for group in by_head.values() {
            for (i, &x) in group.iter().enumerate() {
                pairs.extend(group[i + 1..].iter().map(|&y| (x, y)));
            }
        }
        for &g in &generic {
            pairs.extend(impls.iter().filter(|&&i| i != g).map(|&i| (i.min(g), i.max(g))));
        }
        pairs.sort();
        pairs.dedup();
        for (x, y) in pairs {
            let (a, b) = (p.impl_(x), p.impl_(y));
            let (Some(ra), Some(rb)) = (&a.trait_ref, &b.trait_ref) else { continue };
            // A structural opt-in doesn't conflict: it holds through the fields, or through a
            // leaf std writes by hand.
            if (a.from_opt_in || b.from_opt_in)
                && p.trait_(ra.trait_).lang.is_some_and(Lang::is_structural)
            {
                continue;
            }
            // The self type, then the trait's arguments, each impl's parameters bound once.
            let heads = |self_ty: TyId, args: &[TyId]| {
                std::iter::once(self_ty).chain(args.iter().copied()).collect::<Vec<_>>()
            };
            let (ta, tb) = (heads(a.self_ty, &ra.args), heads(b.self_ty, &rb.args));
            // An impl whose types have an error (reported) conflicts with nothing.
            if ta.iter().chain(&tb).any(|&t| p.types.any(t, &mut |k| matches!(k, TyKind::Error))) {
                continue;
            }
            // An impl for any closure or function (`impl<F: fn(vec3) -> f32> Tr for F`) can't
            // meet one for a type that can't be called, such as a struct.
            let fn_only = |i: &ImplDef| matches!(p.types.kind(i.self_ty), TyKind::Param(g) if p.param(*g).fn_bound.is_some());
            let callable = |t: TyId| {
                matches!(
                    p.types.kind(t),
                    TyKind::Param(_) | TyKind::Closure(..) | TyKind::FnDef(..) | TyKind::FnPtr(..)
                )
            };
            if (fn_only(a) && !callable(b.self_ty)) || (fn_only(b) && !callable(a.self_ty)) {
                continue;
            }
            if crate::traits::impls_could_meet(p, &ta, &tb) {
                let name = a.trait_ref.as_ref().map_or("", |r| p.trait_(r.trait_).name.as_str());
                diags.push(
                    Diagnostic::new(
                        codes::E0403,
                        b.span,
                        format!(
                            "this impl of `{name}` for `{}` conflicts with another",
                            p.display_ty(b.self_ty)
                        ),
                    )
                    .with_secondary(a.span, "the other impl")
                    .with_help("remove one of the two impls; a type implements a trait once"),
                );
            }
        }
    }
}

/// `a`, `a and b`, `a, b and c`.
pub(crate) fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Whether the value `ty` holds itself by value, through its fields with their projections
/// resolved: `stack` is the ADT instances being expanded, `finite` those known not to. A chain
/// past 64 instances deep is taken as growing without end.
fn holds_itself(p: &Program, ty: TyId, stack: &mut Vec<TyId>, finite: &mut HashSet<TyId>) -> bool {
    if finite.contains(&ty) {
        return false;
    }
    let holds = match p.types.kind(ty) {
        TyKind::Adt(a, args) => {
            if stack.contains(&ty) || stack.len() >= 64 {
                return true;
            }
            stack.push(ty);
            let adt = p.adt(*a);
            let subst = Subst::from_pairs(&adt.generics, args);
            let holds = adt.all_fields().any(|f| {
                let ft = crate::traits::normalize(p, p.types.subst(f.ty, &subst), None);
                holds_itself(p, ft, stack, finite)
            });
            stack.pop();
            holds
        }
        TyKind::Tuple(ts) => ts.iter().any(|&t| holds_itself(p, t, stack, finite)),
        TyKind::Array(e, _) | TyKind::ArrayN(e, _) => holds_itself(p, *e, stack, finite),
        _ => false,
    };
    if !holds {
        finite.insert(ty);
    }
    holds
}

/// Calls `f` with each alias a type expression depends on, including through a length
/// constant's annotation, resolved in module `m` with its generic parameters in scope.
fn alias_refs(
    p: &Program,
    m: ModuleId,
    t: &ast::TypeExpr,
    params: &[ast::GenericParam],
    f: &mut impl FnMut(AliasId),
) {
    alias_refs_in(p, m, t, params, f, &mut Vec::new());
}

/// A length can depend on an alias through a constant's annotation, including annotations
/// on other constants it uses. These aliases must resolve before the length does.
fn const_alias_refs(p: &Program, c: ConstId, f: &mut impl FnMut(AliasId), seen: &mut Vec<ConstId>) {
    if seen.contains(&c) {
        return;
    }
    seen.push(c);
    let def = p.const_(c);
    if let Some(annotation) = &def.annotation {
        alias_refs_in(p, def.module, annotation, &[], f, seen);
    }
    def.value.walk(&mut |e| {
        if let ast::ExprKind::Path(path) = &e.kind
            && let Some(Res::Const(c)) = resolve::resolve_value_item(p, def.module, path)
        {
            const_alias_refs(p, c, f, seen);
        }
    });
}

fn alias_refs_in(
    p: &Program,
    m: ModuleId,
    t: &ast::TypeExpr,
    params: &[ast::GenericParam],
    f: &mut impl FnMut(AliasId),
    seen: &mut Vec<ConstId>,
) {
    match &t.kind {
        ast::TypeExprKind::Path(path) => {
            if !params.iter().any(|g| g.name.name == path.segments[0].ident.name) {
                match resolve::resolve_value_item(p, m, path) {
                    Some(Res::Alias(a)) => f(a),
                    Some(Res::Const(c)) => const_alias_refs(p, c, f, seen),
                    _ => {}
                }
            }
            for seg in &path.segments {
                for g in seg.generics.iter().flatten() {
                    alias_refs_in(p, m, g, params, f, seen);
                }
            }
        }
        ast::TypeExprKind::Array(e, len) => {
            alias_refs_in(p, m, e, params, f, seen);
            if let Some(len) = len {
                len.walk(&mut |e| {
                    if let ast::ExprKind::Path(path) = &e.kind
                        && !params.iter().any(|g| g.name.name == path.segments[0].ident.name)
                        && let Some(Res::Const(c)) = resolve::resolve_value_item(p, m, path)
                    {
                        const_alias_refs(p, c, f, seen);
                    }
                });
            }
        }
        ast::TypeExprKind::Paren(e) => alias_refs_in(p, m, e, params, f, seen),
        ast::TypeExprKind::Tuple(ts) | ast::TypeExprKind::Traits(ts) => {
            ts.iter().for_each(|x| alias_refs_in(p, m, x, params, f, seen))
        }
        ast::TypeExprKind::Fn(ft) => {
            ft.params.iter().for_each(|x| alias_refs_in(p, m, &x.ty, params, f, seen));
            if let Some(r) = &ft.ret {
                alias_refs_in(p, m, r, params, f, seen);
            }
        }
        ast::TypeExprKind::Int(_) | ast::TypeExprKind::Error => {}
    }
}

/// The most fuel a constant may set (`@fuel`, §13): 2 ** 46 units, some hours of one core.
pub const MAX_FUEL: u64 = 1 << 46;

/// The value of `@fuel`'s argument: a whole number written as a literal, a power (`2 ** 40`) or
/// a product of them.
fn fuel_value(e: &ast::Expr) -> Option<u64> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int(v), .. }) => v.ok(),
        ast::ExprKind::Paren(inner) => fuel_value(inner),
        ast::ExprKind::Binary(op, a, b) => {
            let (a, b) = (fuel_value(a)?, fuel_value(b)?);
            match op {
                ast::BinOp::Pow => a.checked_pow(u32::try_from(b).ok()?),
                ast::BinOp::Mul => a.checked_mul(b),
                ast::BinOp::Shl => {
                    u32::try_from(b).ok().and_then(|b| a.checked_shl(b)).filter(|_| b < 64)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether `text` is a blob budget, as `wrela tool --blob-budget` takes it: digits, then `K`,
/// `M` or `G` if it says, then `,` and digits if it gives the handles.
fn is_blob_budget(text: &str) -> bool {
    let (bytes, handles) = match text.split_once(',') {
        Some((b, h)) => (b, Some(h)),
        None => (text, None),
    };
    let digits = bytes.strip_suffix(['K', 'k', 'M', 'm', 'G', 'g']).unwrap_or(bytes);
    let number = |d: &str| !d.is_empty() && d.len() <= 18 && d.bytes().all(|b| b.is_ascii_digit());
    number(digits) && handles.is_none_or(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wrela_diag::FileId;

    fn collect_source(text: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        collect(crate::test_units(text), &PackageInfo::std_and_program(), &mut diagnostics);
        diagnostics
    }

    #[test]
    fn trait_method_impl_cannot_add_or_change_a_function_bound() {
        for (trait_bound, impl_bound) in [
            ("", ": fn() -> u32"),
            (": fn() -> u32", ": fn(i32) -> u32"),
            (": fn() -> u32", ": @deterministic fn() -> u32"),
        ] {
            let source = format!(
                "trait Call {{ fn call<T{trait_bound}>(value: T) -> u32 }}\n\
                 struct Caller {{}}\n\
                 impl Call for Caller {{ fn call<T{impl_bound}>(value: T) -> u32 {{ 0 }} }}\n"
            );
            let diagnostics = collect_source(&source);
            assert!(
                diagnostics.iter().any(|d| d.code.as_str() == "E0405"),
                "accepted the stronger bound in {source}: {diagnostics:#?}"
            );
        }
        for impl_bound in ["", ": fn() -> u32"] {
            let source = format!(
                "trait Call {{ fn call<T: @deterministic fn() -> u32>(value: T) -> u32 }}\n\
                 struct Caller {{}}\n\
                 impl Call for Caller {{ fn call<T{impl_bound}>(value: T) -> u32 {{ 0 }} }}\n"
            );
            let diagnostics = collect_source(&source);
            assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
        }
    }

    #[test]
    fn trait_method_impl_preserves_constant_parameter_kinds() {
        for (trait_param, impl_param) in [("T", "const T: u32"), ("const T: u32", "T")] {
            let source = format!(
                "trait Call {{ fn call<{trait_param}>() -> u32 }}\n\
                 struct Caller {{}}\n\
                 impl Call for Caller {{ fn call<{impl_param}>() -> u32 {{ 0 }} }}\n"
            );
            let diagnostics = collect_source(&source);
            assert!(
                diagnostics.iter().any(|d| d.code.as_str() == "E0405"),
                "accepted different generic parameter kinds in {source}: {diagnostics:#?}"
            );
        }
    }

    #[test]
    fn import_alias_cannot_share_a_module_name() {
        fn unit(file: u32, path: Vec<String>, text: &str, package: usize) -> SourceUnit {
            let parsed = wrela_syntax::parse(FileId(file), text);
            assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
            SourceUnit {
                path,
                ast: Arc::new(parsed.file),
                package,
                syntax_errors: Vec::new(),
                text: Arc::from(text),
            }
        }

        let mut units: Vec<SourceUnit> = crate::STD_SOURCES
            .iter()
            .enumerate()
            .map(|(i, &(name, text))| {
                unit(i as u32, name.split("::").map(str::to_string).collect(), text, 0)
            })
            .collect();
        let next = units.len() as u32;
        units.push(unit(next, vec!["main".into()], "use std::units::m as shapes\n", 1));
        units.push(unit(next + 1, vec!["main".into(), "shapes".into()], "pub const one = 1\n", 1));
        let mut diagnostics = Vec::new();
        collect(units, &PackageInfo::std_and_program(), &mut diagnostics);
        assert!(
            diagnostics
                .iter()
                .any(|d| { d.code.as_str() == "E0201" && d.message.contains("also the module") }),
            "{:?}",
            diagnostics.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>()
        );
    }
}
