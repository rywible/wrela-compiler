//! Collects a program's definitions from its parsed files: the module tree, every item, `use`
//! imports, and every signature (generics, fields, variants, traits, impls, functions).

use crate::defs::*;
use crate::graph::{Visit, dfs_cycles};
use crate::program::{LangRes, Program};
use crate::resolve::{self, Scope, TyPos};
use crate::ty::*;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

/// One parsed source file and the module it is.
pub struct SourceUnit {
    /// The module path: `["shapes", "blob"]`, or `["std", "field"]`.
    pub path: Vec<String>,
    pub ast: ast::File,
    pub is_std: bool,
    /// Where its syntax errors are: code around them may be missing parts.
    pub syntax_errors: Vec<Span>,
}

struct PendingUse {
    module: ModuleId,
    path: Vec<ast::Ident>,
    alias: ast::Ident,
    public: bool,
    span: Span,
}

/// What `collect` remembers between its passes about each AST item.
enum PendingItem<'u> {
    Fn(FnId, &'u ast::FnDecl),
    Adt(AdtId, &'u ast::Item),
    Trait(TraitId, &'u ast::TraitDecl, Vec<(FnId, &'u ast::FnDecl)>),
    Impl(ImplId, &'u ast::ImplDecl, Vec<(FnId, &'u ast::FnDecl)>),
    Const(ConstId, &'u ast::ConstDecl),
}

pub fn collect(units: Vec<SourceUnit>, diags: &mut Vec<Diagnostic>) -> Program {
    let mut c = Collector { p: Program::default(), diags, pending: Vec::new(), uses: Vec::new() };
    c.p.syntax_errors = units.iter().flat_map(|u| u.syntax_errors.iter().copied()).collect();
    let modules = c.build_modules(&units);
    c.declare_items(&units, &modules);
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
}

impl<'d, 'u> Collector<'d, 'u> {
    // ---- modules ---------------------------------------------------------------------------

    fn new_module(&mut self, path: Vec<String>, is_std: bool) -> ModuleId {
        self.p.modules.push(Module {
            path,
            children: BTreeMap::new(),
            scope: BTreeMap::new(),
            broken: BTreeSet::new(),
            is_std,
        });
        ModuleId(self.p.modules.len() as u32 - 1)
    }

    /// Builds the module tree; returns the module each unit is.
    fn build_modules(&mut self, units: &[SourceUnit]) -> Vec<ModuleId> {
        let pkg = self.new_module(Vec::new(), false);
        let std = self.new_module(vec!["std".into()], true);
        self.p.package_root = Some(pkg);
        self.p.std_root = Some(std);
        let mut modules = Vec::new();
        let mut named_std = false;
        for u in units {
            if !u.is_std && u.path.first().is_some_and(|s| s == "std") && !named_std {
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
            if !u.is_std && u.path == ["main"] {
                self.p.main = Some(m);
            }
            modules.push(m);
        }
        modules
    }

    fn bind(&mut self, m: ModuleId, name: &ast::Ident, res: Res, public: bool) {
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
        scope.insert(name.name.clone(), Binding { res, public, span: name.span });
    }

    // ---- items -----------------------------------------------------------------------------

    fn declare_items(&mut self, units: &'u [SourceUnit], modules: &[ModuleId]) {
        for (u, &m) in units.iter().zip(modules) {
            for item in &u.ast.items {
                self.declare_item(m, item, u.is_std);
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

    fn declare_item(&mut self, m: ModuleId, item: &'u ast::Item, is_std: bool) {
        let public = item.vis.is_some();
        if !matches!(item.kind, ast::ItemKind::Fn(_)) {
            self.attrs_not_on_fn(&item.attrs);
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
                self.pending.push((m, PendingItem::Fn(id, f)));
            }
            ast::ItemKind::Struct(s) => {
                let kind = AdtKind::Struct(Vec::new());
                self.declare_adt(m, item, &s.name, &s.generics, kind, public);
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
                self.declare_adt(m, item, &e.name, &e.generics, kind, public);
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
                        let fid =
                            self.new_fn(m, FnOwner::Trait(id), f, true, &member.attrs, is_std);
                        methods.push((fid, f));
                    }
                }
                self.p.traits[id.index()].methods = methods.iter().map(|(f, _)| *f).collect();
                self.bind(m, &t.name, Res::Trait(id), public);
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
                        let public = member.vis.is_some() || i.trait_.is_some();
                        let fid =
                            self.new_fn(m, FnOwner::Impl(id), f, public, &member.attrs, is_std);
                        methods.push((fid, f));
                    }
                }
                self.p.impls[id.index()].methods = methods.iter().map(|(f, _)| *f).collect();
                self.pending.push((m, PendingItem::Impl(id, i, methods)));
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
                self.pending.push((m, PendingItem::Const(id, c)));
            }
            ast::ItemKind::Use(u) => self.flatten_use(m, u, Vec::new(), public),
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

    /// Declares a struct or enum; its fields or variants come with the signatures.
    fn declare_adt(
        &mut self,
        m: ModuleId,
        item: &'u ast::Item,
        name: &ast::Ident,
        generics: &[ast::GenericParam],
        kind: AdtKind,
        public: bool,
    ) {
        let generics = self.declare_generics(generics);
        let id = AdtId(self.p.adts.len() as u32);
        self.p.adts.push(AdtDef {
            name: name.name.clone(),
            module: m,
            generics,
            kind,
            opt_in: Vec::new(),
            public,
            span: item.span,
            name_span: name.span,
            lang: None,
        });
        self.bind(m, name, Res::Adt(id), public);
        self.pending.push((m, PendingItem::Adt(id, item)));
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
                    self.diags.push(
                        Diagnostic::new(codes::E0903, a.span, format!("`@{name}` is tier 1; it comes in milestone 2 or 3"))
                            .with_note("tier 0 has `@compute`, `@vertex`, `@fragment` and `@gpu` (language.md §9)"),
                    );
                }
                "audio" => {
                    self.diags
                        .push(Diagnostic::new(codes::E0903, a.span, "`@audio` is tier 2").with_note(
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
                    self.diags.push(d);
                }
            }
        }
        out
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
            // The names the imports still waiting will bind, and where.
            let binds: Vec<(ModuleId, String)> =
                pending.iter().map(|u| (u.module, u.alias.name.clone())).collect();
            // Each import still waiting, with the name it waits for and where.
            let mut still = Vec::new();
            for (i, u) in pending.into_iter().enumerate() {
                // A path's first segment is a name in the module's scope before a top-level
                // module or a global: while another import may still bind it, wait for that
                // one, so the order of the `use` lines doesn't matter.
                let first = &u.path[0].name;
                let bound_later = first != "std"
                    && !self.p.module(u.module).scope.contains_key(first)
                    && binds
                        .iter()
                        .enumerate()
                        .any(|(j, (m, n))| j != i && *m == u.module && n == first);
                if bound_later {
                    let (m, name) = (u.module, first.clone());
                    still.push((u, m, name));
                    continue;
                }
                match resolve::resolve_module_path(&self.p, u.module, &u.path) {
                    resolve::PathLookup::Found(res) => {
                        self.bind(u.module, &u.alias, res, u.public);
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
    }

    // ---- signatures ------------------------------------------------------------------------

    fn resolve_signatures(&mut self) {
        let pending = std::mem::take(&mut self.pending);
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
                PendingItem::Trait(id, _, methods) => {
                    let scope = self.trait_scope(*m, *id);
                    self.method_signatures(&scope, methods);
                }
                PendingItem::Impl(id, _, methods) => {
                    let scope = self.impl_scope(*m, *id);
                    self.method_signatures(&scope, methods);
                }
                PendingItem::Fn(id, f) => self.fn_signature(&Scope::new(*m), *id, f),
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
                PendingItem::Fn(id, f) => self.fn_bounds(&Scope::new(*m), *id, f),
                PendingItem::Const(..) => {}
            }
        }
        supertraits
    }

    /// What's in scope in a trait's items: `Self` and the trait's parameters.
    fn trait_scope(&self, m: ModuleId, id: TraitId) -> Scope {
        let tr = self.p.trait_(id);
        let mut scope = Scope::new(m);
        scope.self_ty = Some(self.p.types.param(tr.self_param));
        scope.push_params(&self.p, &tr.generics);
        scope
    }

    /// What's in scope in an impl's items: its parameters, and `Self`, the type it's for.
    fn impl_scope(&self, m: ModuleId, id: ImplId) -> Scope {
        let imp = self.p.impl_(id);
        let mut scope = Scope::new(m);
        scope.self_ty = Some(imp.self_ty);
        scope.impl_ = Some(id);
        scope.push_params(&self.p, &imp.generics);
        scope
    }

    /// What's in scope in a struct's or an enum's declaration: its parameters. Its fields and
    /// variants also see `Self`.
    fn adt_scope(&self, m: ModuleId, id: AdtId) -> Scope {
        let mut scope = Scope::new(m);
        scope.push_params(&self.p, &self.p.adt(id).generics);
        scope
    }

    /// The bounds and signatures of a trait's or an impl's methods, in the owner's scope.
    fn method_signatures(&mut self, scope: &Scope, methods: &[(FnId, &ast::FnDecl)]) {
        for &(fid, f) in methods {
            self.fn_bounds(scope, fid, f);
            self.fn_signature(scope, fid, f);
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
        let scope = self.trait_scope(m, id);
        self.set_param_bounds(&scope, &gens, &t.generics);
        let mut supers = Vec::new();
        let mut edges = Vec::new();
        for b in &t.supertraits {
            if let Some(r) = resolve::resolve_trait_ref(&self.p, self.diags, &scope, b) {
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
            let ty = resolve::resolve_type(&self.p, self.diags, scope, &f.ty, TyPos::Normal);
            let default = if allow_default { f.default.clone() } else { None };
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
        scope.impl_ = Some(id);
        if let Some(t) = &i.trait_ {
            let r = resolve::resolve_trait_ref(&self.p, self.diags, &scope, t);
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

    fn fn_bounds(&mut self, outer: &Scope, id: FnId, f: &ast::FnDecl) {
        let gens = self.p.func(id).generics.clone();
        let mut scope = outer.clone();
        scope.push_params(&self.p, &gens);
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
                    params.push(ParamSig {
                        name: name.name.clone(),
                        mode: *mode,
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
                (t, r.mode)
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
                    let is_projection = |k: &TyKind| matches!(k, TyKind::Projection { .. });
                    for f in adt.all_fields() {
                        if p.types.any(f.ty, &mut { is_projection }) {
                            let ty = crate::traits::normalize(p, p.types.subst(f.ty, &subst), None);
                            held_in(p, holds, ty, out, depth + 1);
                        }
                    }
                }
                TyKind::Param(q) => out.push(Held::Param(*q)),
                TyKind::Tuple(ts) => ts.iter().for_each(|&x| held_in(p, holds, x, out, depth)),
                TyKind::Array(e, _) => held_in(p, holds, *e, out, depth),
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
                if !tr.lang.is_some_and(Lang::is_structural) {
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
        self.p.impls.extend(implied);
        for (i, imp) in self.p.impls.iter().enumerate() {
            let id = ImplId(i as u32);
            match &imp.trait_ref {
                Some(r) => self.p.impls_of_trait.entry(r.trait_).or_default().push(id),
                None => match *self.p.types.kind(imp.self_ty) {
                    TyKind::Adt(a, _) => self.p.inherent_impls.entry(a).or_default().push(id),
                    TyKind::Error => {}
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
            if p.is_std(adt_mod) && !p.is_std(imp.module) {
                diags.push(
                    Diagnostic::new(
                        codes::E0404,
                        imp.span,
                        format!(
                            "`{}` is defined in std, so only std can add methods to it",
                            p.adt(a).name
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
    // Builtin traits are structural: only by opting in.
    if structural && !imp.from_opt_in {
        diags.push(
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
        check_structural_opt_in(p, diags, imp, tr);
        return;
    }
    // Orphan rule (D-071): the trait or the type must be this package's.
    let trait_local = p.is_std(tr.module) == p.is_std(imp.module);
    let type_local = match *p.types.kind(imp.self_ty) {
        TyKind::Adt(a, _) => p.is_std(p.adt(a).module) == p.is_std(imp.module),
        _ => false,
    };
    if !trait_local && !type_local {
        diags.push(
            Diagnostic::new(
                codes::E0404,
                imp.span,
                format!(
                    "this package can't implement std's `{}` for std's `{}`",
                    tr.name,
                    p.display_ty(imp.self_ty)
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
    // The type has the trait's supertraits too.
    for sup in &tr.supertraits {
        let sup = sup.subst(&p.types, &trait_subst);
        if !crate::traits::implements(p, imp.self_ty, &sup) {
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
    if lang == Some(Lang::Copy)
        && !adt.opt_in.iter().any(|(r, _)| p.is_lang_trait(r.trait_, Lang::Clone))
    {
        diags.push(
            Diagnostic::new(
                codes::E0407,
                imp.span,
                format!("`{}` opts in to `Copy` but not `Clone`", adt.name),
            )
            .with_note("every `Copy` type is also `Clone`")
            .with_fix("opt in to `Clone` too", imp.span.shrink_to_end(), " + Clone"),
        );
    }
    if lang == Some(Lang::GpuData) && adt.is_enum() {
        diags.push(
            Diagnostic::new(
                codes::E0407,
                imp.span,
                format!("the enum `{}` can't be `GpuData`", adt.name),
            )
            .with_note("WGSL has no sum types; use a struct with a `u32` tag"),
        );
        return;
    }
    for f in adt.all_fields() {
        if p.types.has_params(f.ty) {
            continue; // conditional on the arguments; checked where it's used
        }
        let ok = crate::traits::implements_builtin(p, f.ty, lang.unwrap_or(Lang::Clone));
        if !ok {
            let why = match lang {
                Some(Lang::GpuData) => {
                    "WGSL can't hold it in a buffer (bool, 64-bit and 8/16-bit types and empty arrays can't cross to the GPU)"
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

/// What a type is at its top, for grouping impls: two whose self types have different heads
/// can't overlap. `None` for a generic parameter, which could be any type.
fn ty_head(p: &Program, t: TyId) -> Option<(u8, u32)> {
    Some(match p.types.kind(t) {
        TyKind::Param(_) | TyKind::Projection { .. } | TyKind::Error => return None,
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
            let same_args = match (&a.trait_ref, &b.trait_ref) {
                (Some(x), Some(y)) => {
                    x.args.len() == y.args.len()
                        && crate::traits::could_unify_all(p, &x.args, &y.args)
                }
                _ => false,
            };
            if same_args && crate::traits::could_unify(p, a.self_ty, b.self_ty) {
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
