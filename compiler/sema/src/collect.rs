//! Collects a program's definitions from its parsed files: the module tree, every item, `use`
//! imports, and every signature (generics, fields, variants, traits, impls, functions).

use crate::defs::*;
use crate::program::{LangRes, Program};
use crate::resolve::{self, Scope, TyPos};
use crate::ty::*;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use wrela_diag::{Diagnostic, FileId, Span, codes};
use wrela_syntax::ast;

/// One parsed source file and the module it is.
pub struct SourceUnit {
    /// The module path: `["shapes", "blob"]`, or `["std", "field"]`.
    pub path: Vec<String>,
    pub file: FileId,
    pub ast: Rc<ast::File>,
    pub is_std: bool,
}

struct PendingUse {
    module: ModuleId,
    path: Vec<ast::Ident>,
    alias: ast::Ident,
    public: bool,
    span: Span,
}

/// What `collect` remembers between its passes about each AST item.
enum PendingItem {
    Fn(FnId, Rc<ast::FnDecl>),
    Adt(AdtId, Rc<ast::Item>),
    Trait(TraitId, Rc<ast::TraitDecl>, Vec<(FnId, Rc<ast::FnDecl>)>),
    Impl(ImplId, Rc<ast::ImplDecl>, Vec<(FnId, Rc<ast::FnDecl>)>),
    Const(ConstId, Rc<ast::ConstDecl>),
}

pub fn collect(units: Vec<SourceUnit>, diags: &mut Vec<Diagnostic>) -> Program {
    let mut c = Collector { p: Program::default(), diags, pending: Vec::new(), uses: Vec::new() };
    c.build_modules(&units);
    c.declare_items(&units);
    c.resolve_imports();
    c.find_lang_items();
    c.resolve_signatures();
    c.check_recursive_types();
    c.check_impls();
    c.p
}

struct Collector<'d> {
    p: Program,
    diags: &'d mut Vec<Diagnostic>,
    pending: Vec<(ModuleId, PendingItem)>,
    uses: Vec<PendingUse>,
}

impl<'d> Collector<'d> {
    fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    // ---- modules ---------------------------------------------------------------------------

    fn new_module(&mut self, path: Vec<String>, is_std: bool) -> ModuleId {
        self.p.modules.push(Module {
            path,
            file: None,
            children: BTreeMap::new(),
            scope: BTreeMap::new(),
            broken: BTreeSet::new(),
            is_std,
            ast: None,
        });
        ModuleId(self.p.modules.len() as u32 - 1)
    }

    fn build_modules(&mut self, units: &[SourceUnit]) {
        let pkg = self.new_module(Vec::new(), false);
        let std = self.new_module(vec!["std".into()], true);
        self.p.package_root = Some(pkg);
        self.p.std_root = Some(std);
        for u in units {
            let (mut m, rest) = if u.is_std { (std, &u.path[1..]) } else { (pkg, &u.path[..]) };
            for seg in rest {
                m = match self.p.modules[m.index()].children.get(seg) {
                    Some(&c) => c,
                    None => {
                        let mut path = self.p.modules[m.index()].path.clone();
                        path.push(seg.clone());
                        let c = self.new_module(path, u.is_std);
                        self.p.modules[m.index()].children.insert(seg.clone(), c);
                        c
                    }
                };
            }
            let module = &mut self.p.modules[m.index()];
            module.file = Some(u.file);
            module.ast = Some(u.ast.clone());
            if !u.is_std && u.path == ["main"] {
                self.p.main = Some(m);
            }
        }
    }

    fn bind(&mut self, m: ModuleId, name: &ast::Ident, res: Res, public: bool) {
        let scope = &mut self.p.modules[m.index()].scope;
        if let Some(prev) = scope.get(&name.name) {
            let prev_span = prev.span;
            self.err(
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
        scope.insert(name.name.clone(), Binding { res, public, span: name.span });
    }

    // ---- items -----------------------------------------------------------------------------

    fn declare_items(&mut self, units: &[SourceUnit]) {
        let mut module_of_file = Vec::new();
        for (i, m) in self.p.modules.iter().enumerate() {
            if let Some(f) = m.file {
                module_of_file.push((f, ModuleId(i as u32)));
            }
        }
        for u in units {
            let Some(&(_, m)) = module_of_file.iter().find(|(f, _)| *f == u.file) else { continue };
            for item in &u.ast.items {
                self.declare_item(m, item, u.is_std);
            }
        }
    }

    fn declare_generics(&mut self, generics: &[ast::GenericParam]) -> Vec<ParamId> {
        generics
            .iter()
            .map(|g| {
                self.p.new_param(ParamDef {
                    name: g.name.name.clone(),
                    bounds: Vec::new(),
                    span: g.name.span,
                    is_self: false,
                })
            })
            .collect()
    }

    fn new_fn(
        &mut self,
        m: ModuleId,
        owner: FnOwner,
        f: &ast::FnDecl,
        public: bool,
        attrs: &[ast::Attribute],
        is_std: bool,
    ) -> FnId {
        let generics = self.declare_generics(&f.generics);
        let attrs = self.fn_attrs(attrs, is_std);
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
            span: f.sig_span.to(f.body.as_ref().map_or(f.sig_span, |b| b.span)),
            name_span: f.name.span,
            sig_span: f.sig_span,
            lang: None,
        });
        FnId(self.p.fns.len() as u32 - 1)
    }

    fn declare_item(&mut self, m: ModuleId, item: &ast::Item, is_std: bool) {
        let public = item.vis.is_some();
        if !matches!(item.kind, ast::ItemKind::Fn(_)) {
            for a in &item.attrs {
                self.err(
                    Diagnostic::new(
                        codes::E0108,
                        a.span,
                        format!("`@{}` only applies to functions", a.name.name),
                    )
                    .with_fix("remove it", a.span, ""),
                );
            }
        }
        match &item.kind {
            ast::ItemKind::Error(name) => {
                if let Some(n) = name {
                    self.p.modules[m.index()].broken.insert(n.name.clone());
                }
            }
            ast::ItemKind::Fn(f) => {
                let id = self.new_fn(m, FnOwner::Free, f, public, &item.attrs, is_std);
                self.bind(m, &f.name, Res::Fn(id), public);
                self.pending.push((m, PendingItem::Fn(id, Rc::new(f.clone()))));
            }
            ast::ItemKind::Struct(s) => {
                let generics = self.declare_generics(&s.generics);
                let id = AdtId(self.p.adts.len() as u32);
                self.p.adts.push(AdtDef {
                    name: s.name.name.clone(),
                    module: m,
                    generics,
                    kind: AdtKind::Struct(Vec::new()),
                    opt_in: Vec::new(),
                    public,
                    span: item.span,
                    name_span: s.name.span,
                    lang: None,
                });
                self.bind(m, &s.name, Res::Adt(id), public);
                self.pending.push((m, PendingItem::Adt(id, Rc::new(item.clone()))));
            }
            ast::ItemKind::Enum(e) => {
                let generics = self.declare_generics(&e.generics);
                let id = AdtId(self.p.adts.len() as u32);
                self.p.adts.push(AdtDef {
                    name: e.name.name.clone(),
                    module: m,
                    generics,
                    kind: AdtKind::Enum(Vec::new()),
                    opt_in: Vec::new(),
                    public,
                    span: item.span,
                    name_span: e.name.span,
                    lang: None,
                });
                self.bind(m, &e.name, Res::Adt(id), public);
                self.pending.push((m, PendingItem::Adt(id, Rc::new(item.clone()))));
            }
            ast::ItemKind::Trait(t) => {
                let id = TraitId(self.p.traits.len() as u32);
                let self_param = self.p.new_param(ParamDef {
                    name: "Self".into(),
                    bounds: Vec::new(),
                    span: t.name.span,
                    is_self: true,
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
                });
                let mut methods = Vec::new();
                for member in &t.members {
                    if let ast::TraitMemberKind::Fn(f) = &member.kind {
                        if let Some(prev) = methods
                            .iter()
                            .find(|(_, g): &&(FnId, Rc<ast::FnDecl>)| g.name.name == f.name.name)
                        {
                            let prev_span = prev.1.name.span;
                            self.err(
                                Diagnostic::new(
                                    codes::E0201,
                                    f.name.span,
                                    format!("`{}` is declared twice in this trait", f.name.name),
                                )
                                .with_secondary(prev_span, "first declared here"),
                            );
                            continue;
                        }
                        let fid =
                            self.new_fn(m, FnOwner::Trait(id), f, true, &member.attrs, is_std);
                        methods.push((fid, Rc::new(f.clone())));
                    }
                }
                self.p.traits[id.index()].methods = methods.iter().map(|(f, _)| *f).collect();
                self.bind(m, &t.name, Res::Trait(id), public);
                self.pending.push((m, PendingItem::Trait(id, Rc::new(t.clone()), methods)));
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
                        let public = member.vis.is_some() || i.trait_.is_some();
                        let fid =
                            self.new_fn(m, FnOwner::Impl(id), f, public, &member.attrs, is_std);
                        methods.push((fid, Rc::new(f.clone())));
                    }
                }
                self.p.impls[id.index()].methods = methods.iter().map(|(f, _)| *f).collect();
                self.pending.push((m, PendingItem::Impl(id, Rc::new(i.clone()), methods)));
            }
            ast::ItemKind::Const(c) => {
                let id = ConstId(self.p.consts.len() as u32);
                self.p.consts.push(ConstDef {
                    name: c.name.name.clone(),
                    module: m,
                    ty: None,
                    value: c.value.clone(),
                    public,
                    span: item.span,
                });
                self.bind(m, &c.name, Res::Const(id), public);
                self.pending.push((m, PendingItem::Const(id, Rc::new(c.clone()))));
            }
            ast::ItemKind::Use(u) => self.flatten_use(m, u, Vec::new(), public),
        }
    }

    fn flatten_use(
        &mut self,
        m: ModuleId,
        u: &ast::UseTree,
        prefix: Vec<ast::Ident>,
        public: bool,
    ) {
        let mut path = prefix;
        path.extend(u.path.iter().cloned());
        match &u.kind {
            ast::UseKind::Simple(alias) => {
                let alias = alias.clone().unwrap_or_else(|| path[path.len() - 1].clone());
                self.uses.push(PendingUse { module: m, path, alias, public, span: u.span });
            }
            ast::UseKind::Group(trees) => {
                for t in trees {
                    self.flatten_use(m, t, path.clone(), public);
                }
            }
        }
    }

    fn fn_attrs(&mut self, attrs: &[ast::Attribute], is_std: bool) -> FnAttrs {
        let mut out = FnAttrs::default();
        for a in attrs {
            let name = a.name.name.as_str();
            match name {
                "compute" | "vertex" | "fragment" => {
                    if let Some((_, prev)) = out.entry {
                        self.err(
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
                                self.err(
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
                        _ => match self.workgroup_size(a) {
                            Some(size) => Entry::Compute(size),
                            None => continue,
                        },
                    };
                    out.entry = Some((entry, a.span));
                }
                "gpu" => out.gpu = Some(a.span),
                "intrinsic" if is_std => out.intrinsic = true,
                "comptime" | "deterministic" | "assert" | "assume" | "escaping" | "diagnostic" => {
                    self.err(
                        Diagnostic::new(codes::E0903, a.span, format!("`@{name}` is tier 1; it comes in milestone 2 or 3"))
                            .with_note("tier 0 has `@compute`, `@vertex`, `@fragment` and `@gpu` (language.md §9)"),
                    );
                }
                "audio" => {
                    self
                        .err(Diagnostic::new(codes::E0903, a.span, "`@audio` is tier 2").with_note(
                        "tier 0 has `@compute`, `@vertex`, `@fragment` and `@gpu` (language.md §9)",
                    ));
                }
                _ => {
                    let known = ["compute", "vertex", "fragment", "gpu"];
                    let mut d = Diagnostic::new(
                        codes::E0204,
                        a.name.span,
                        format!("there's no attribute `@{name}`"),
                    )
                    .with_note("attributes are a closed set defined by the language (D-037)");
                    if let Some(s) = resolve::closest(name, known.iter().copied()) {
                        d = d.with_fix(format!("did you mean `@{s}`?"), a.name.span, s);
                    }
                    self.err(d);
                }
            }
        }
        out
    }

    /// `@compute(x)`, `@compute(x, y)` or `@compute(x, y, z)`, within WebGPU's default limits.
    fn workgroup_size(&mut self, a: &ast::Attribute) -> Option<[u32; 3]> {
        let Some(args) = &a.args else {
            self.err(
                Diagnostic::new(codes::E0602, a.span, "`@compute` needs a workgroup size")
                    .with_help("write `@compute(64)` for 64 invocations per workgroup")
                    .with_fix("use a workgroup of 64", a.span, "@compute(64)"),
            );
            return None;
        };
        if args.is_empty() || args.len() > 3 {
            self.err(Diagnostic::new(
                codes::E0602,
                a.span,
                "`@compute` takes one to three sizes: `@compute(x, y, z)`",
            ));
            return None;
        }
        let mut size = [1u32; 3];
        for (i, arg) in args.iter().enumerate() {
            let v = match &arg.value.kind {
                ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Int, text, .. })
                    if arg.name.is_none() =>
                {
                    wrela_syntax::lexer::int_value(text)
                }
                _ => None,
            };
            match v {
                Some(v) if v >= 1 && v <= u32::MAX as u64 => size[i] = v as u32,
                _ => {
                    self.err(Diagnostic::new(
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
                self.err(
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
            self.err(
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
            let mut still = Vec::new();
            for u in pending {
                match resolve::resolve_module_path(&self.p, u.module, &u.path) {
                    resolve::PathLookup::Found(res) => {
                        self.bind(u.module, &u.alias, res, u.public);
                    }
                    resolve::PathLookup::NotYet => still.push(u),
                    resolve::PathLookup::Broken => {
                        self.p.modules[u.module.index()].broken.insert(u.alias.name.clone());
                    }
                    resolve::PathLookup::Error(d) => self.err(*d),
                }
            }
            if still.is_empty() {
                break;
            }
            if still.len() == before {
                // No progress: report each with what's actually wrong.
                for u in still {
                    match resolve::resolve_module_path_in(&self.p, u.module, &u.path, true) {
                        resolve::PathLookup::Error(d) => self.err(*d),
                        resolve::PathLookup::Broken => {}
                        _ => {
                            let text = u
                                .path
                                .iter()
                                .map(|i| i.name.as_str())
                                .collect::<Vec<_>>()
                                .join("::");
                            self.err(Diagnostic::new(codes::E0209, u.span, format!("`{text}` can't be resolved: the imports refer to each other in a cycle")));
                        }
                    }
                }
                break;
            }
            pending = still;
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
                _ => self.err(Diagnostic::internal(format!(
                    "std has no `{path}`, which the compiler relies on"
                ))),
            }
        }
    }

    // ---- signatures ------------------------------------------------------------------------

    fn resolve_signatures(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        // Bounds and supertraits first, so later lookups (`T::Kind`) can see them.
        for (m, item) in &pending {
            match item {
                PendingItem::Trait(id, t, _) => self.trait_header(*m, *id, t),
                PendingItem::Adt(id, item) => self.adt_bounds(*m, *id, item),
                PendingItem::Impl(id, i, _) => self.impl_header(*m, *id, i),
                PendingItem::Fn(id, f) => self.fn_bounds(*m, *id, f, &[]),
                PendingItem::Const(..) => {}
            }
        }
        for (m, item) in &pending {
            match item {
                PendingItem::Trait(id, _, methods) => {
                    let self_ty = self.p.types.param(self.p.trait_(*id).self_param);
                    let mut scope = Scope::new(*m);
                    scope.self_ty = Some(self_ty);
                    scope.push_params(&self.p, &[self.p.trait_(*id).self_param]);
                    let gens = self.p.trait_(*id).generics.clone();
                    scope.push_params(&self.p, &gens);
                    for (fid, f) in methods {
                        self.fn_bounds(*m, *fid, f, &scope.params);
                        self.fn_signature(&scope, *fid, f);
                    }
                }
                PendingItem::Impl(id, _, methods) => {
                    let mut scope = Scope::new(*m);
                    scope.self_ty = Some(self.p.impl_(*id).self_ty);
                    let gens = self.p.impl_(*id).generics.clone();
                    scope.push_params(&self.p, &gens);
                    for (fid, f) in methods {
                        self.fn_bounds(*m, *fid, f, &scope.params);
                        self.fn_signature(&scope, *fid, f);
                    }
                }
                PendingItem::Fn(id, f) => {
                    let scope = Scope::new(*m);
                    self.fn_signature(&scope, *id, f);
                }
                PendingItem::Adt(id, item) => self.adt_body(*m, *id, item),
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

    fn bounds_of(&mut self, scope: &Scope, bounds: &[ast::TypeExpr]) -> Vec<TraitRef> {
        bounds
            .iter()
            .filter_map(|b| resolve::resolve_trait_ref(&self.p, self.diags, scope, b))
            .collect()
    }

    fn set_param_bounds(
        &mut self,
        scope: &Scope,
        params: &[ParamId],
        generics: &[ast::GenericParam],
    ) {
        for (p, g) in params.iter().zip(generics) {
            let bounds = self.bounds_of(scope, &g.bounds);
            self.p.params[p.index()].bounds = bounds;
        }
    }

    fn trait_header(&mut self, m: ModuleId, id: TraitId, t: &ast::TraitDecl) {
        let self_param = self.p.trait_(id).self_param;
        let gens = self.p.trait_(id).generics.clone();
        let mut scope = Scope::new(m);
        scope.self_ty = Some(self.p.types.param(self_param));
        scope.push_params(&self.p, &[self_param]);
        scope.push_params(&self.p, &gens);
        self.set_param_bounds(&scope, &gens, &t.generics);
        let supers = self.bounds_of(&scope, &t.supertraits);
        for s in &supers {
            if s.trait_ == id {
                self.err(Diagnostic::new(
                    codes::E0411,
                    t.name.span,
                    format!("`{}` can't be its own supertrait", t.name.name),
                ));
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
                    self.err(Diagnostic::new(
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
    }

    fn adt_bounds(&mut self, m: ModuleId, id: AdtId, item: &ast::Item) {
        let gens = self.p.adt(id).generics.clone();
        let mut scope = Scope::new(m);
        scope.push_params(&self.p, &gens);
        let ast_generics = match &item.kind {
            ast::ItemKind::Struct(s) => &s.generics,
            ast::ItemKind::Enum(e) => &e.generics,
            _ => return,
        };
        self.set_param_bounds(&scope, &gens, ast_generics);
    }

    fn adt_body(&mut self, m: ModuleId, id: AdtId, item: &ast::Item) {
        let gens = self.p.adt(id).generics.clone();
        let mut scope = Scope::new(m);
        scope.push_params(&self.p, &gens);
        let self_args: Vec<TyId> = gens.iter().map(|&g| self.p.types.param(g)).collect();
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
                        self.err(
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
                                    default: None,
                                    span: t.span,
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
        let mut opt_in = Vec::new();
        for t in traits {
            if let Some(r) = resolve::resolve_trait_ref(&self.p, self.diags, &scope, t) {
                opt_in.push((r, t.span));
            }
        }
        self.p.adts[id.index()].opt_in = opt_in;
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
                self.err(
                    Diagnostic::new(
                        codes::E0201,
                        f.name.span,
                        format!("the field `{}` is declared twice", f.name.name),
                    )
                    .with_secondary(ps, "first declared here"),
                );
                continue;
            }
            let ty = resolve::resolve_type(&self.p, self.diags, scope, &f.ty, TyPos::Normal);
            let default = if allow_default { f.default.clone() } else { None };
            if !allow_default && let Some(d) = &f.default {
                self.err(Diagnostic::new(
                    codes::E0324,
                    d.span,
                    "variant fields can't have defaults",
                ));
            }
            out.push(FieldDef {
                name: f.name.name.clone(),
                ty,
                public: f.vis.is_some(),
                default,
                span: f.span,
            });
        }
        out
    }

    fn impl_header(&mut self, m: ModuleId, id: ImplId, i: &ast::ImplDecl) {
        let gens = self.p.impl_(id).generics.clone();
        let mut scope = Scope::new(m);
        scope.push_params(&self.p, &gens);
        self.set_param_bounds(&scope, &gens, &i.generics);
        let self_ty = resolve::resolve_type(&self.p, self.diags, &scope, &i.self_ty, TyPos::Normal);
        self.p.impls[id.index()].self_ty = self_ty;
        scope.self_ty = Some(self_ty);
        if let Some(t) = &i.trait_ {
            let r = resolve::resolve_trait_ref(&self.p, self.diags, &scope, t);
            self.p.impls[id.index()].trait_ref = r;
        }
        let mut assoc = BTreeMap::new();
        for member in &i.members {
            if let ast::ImplMemberKind::Type { name, ty } = &member.kind {
                let t = resolve::resolve_type(&self.p, self.diags, &scope, ty, TyPos::Normal);
                if assoc.insert(name.name.clone(), t).is_some() {
                    self.err(Diagnostic::new(
                        codes::E0201,
                        name.span,
                        format!("`{}` is defined twice in this impl", name.name),
                    ));
                }
            }
        }
        self.p.impls[id.index()].assoc_types = assoc;
    }

    fn fn_bounds(&mut self, m: ModuleId, id: FnId, f: &ast::FnDecl, outer: &[(String, ParamId)]) {
        let gens = self.p.func(id).generics.clone();
        let mut scope = Scope::new(m);
        scope.params = outer.to_vec();
        scope.push_params(&self.p, &gens);
        if let FnOwner::Impl(i) = self.p.func(id).owner {
            scope.self_ty = Some(self.p.impl_(i).self_ty);
        } else if let FnOwner::Trait(t) = self.p.func(id).owner {
            scope.self_ty = Some(self.p.types.param(self.p.trait_(t).self_param));
        }
        self.set_param_bounds(&scope, &gens, &f.generics);
    }

    fn fn_signature(&mut self, outer: &Scope, id: FnId, f: &ast::FnDecl) {
        let mut scope = outer.clone();
        let gens = self.p.func(id).generics.clone();
        scope.push_params(&self.p, &gens);
        let mut implicit = resolve::ImplicitParams::new(&self.p);
        let mut params: Vec<ParamSig> = Vec::new();
        let owner = self.p.func(id).owner;
        for (i, p) in f.params.iter().enumerate() {
            match p {
                ast::Param::SelfParam { mode, span } => {
                    if owner == FnOwner::Free || i != 0 {
                        self.err(Diagnostic::new(
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
                        mode: (*mode).into(),
                        ty,
                        default: None,
                        span: *span,
                        is_self: true,
                    });
                }
                ast::Param::Named { name, mode, ty, default, span } => {
                    if let Some(prev) = params.iter().find(|q| q.name == name.name) {
                        let ps = prev.span;
                        self.err(
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
                    params.push(ParamSig {
                        name: name.name.clone(),
                        mode: (*mode).into(),
                        ty: t,
                        default: default.clone(),
                        span: *span,
                        is_self: false,
                    });
                }
            }
        }
        // The parameters `x: Trait` introduced, numbered as `ImplicitParams` promised.
        let implicit_ids = implicit.ids();
        for def in implicit.defs {
            self.p.new_param(def);
        }
        let implicit = implicit_ids;
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
                let mode = match r.mode {
                    ast::RetMode::Owned => RetMode::Owned,
                    ast::RetMode::Borrow => RetMode::Borrow,
                    ast::RetMode::Mut => RetMode::Mut,
                };
                (t, mode)
            }
            None => (self.p.types.unit, RetMode::Owned),
        };
        let ret = if opaque.is_some() {
            // The return type names traits: it's this function's hidden concrete type.
            let all = self.p.fn_all_generics(id);
            let mut args: Vec<TyId> = all.iter().map(|&g| self.p.types.param(g)).collect();
            args.extend(implicit.iter().map(|&g| self.p.types.param(g)));
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
        let n = self.p.adts.len();
        let field_tys = |p: &Program, a: usize| -> Vec<(TyId, Span)> {
            match &p.adts[a].kind {
                AdtKind::Struct(fs) => fs.iter().map(|f| (f.ty, f.span)).collect(),
                AdtKind::Enum(vs) => {
                    vs.iter().flat_map(|v| v.fields.iter().map(|f| (f.ty, f.span))).collect()
                }
            }
        };
        // The ADTs a type holds by value, given which generic parameters each ADT holds.
        fn held(p: &Program, holds: &[Vec<bool>], t: TyId, out: &mut Vec<TyKind>) {
            match p.types.kind(t) {
                TyKind::Adt(b, args) => {
                    out.push(TyKind::Adt(*b, Vec::new()));
                    for (i, &arg) in args.iter().enumerate() {
                        if holds[b.index()].get(i).copied().unwrap_or(true) {
                            held(p, holds, arg, out);
                        }
                    }
                }
                TyKind::Param(q) => out.push(TyKind::Param(*q)),
                TyKind::Tuple(ts) => ts.iter().for_each(|&x| held(p, holds, x, out)),
                TyKind::Array(e, _) => held(p, holds, *e, out),
                _ => {}
            }
        }
        // Which of its generic parameters each ADT holds by value: a fixpoint.
        let mut holds: Vec<Vec<bool>> =
            self.p.adts.iter().map(|a| vec![false; a.generics.len()]).collect();
        loop {
            let mut changed = false;
            for a in 0..n {
                for (t, _) in field_tys(&self.p, a) {
                    let mut out = Vec::new();
                    held(&self.p, &holds, t, &mut out);
                    for k in out {
                        if let TyKind::Param(q) = k
                            && let Some(i) = self.p.adts[a].generics.iter().position(|&g| g == q)
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
        // Edges: the ADTs each field holds, with the field.
        let edges: Vec<Vec<(usize, usize)>> = (0..n)
            .map(|a| {
                let mut e = Vec::new();
                for (fi, (t, _)) in field_tys(&self.p, a).into_iter().enumerate() {
                    let mut out = Vec::new();
                    held(&self.p, &holds, t, &mut out);
                    for k in out {
                        if let TyKind::Adt(b, _) = k {
                            e.push((b.index(), fi));
                        }
                    }
                }
                e
            })
            .collect();
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            New,
            Active,
            Done,
        }
        let mut mark = vec![Mark::New; n];
        let mut broken: Vec<(usize, usize)> = Vec::new();
        for root in 0..n {
            if mark[root] != Mark::New {
                continue;
            }
            // (adt, next edge to follow)
            let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
            mark[root] = Mark::Active;
            while let Some(&mut (a, ref mut next)) = stack.last_mut() {
                let Some(&(b, fi)) = edges[a].get(*next) else {
                    mark[a] = Mark::Done;
                    stack.pop();
                    continue;
                };
                *next += 1;
                match mark[b] {
                    Mark::New => {
                        mark[b] = Mark::Active;
                        stack.push((b, 0));
                    }
                    Mark::Active => {
                        let at = stack.iter().position(|&(x, _)| x == b).unwrap_or(0);
                        let cycle: Vec<usize> = stack[at..].iter().map(|&(x, _)| x).collect();
                        self.report_recursive(&cycle, a, field_tys(&self.p, a)[fi].1);
                        broken.push((a, fi));
                    }
                    Mark::Done => {}
                }
            }
        }
        let error = self.p.types.error;
        for (a, fi) in broken {
            match &mut self.p.adts[a].kind {
                AdtKind::Struct(fs) => fs[fi].ty = error,
                AdtKind::Enum(vs) => {
                    if let Some(f) = vs.iter_mut().flat_map(|v| v.fields.iter_mut()).nth(fi) {
                        f.ty = error;
                    }
                }
            }
        }
    }

    fn report_recursive(&mut self, cycle: &[usize], last: usize, field_span: Span) {
        let first = &self.p.adts[cycle[0]];
        let msg = if cycle.len() == 1 {
            format!(
                "`{}` holds a `{}` inside itself, so it would be infinitely large",
                first.name, first.name
            )
        } else {
            let names: Vec<String> =
                cycle.iter().map(|&a| format!("`{}`", self.p.adts[a].name)).collect();
            format!("{} hold each other, so they would be infinitely large", names.join(", "))
        };
        let d = Diagnostic::new(codes::E0318, first.name_span, msg)
            .with_secondary(
                field_span,
                format!("this field of `{}` closes the cycle", self.p.adts[last].name),
            )
            .with_note("a value holds its fields directly; there are no pointers in tier 0")
            .with_help("hold an index into an array instead");
        self.err(d);
    }

    fn check_impls(&mut self) {
        // Opting in to a trait in a declaration implies an impl of it.
        for a in 0..self.p.adts.len() {
            let opt_in = self.p.adts[a].opt_in.clone();
            for (r, span) in opt_in {
                let gens = self.p.adts[a].generics.clone();
                let args = gens.iter().map(|&g| self.p.types.param(g)).collect();
                let self_ty = self.p.types.adt(AdtId(a as u32), args);
                let tr = self.p.trait_(r.trait_).clone();
                if !matches!(tr.lang, Some(Lang::Copy | Lang::Clone | Lang::GpuData)) {
                    let required: Vec<String> = tr
                        .methods
                        .iter()
                        .filter(|&&f| self.p.func(f).body.is_none())
                        .map(|&f| format!("`fn {}`", self.p.func(f).name))
                        .chain(tr.assoc_types.iter().map(|t| format!("`type {}`", t.name)))
                        .collect();
                    if !required.is_empty() {
                        self.err(
                            Diagnostic::new(
                                codes::E0401,
                                span,
                                format!(
                                    "`{}` can't opt in to `{}`: the trait requires {}",
                                    self.p.adts[a].name,
                                    tr.name,
                                    required.join(", ")
                                ),
                            )
                            .with_help(format!(
                                "write `impl {} for {} {{ ... }}` instead",
                                tr.name, self.p.adts[a].name
                            )),
                        );
                        continue;
                    }
                }
                self.p.impls.push(ImplDef {
                    module: self.p.adts[a].module,
                    generics: gens,
                    trait_ref: Some(r),
                    self_ty,
                    assoc_types: BTreeMap::new(),
                    methods: Vec::new(),
                    span,
                    from_opt_in: true,
                });
            }
        }
        for i in 0..self.p.impls.len() {
            let id = ImplId(i as u32);
            let imp = self.p.impls[i].clone();
            match &imp.trait_ref {
                Some(r) => self.p.impls_of_trait.entry(r.trait_).or_default().push(id),
                None => match self.p.types.kind(imp.self_ty).clone() {
                    TyKind::Adt(a, _) => self.p.inherent_impls.entry(a).or_default().push(id),
                    TyKind::Error => {}
                    _ => {
                        self.err(
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
        for i in 0..self.p.impls.len() {
            self.check_impl(ImplId(i as u32));
        }
        self.check_overlap();
    }

    fn check_impl(&mut self, id: ImplId) {
        let imp = self.p.impl_(id).clone();
        let Some(r) = imp.trait_ref.clone() else {
            // Inherent: methods must have distinct names, and the type must be local.
            if let TyKind::Adt(a, _) = *self.p.types.kind(imp.self_ty) {
                let adt_mod = self.p.adt(a).module;
                if self.p.is_std(adt_mod) && !self.p.is_std(imp.module) {
                    self.err(
                        Diagnostic::new(
                            codes::E0404,
                            imp.span,
                            format!(
                                "`{}` is defined in std, so only std can add methods to it",
                                self.p.adt(a).name
                            ),
                        )
                        .with_help("define a trait with the methods and implement it for the type"),
                    );
                }
            }
            let mut seen: Vec<(String, Span)> = Vec::new();
            for &f in &imp.methods {
                let d = self.p.func(f);
                if let Some((_, ps)) = seen.iter().find(|(n, _)| *n == d.name) {
                    let (ps, ns, name) = (*ps, d.name_span, d.name.clone());
                    self.err(
                        Diagnostic::new(codes::E0201, ns, format!("`{name}` is defined twice"))
                            .with_secondary(ps, "first defined here"),
                    );
                } else {
                    seen.push((d.name.clone(), d.name_span));
                }
            }
            return;
        };
        let tr = self.p.trait_(r.trait_).clone();
        let structural = matches!(tr.lang, Some(Lang::Copy | Lang::Clone | Lang::GpuData));
        // Builtin traits are structural: only by opting in.
        if structural && !imp.from_opt_in {
            self.err(
                Diagnostic::new(
                    codes::E0403,
                    imp.span,
                    format!("`{}` is implemented by opting in, not with an impl", tr.name),
                )
                .with_help(format!("declare the type as `struct Name: {} {{ ... }}`", tr.name)),
            );
            return;
        }
        if structural {
            self.check_structural_opt_in(id, &tr);
            return;
        }
        // Orphan rule (D-071): the trait or the type must be this package's.
        let trait_local = self.p.is_std(tr.module) == self.p.is_std(imp.module);
        let type_local = match *self.p.types.kind(imp.self_ty) {
            TyKind::Adt(a, _) => self.p.is_std(self.p.adt(a).module) == self.p.is_std(imp.module),
            _ => false,
        };
        if !trait_local && !type_local {
            self.err(
                Diagnostic::new(
                    codes::E0404,
                    imp.span,
                    format!(
                        "this package can't implement std's `{}` for std's `{}`",
                        tr.name,
                        self.p.display_ty(imp.self_ty)
                    ),
                )
                .with_note("an impl lives with its trait or with its type (D-071)")
                .with_help("wrap the type in a struct of your own, or define your own trait"),
            );
        }
        // Every required item, nothing extra, and matching signatures.
        let trait_subst = {
            let mut s = Subst::from_pairs(&tr.generics, &r.args);
            s.insert(tr.self_param, imp.self_ty);
            s
        };
        for at in &tr.assoc_types {
            if !imp.assoc_types.contains_key(&at.name) && !imp.from_opt_in {
                self.err(
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
                self.err(Diagnostic::new(
                    codes::E0402,
                    imp.span,
                    format!("`{}` has no associated type `{name}`", tr.name),
                ));
            }
        }
        for &tm in &tr.methods {
            let tdef = self.p.func(tm).clone();
            let found = imp.methods.iter().copied().find(|&f| self.p.func(f).name == tdef.name);
            match found {
                None if tdef.body.is_none() && !imp.from_opt_in => {
                    self.err(
                        Diagnostic::new(
                            codes::E0401,
                            imp.span,
                            format!("this impl of `{}` is missing `fn {}`", tr.name, tdef.name),
                        )
                        .with_secondary(tdef.sig_span, "declared here"),
                    );
                }
                None => {}
                Some(f) => self.compare_signatures(f, tm, &trait_subst, &imp),
            }
        }
        for &f in &imp.methods {
            let name = self.p.func(f).name.clone();
            if !tr.methods.iter().any(|&m| self.p.func(m).name == name) {
                let ns = self.p.func(f).name_span;
                self.err(
                    Diagnostic::new(
                        codes::E0402,
                        ns,
                        format!("`{}` has no method `{name}`", tr.name),
                    )
                    .with_help("move it to an inherent `impl` block"),
                );
            }
        }
    }

    /// An impl method's signature must be the trait's, with `Self` and the trait's parameters
    /// substituted.
    fn compare_signatures(
        &mut self,
        impl_fn: FnId,
        trait_fn: FnId,
        trait_subst: &Subst,
        imp: &ImplDef,
    ) {
        let a = self.p.func(impl_fn).clone();
        let b = self.p.func(trait_fn).clone();
        let mut subst = trait_subst.clone();
        let mismatch = |this: &mut Self, what: String| {
            this.err(
                Diagnostic::new(
                    codes::E0405,
                    a.sig_span,
                    format!("`{}` doesn't match the trait's signature: {what}", a.name),
                )
                .with_secondary(b.sig_span, "the trait declares it here"),
            );
        };
        if a.generics.len() != b.generics.len() {
            return mismatch(
                self,
                format!(
                    "it has {} generic parameters, the trait's has {}",
                    a.generics.len(),
                    b.generics.len()
                ),
            );
        }
        for (&tp, &ip) in b.generics.iter().zip(&a.generics) {
            let t = self.p.types.param(ip);
            subst.insert(tp, t);
        }
        if a.params.len() != b.params.len() {
            return mismatch(
                self,
                format!(
                    "it takes {} parameters, the trait's takes {}",
                    a.params.len(),
                    b.params.len()
                ),
            );
        }
        for (pa, pb) in a.params.iter().zip(&b.params) {
            if pa.mode != pb.mode {
                return mismatch(
                    self,
                    format!(
                        "`{}` is `{}` here but `{}` in the trait",
                        pa.name,
                        pa.mode.keyword(),
                        pb.mode.keyword()
                    ),
                );
            }
            let expected = self.p.types.subst(pb.ty, &subst);
            let expected = crate::traits::normalize(&self.p, expected, Some(imp));
            if pa.ty != expected && !pa.is_self {
                let (x, y) = (self.p.display_ty(pa.ty), self.p.display_ty(expected));
                return mismatch(
                    self,
                    format!("`{}` has type `{x}`, but the trait says `{y}`", pa.name),
                );
            }
        }
        if a.ret_mode != b.ret_mode {
            return mismatch(self, "the return mode differs".into());
        }
        if b.opaque.is_none() && a.opaque.is_none() {
            let expected = self.p.types.subst(b.ret, &subst);
            let expected = crate::traits::normalize(&self.p, expected, Some(imp));
            if a.ret != expected {
                let (x, y) = (self.p.display_ty(a.ret), self.p.display_ty(expected));
                mismatch(self, format!("it returns `{x}`, but the trait says `{y}`"));
            }
        }
    }

    /// `Copy`, `Clone` and `GpuData` are implemented structurally: every field must have them
    /// too (field types that use the type's parameters make the impl conditional).
    fn check_structural_opt_in(&mut self, id: ImplId, tr: &TraitDef) {
        let imp = self.p.impl_(id).clone();
        let TyKind::Adt(a, _) = *self.p.types.kind(imp.self_ty) else { return };
        let adt = self.p.adt(a).clone();
        let lang = tr.lang;
        let mut fields: Vec<&FieldDef> = adt.fields().iter().collect();
        for v in adt.variants() {
            fields.extend(v.fields.iter());
        }
        if lang == Some(Lang::Copy)
            && !adt.opt_in.iter().any(|(r, _)| self.p.is_lang_trait(r.trait_, Lang::Clone))
        {
            self.err(
                Diagnostic::new(
                    codes::E0407,
                    imp.span,
                    format!("`{}` opts in to `Copy` but not `Clone`", adt.name),
                )
                .with_note("every `Copy` type is also `Clone`")
                .with_fix(
                    "opt in to `Clone` too",
                    imp.span.shrink_to_end(),
                    " + Clone",
                ),
            );
        }
        if lang == Some(Lang::GpuData) && adt.is_enum() {
            self.err(
                Diagnostic::new(
                    codes::E0407,
                    imp.span,
                    format!("the enum `{}` can't be `GpuData`", adt.name),
                )
                .with_note("WGSL has no sum types; use a struct with a `u32` tag"),
            );
            return;
        }
        for f in fields {
            if self.p.types.has_params(f.ty) {
                continue; // conditional on the arguments; checked where it's used
            }
            let ok = crate::traits::implements_builtin(&self.p, f.ty, lang.unwrap_or(Lang::Clone));
            if !ok {
                let why = match lang {
                    Some(Lang::GpuData) => {
                        "WGSL can't hold it in a buffer (bool, 64-bit and 8/16-bit types can't cross to the GPU)"
                    }
                    _ => "it isn't",
                };
                self.err(
                    Diagnostic::new(
                        codes::E0407,
                        f.span,
                        format!(
                            "`{}` can't be `{}`: the field `{}` has type `{}`, and {why}",
                            adt.name,
                            tr.name,
                            f.name,
                            self.p.display_ty(f.ty)
                        ),
                    )
                    .with_secondary(imp.span, format!("`{}` opted in here", tr.name)),
                );
            }
        }
    }

    /// Two impls of one trait whose self types could be the same type conflict (E0403).
    fn check_overlap(&mut self) {
        let by_trait: Vec<(TraitId, Vec<ImplId>)> =
            self.p.impls_of_trait.iter().map(|(t, v)| (*t, v.clone())).collect();
        for (_, impls) in by_trait {
            for i in 0..impls.len() {
                for j in i + 1..impls.len() {
                    let (a, b) = (self.p.impl_(impls[i]).clone(), self.p.impl_(impls[j]).clone());
                    let same_args = match (&a.trait_ref, &b.trait_ref) {
                        (Some(x), Some(y)) => {
                            x.args.len() == y.args.len()
                                && crate::traits::could_unify_all(&self.p, &x.args, &y.args)
                        }
                        _ => false,
                    };
                    if same_args && crate::traits::could_unify(&self.p, a.self_ty, b.self_ty) {
                        let name = a
                            .trait_ref
                            .as_ref()
                            .map(|r| self.p.trait_(r.trait_).name.clone())
                            .unwrap_or_default();
                        self.err(
                            Diagnostic::new(
                                codes::E0403,
                                b.span,
                                format!(
                                    "this impl of `{name}` for `{}` conflicts with another",
                                    self.p.display_ty(b.self_ty)
                                ),
                            )
                            .with_secondary(a.span, "the other impl"),
                        );
                    }
                }
            }
        }
    }
}
