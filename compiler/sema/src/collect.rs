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
pub const MAX_DERIVED_TUPLE: usize = 12;

/// One parsed source file and the module it is.
pub struct SourceUnit {
    /// The module path in its package: `["shapes", "blob"]`, or `["std", "field"]` for std.
    pub path: Vec<String>,
    pub ast: ast::File,
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
    /// A lifted build lifts its literals (language.md §22): its literal tables are built where
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
    c.check_items_beside_modules();
    c.resolve_imports();
    c.find_lang_items();
    c.resolve_signatures();
    c.index_impls();
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
                self.p.new_param(ParamDef {
                    name: g.name.name.clone(),
                    bounds: Vec::new(),
                    span: g.name.span,
                    is_self: false,
                    is_const: g.const_ty.is_some(),
                    fn_bound: None,
                })
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
        // std's unsafe core: its public functions are callable only inside `unsafe` (§6.14).
        attrs.unsafe_call = is_std
            && public
            && matches!(self.p.module(m).path.get(1).map(String::as_str), Some("mem" | "alloc"))
            && !matches!(
                f.name.name.as_str(),
                "size_of" | "align_of" | "needs_drop" | "allocations" | "debug_build"
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
                for v in &e.variants {
                    if variants.iter().any(|p| p.name == v.name.name) {
                        continue;
                    }
                    let shape = match &v.kind {
                        ast::VariantKind::Unit => VariantShape::Unit,
                        ast::VariantKind::Tuple(_) => VariantShape::Tuple,
                        ast::VariantKind::Struct(_) => VariantShape::Struct,
                    };
                    let name = v.name.name.clone();
                    variants.push(VariantDef { name, shape, fields: Vec::new(), span: v.span });
                }
                let kind = AdtKind::Enum(variants);
                let a = self.declare_adt(m, item, &e.name, &e.generics, kind, vis);
                self.p.adts[a.index()].diagnostic = diagnostic;
            }
            ast::ItemKind::Trait(t) => {
                let id = TraitId(self.p.traits.len() as u32);
                let self_param = self.p.new_param(ParamDef {
                    name: "Self".into(),
                    bounds: Vec::new(),
                    span: t.name.span,
                    is_self: true,
                    is_const: false,
                    fn_bound: None,
                });
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
                    span: item.span,
                    lang: None,
                    fieldwise,
                    diagnostic,
                });
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
                // Its type, once it's known, is the function's result (`check_program`).
                self.p.fns.push(FnDef {
                    name: c.name.name.clone(),
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
                    span: item.span,
                    name_span: c.name.span,
                    sig_span: item.span,
                    lang: None,
                    derived: None,
                });
                let eval = FnId(self.p.fns.len() as u32 - 1);
                self.p.consts.push(ConstDef {
                    name: c.name.name.clone(),
                    module: m,
                    ty: None,
                    value: c.value.clone(),
                    public,
                    span: item.span,
                    eval,
                    default: false,
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
                    let text = a.args.as_deref().and_then(|args| match args {
                        [
                            ast::Arg {
                                value: ast::Expr { kind: ast::ExprKind::Lit(l), .. }, ..
                            },
                        ] if l.kind == ast::LitKind::Str => {
                            Some(wrela_syntax::lexer::string_value(&l.text))
                        }
                        _ => None,
                    });
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
        });
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
                    if a.args.is_some() {
                        self.diags.push(Diagnostic::new(
                            codes::E0204,
                            a.span,
                            "`@thread_entry` takes no arguments",
                        ));
                    }
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
                            if a.args.is_some() {
                                self.diags.push(
                                    Diagnostic::new(
                                        codes::E0602,
                                        a.span,
                                        format!("`@{name}` takes no arguments"),
                                    )
                                    .with_fix(
                                        "remove the arguments",
                                        a.span,
                                        format!("@{name}"),
                                    ),
                                );
                            }
                            if name == "vertex" { Entry::Vertex } else { Entry::Fragment }
                        }
                        // A size that's wrong is reported; the kernel is still one, so its
                        // dispatches aren't reported too.
                        _ => Entry::Compute(self.workgroup_size(a).unwrap_or([1, 1, 1])),
                    };
                    out.entry = Some((entry, a.span));
                }
                "gpu" => {
                    if a.args.is_some() {
                        self.diags.push(
                            Diagnostic::new(codes::E0602, a.span, "`@gpu` takes no arguments")
                                .with_fix("remove the arguments", a.span, "@gpu"),
                        );
                    }
                    out.gpu = Some(a.span);
                }
                "intrinsic" if is_std => out.intrinsic = true,
                "test" => {
                    if let Some(args) = &a.args {
                        (out.test_frames, out.test_ticks, out.test_input) =
                            self.test_frames(a, args);
                    }
                    out.test = Some(a.span);
                }
                "deterministic" | "audio" => {
                    if a.args.is_some() {
                        self.diags.push(
                            Diagnostic::new(
                                codes::E0204,
                                a.span,
                                format!("`@{name}` takes no arguments"),
                            )
                            .with_fix(
                                "remove the arguments",
                                a.span,
                                format!("@{name}"),
                            ),
                        );
                    }
                    if name == "deterministic" {
                        out.deterministic = Some(a.span);
                    } else {
                        out.audio = Some(a.span);
                    }
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
                        "comptime" => "write the work as a `const`, which the build runs (§10)",
                        "escaping" => {
                            "a closure that captures only values can be stored as it is (§6.7)"
                        }
                        _ => {
                            "state a bound as a method, such as `lipschitz(near)`, and test it (§13)"
                        }
                    };
                    self.diags.push(
                        Diagnostic::new(codes::E0204, a.span, format!("wrela has no `@{name}`"))
                            .with_note("it was decided against (language.md §18)")
                            .with_help(instead),
                    );
                }
                _ => {
                    let known =
                        ["compute", "vertex", "fragment", "gpu", "deterministic", "audio", "test"];
                    let mut d = Diagnostic::new(
                        codes::E0204,
                        a.name.span,
                        format!("there's no attribute `@{name}`"),
                    )
                    .with_note("attributes are a closed set defined by the language (D-037)");
                    if let Some(s) = resolve::closest(name, known.iter().copied()) {
                        d = d.with_fix(format!("did you mean `@{s}`?"), a.name.span, s);
                    }
                    self.diags.push(d);
                }
            }
        }
        out
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
                None => self.diags.push(Diagnostic::new(
                    codes::E0204,
                    arg.span,
                    "`@effects` names effects: alloc, io, nondet, recursion, host or panic",
                )),
            }
        }
        out
    }

    /// `@compute(x)`, `@compute(x, y)` or `@compute(x, y, z)`, within WebGPU's default limits.
    /// `@test(frames: n)`'s `n` (§10): from 1 to [`MAX_TEST_FRAMES`], or `@test(ticks: n)`'s,
    /// and, with `input: "path"`, the script of input events (or the tick log) they get.
    /// Anything else is E0222.
    fn test_frames(
        &mut self,
        a: &ast::Attribute,
        args: &[ast::Arg],
    ) -> (Option<u32>, Option<u32>, Option<(String, Span)>) {
        let named =
            |name: &str| args.iter().find(|x| x.name.as_ref().is_some_and(|n| n.name == name));
        let others = args.iter().any(|x| {
            !x.name
                .as_ref()
                .is_some_and(|n| matches!(n.name.as_str(), "frames" | "ticks" | "input"))
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
        let input = match named("input") {
            None => Ok(None),
            Some(x) => match &x.value.kind {
                ast::ExprKind::Lit(l @ ast::Lit { kind: ast::LitKind::Str, .. }) => {
                    Ok(Some((wrela_syntax::lexer::string_value(&l.text), x.value.span)))
                }
                _ => Err(()),
            },
        };
        match (n, input) {
            (Some(n), Ok(input)) if !others => {
                let n = n as u32;
                if frames { (Some(n), None, input) } else { (None, Some(n), input) }
            }
            _ => {
                self.diags.push(
                    Diagnostic::new(
                        codes::E0222,
                        a.span,
                        format!("`@test` takes `frames: n` or `ticks: n`, from 1 to {MAX_TEST_FRAMES}: how many of the program's frames, or ticks, run before the test; and may take `input: \"script.json\"`, a script of input events for them (or, with `ticks`, a tick log to check them against)"),
                    )
                    .with_note("`@test` alone is a test of code; `@test(frames: 600)` runs the program for 600 frames, then the test with its state (§10)"),
                );
                (None, None, None)
            }
        }
    }

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
                    )),
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
                        self.test_signature(fid);
                    }
                }
                PendingItem::Fn(id, f) => {
                    self.fn_signature(*id, f);
                    self.test_signature(*id);
                }
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
                alias_refs(&self.p, *m, &t.ty, &mut |a| {
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
            let mut quiet = Vec::new();
            let mut bounds = None;
            let _ = resolve::resolve_type(
                &self.p,
                &mut quiet,
                &scope,
                &t.ty,
                TyPos::Return(&mut bounds),
            );
            if let Some(bounds) = bounds {
                self.diags.extend(quiet);
                let ty = self.opaque_alias(m, id, t, bounds, pending);
                self.p.aliases[id.index()].ty = ty;
                continue;
            }
            let ty = resolve::resolve_type(&self.p, self.diags, &scope, &t.ty, TyPos::Normal);
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

    /// E0222: a `@test` is a free function of no parameters and no result, run on the CPU when
    /// `wrela test` asks (§10).
    fn test_signature(&mut self, id: FnId) {
        let def = self.p.func(id);
        let Some(at) = def.attrs.test else { return };
        let frames = def.attrs.runs_program();
        let program = self.p.package_of(def.module).kind == PackageKind::Program;
        // A dependency's frame test is about its own program, checked when it's the package
        // checked; here its frames aren't this program's.
        if frames && !program {
            return;
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
                .with_note("a test is `@test fn name() { ... }`, or `@test(frames: n) fn name(state: State) { ... }` to run the program's frames first (`ticks: n`, its ticks): it passes unless it panics, and a failed `assert` panics"),
            );
        }
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
                    )
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
                for v in &e.variants {
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
    /// constant expression becomes a constant the build computes once (§10), which the default
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
        self.p.fns.push(FnDef {
            name: of.to_string(),
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
            span: d.span,
            name_span: d.span,
            sig_span: d.span,
            lang: None,
            derived: None,
        });
        let eval = FnId(self.p.fns.len() as u32 - 1);
        self.p.consts.push(ConstDef {
            name: of.to_string(),
            module: m,
            ty: Some(ty),
            value: d.clone(),
            public: false,
            span: d.span,
            eval,
            default: true,
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
            let ty = resolve::resolve_type(&self.p, self.diags, scope, &f.ty, pos);
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
                ));
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
                        .with_note("a value's fields are dropped when it is; only std's core defines destructors (§18)")
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
                    let t = resolve::resolve_type(
                        &self.p,
                        self.diags,
                        &scope,
                        ty,
                        TyPos::Param(&mut implicit),
                    );
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
                let run = matches!(self.p.types.kind(t), TyKind::Slice(_) | TyKind::Str);
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
                    )
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
                    ));
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
            let def = self.p.func(m).clone();
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
            self.p.fns.push(FnDef {
                name: def.name.clone(),
                module: imp.module,
                owner: FnOwner::Impl(id),
                generics: def.generics.clone(),
                params,
                ret,
                ret_mode: def.ret_mode,
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
            // `None` for the array.
            for arity in std::iter::once(None).chain((2..=MAX_DERIVED_TUPLE).map(Some)) {
                let param = |p: &mut Program, name: String, is_const: bool| {
                    p.new_param(ParamDef {
                        name,
                        bounds: Vec::new(),
                        span: tr.span,
                        is_self: false,
                        is_const,
                        fn_bound: None,
                    })
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
                let id = ImplId(self.p.impls.len() as u32);
                let methods = self.derived_methods(id, &imp);
                self.p.impls.push(ImplDef { methods, ..imp });
            }
        }
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
                implied.push((
                    AdtId(a as u32),
                    ImplDef {
                        module: adt.module,
                        generics: adt.generics.clone(),
                        trait_ref: Some(r.clone()),
                        self_ty,
                        assoc_types: BTreeMap::new(),
                        methods: Vec::new(),
                        span: *span,
                        from_opt_in: true,
                    },
                ));
            }
        }
        for (_, imp) in implied {
            let id = ImplId(self.p.impls.len() as u32);
            let methods = self.derived_methods(id, &imp);
            self.p.impls.push(ImplDef { methods, ..imp });
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
                )
                .with_note("associated types belong to traits"),
            );
        }
        return;
    };
    let tr = p.trait_(r.trait_);
    let structural = tr.lang.is_some_and(Lang::is_structural);
    // Destructors are std's core's alone (§18).
    if tr.lang == Some(Lang::Drop) && !p.is_std(imp.module) {
        diags.push(
            Diagnostic::new(
                codes::E0413,
                imp.span,
                "only std's core defines destructors",
            )
            .with_note("wrela has no user-defined destructors: a value's fields are dropped when it is (§18)")
            .with_help("to run code when something ends, call it where it ends"),
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
            diags.push(Diagnostic::new(
                codes::E0318,
                imp.span,
                format!(
                    "`type {}` is defined in terms of itself: `{}` never resolves to a type",
                    at.name,
                    p.display_ty(cycle)
                ),
            ));
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
                ));
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
        match found {
            None if tdef.body.is_none() && !imp.from_opt_in => {
                diags.push(
                    Diagnostic::new(
                        codes::E0401,
                        imp.span,
                        format!("this impl of `{}` is missing `fn {}`", tr.name, tdef.name),
                    )
                    .with_secondary(tdef.sig_span, "declared here"),
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
                            )
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
    };
    if a.generics.len() != b.generics.len() {
        return diags.push(mismatch(format!(
            "it has {} generic parameters, the trait's has {}",
            a.generics.len(),
            b.generics.len()
        )));
    }
    for (&tp, &ip) in b.generics.iter().zip(&a.generics) {
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
    // An enum is its `u32` tag and its payloads' bytes, laid out as the CPU lays it out, on
    // both targets (`ir::gpu_memory`): its payloads must be `GpuData`, as a struct's fields.
    for f in adt.all_fields() {
        if p.types.has_params(f.ty) {
            continue; // conditional on the arguments; checked where it's used
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
                .with_secondary(imp.span, format!("`{}` opted in here", tr.name)),
            );
        }
    }
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
        if custom.is_none() && spoken_for_elsewhere(p, imp, f.ty) {
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
                )),
        );
    }
}

/// Whether `self_ty` opts into a `@fieldwise` trait that `field` lacks and whose library gives
/// a message for `field`'s type (`check_fieldwise_opt_in`).
fn spoken_for_elsewhere(p: &Program, imp: &ImplDef, field: TyId) -> bool {
    p.impls_of_trait.iter().any(|(t, impls)| {
        p.trait_(*t).fieldwise
            && impls.iter().any(|&i| {
                let o = p.impl_(i);
                o.from_opt_in
                    && o.self_ty == imp.self_ty
                    && o.trait_ref.as_ref().is_some_and(|r| {
                        !crate::traits::implements(p, field, r)
                            && p.custom_message(field, r).is_some()
                    })
            })
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
                    .with_secondary(a.span, "the other impl"),
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

/// Calls `f` with each type alias a type expression names by a single-segment path, resolved in
/// module `m`.
fn alias_refs(p: &Program, m: ModuleId, t: &ast::TypeExpr, f: &mut impl FnMut(AliasId)) {
    match &t.kind {
        ast::TypeExprKind::Path(path) => {
            if let Some(Res::Alias(a)) = resolve::resolve_value_item(p, m, path) {
                f(a);
            }
            for seg in &path.segments {
                for g in seg.generics.iter().flatten() {
                    alias_refs(p, m, g, f);
                }
            }
        }
        ast::TypeExprKind::Array(e, _) | ast::TypeExprKind::Paren(e) => alias_refs(p, m, e, f),
        ast::TypeExprKind::Tuple(ts) | ast::TypeExprKind::Traits(ts) => {
            ts.iter().for_each(|x| alias_refs(p, m, x, f))
        }
        ast::TypeExprKind::Fn(ft) => {
            ft.params.iter().for_each(|x| alias_refs(p, m, &x.ty, f));
            if let Some(r) = &ft.ret {
                alias_refs(p, m, r, f);
            }
        }
        ast::TypeExprKind::Int(_) | ast::TypeExprKind::Error => {}
    }
}
