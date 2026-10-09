//! The whole program's definitions: every module, item and generic parameter, plus the type
//! interner. Built by `collect`, then read by the checker, the memory checker and lowering.

use crate::defs::*;
use crate::ty::*;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

#[derive(Debug, Default)]
pub struct Program {
    pub types: Types,
    pub modules: Vec<Module>,
    pub adts: Vec<AdtDef>,
    pub traits: Vec<TraitDef>,
    pub impls: Vec<ImplDef>,
    pub fns: Vec<FnDef>,
    pub consts: Vec<ConstDef>,
    pub aliases: Vec<AliasDef>,
    pub trait_sets: Vec<TraitSetDef>,
    pub params: Vec<ParamDef>,
    /// The root of the user's package and of std.
    pub package_root: Option<ModuleId>,
    pub std_root: Option<ModuleId>,
    /// The packages: std, the program's own, and its dependencies.
    pub packages: Vec<PackageDef>,
    /// The entry module (`main.wrela`), and its file.
    pub main: Option<ModuleId>,
    pub main_file: Option<wrela_diag::FileId>,
    /// Each file's text, for fixes that move what's written.
    pub texts: HashMap<wrela_diag::FileId, std::sync::Arc<str>>,
    pub lang: HashMap<Lang, LangRes>,
    /// Impls by trait, for trait solving.
    pub impls_of_trait: HashMap<TraitId, Vec<ImplId>>,
    /// Inherent impls by the ADT they're for.
    pub inherent_impls: HashMap<AdtId, Vec<ImplId>>,
    /// Std's inherent impls of built-in types (`impl str`).
    pub builtin_inherent: Vec<ImplId>,
    /// Each entry point's bound type (§12), `AdtDef::entry`'s other way.
    pub bound_types: HashMap<FnId, AdtId>,
    /// Each `Packed` struct's fields, in its word (§3). Its one real field is the word.
    pub packed: HashMap<AdtId, Vec<PackedField>>,
    /// The constants code names, where the name resolves: a literal one is folded before the
    /// memory IR, which so doesn't show its uses (W0008).
    pub(crate) used_consts: std::cell::RefCell<HashSet<ConstId>>,
    /// Whether dropping a type may do something: worked out once per type.
    pub(crate) drop_cache: std::cell::RefCell<HashMap<TyId, bool>>,
    /// Each closure's parameter and return types, in its owner's generic terms, for whether
    /// it fits a function-type bound (`impl<F: fn(vec3) -> f32> Tr for F`). Recorded when the
    /// closure is checked, and again when its body's types are final.
    pub(crate) closure_sigs: std::cell::RefCell<HashMap<ClosureRef, (Vec<TyId>, TyId)>>,
    /// Whether a type implements a structural trait (`Copy`, `Clone`, `GpuData`): worked out
    /// once per type, since struct types share their fields' types.
    pub(crate) builtin_impls: std::cell::RefCell<HashMap<(TyId, Lang), bool>>,
    /// Whether a type that declares a `@fieldwise` trait has it: its fields all do. Worked out
    /// once per type and trait.
    pub(crate) fieldwise_impls: std::cell::RefCell<HashMap<(TyId, TraitRef), bool>>,
    /// The solver's goals in the order it took them up, and where each derivation of a
    /// `@fieldwise` trait in progress started in that order: a goal met again after one
    /// started is met (a type that holds itself through a `Box` has the trait if the rest of
    /// its fields do).
    pub(crate) goal_order: std::cell::RefCell<GoalOrder>,
    /// The goals the trait solver is working on: an impl's bounds can lead back to one of
    /// them. The first one's size, while there are any.
    pub(crate) solving: std::cell::RefCell<(HashSet<(TyId, TraitRef)>, u32)>,
    /// Where the source's syntax errors are.
    pub syntax_errors: Vec<wrela_diag::Span>,
    /// The unit suffixes: `std::units`' constants by name (§5).
    pub units: HashMap<String, ConstId>,
}

/// The trait solver's goals in order, and where each derivation in progress started.
pub(crate) type GoalOrder = (Vec<(TyId, TraitRef)>, Vec<usize>);

/// Longer type names are cut here, so diagnostics stay readable.
const MAX_TYPE_TEXT: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LangRes {
    Adt(AdtId),
    Trait(TraitId),
    Fn(FnId),
}

impl Program {
    pub fn module(&self, m: ModuleId) -> &Module {
        &self.modules[m.index()]
    }
    pub fn adt(&self, a: AdtId) -> &AdtDef {
        &self.adts[a.index()]
    }
    /// What's written at `span`, if its file is known.
    pub fn text(&self, span: wrela_diag::Span) -> Option<&str> {
        self.texts.get(&span.file)?.get(span.start as usize..span.end as usize)
    }

    pub fn trait_(&self, t: TraitId) -> &TraitDef {
        &self.traits[t.index()]
    }
    pub fn impl_(&self, i: ImplId) -> &ImplDef {
        &self.impls[i.index()]
    }
    pub fn func(&self, f: FnId) -> &FnDef {
        &self.fns[f.index()]
    }
    pub fn const_(&self, c: ConstId) -> &ConstDef {
        &self.consts[c.index()]
    }
    pub fn param(&self, p: ParamId) -> &ParamDef {
        &self.params[p.index()]
    }

    /// The alias that names traits whose type `f`'s result is (§4), if one is.
    pub fn alias_defined_by(&self, f: FnId) -> Option<&AliasDef> {
        self.aliases.iter().find(|a| a.defined_by == Some(f))
    }

    pub fn lang_adt(&self, l: Lang) -> Option<AdtId> {
        match self.lang.get(&l) {
            Some(LangRes::Adt(a)) => Some(*a),
            _ => None,
        }
    }
    pub fn lang_trait(&self, l: Lang) -> Option<TraitId> {
        match self.lang.get(&l) {
            Some(LangRes::Trait(t)) => Some(*t),
            _ => None,
        }
    }
    pub fn lang_fn(&self, l: Lang) -> Option<FnId> {
        match self.lang.get(&l) {
            Some(LangRes::Fn(f)) => Some(*f),
            _ => None,
        }
    }

    /// Whether `a` is the lang item `l`.
    pub fn is_lang_adt(&self, a: AdtId, l: Lang) -> bool {
        self.adts[a.index()].lang == Some(l)
    }

    pub fn is_lang_trait(&self, t: TraitId, l: Lang) -> bool {
        self.traits[t.index()].lang == Some(l)
    }

    /// The message a library wrote for `ty` lacking `r` (§7): the trait's `@diagnostic`, else
    /// the type's, with `{Self}` replaced by the type and `{Trait}` by the trait.
    pub fn custom_message(&self, ty: TyId, r: &TraitRef) -> Option<String> {
        let from_type = match self.types.kind(ty) {
            TyKind::Adt(a, _) => self.adt(*a).diagnostic.clone(),
            _ => None,
        };
        let text = self.trait_(r.trait_).diagnostic.clone().or(from_type)?;
        Some(
            text.replace("{Self}", &self.display_ty(ty))
                .replace("{Trait}", &self.display_trait_ref(r)),
        )
    }

    /// The lang item a type is, if it's a lang struct or enum (with any arguments).
    pub fn lang_of_ty(&self, t: TyId) -> Option<Lang> {
        match self.types.kind(t) {
            TyKind::Adt(a, _) => self.adt(*a).lang,
            _ => None,
        }
    }

    /// Whether `t` is a borrow struct (§6.6).
    pub fn is_borrow_struct(&self, t: TyId) -> bool {
        matches!(self.types.kind(t), TyKind::Adt(a, _) if self.adt(*a).borrow)
    }

    /// The impls of a trait.
    pub fn impls_of(&self, t: TraitId) -> &[ImplId] {
        self.impls_of_trait.get(&t).map_or(&[], Vec::as_slice)
    }

    /// `a`'s inherent associated function or method named `name`: the first impl's that has one.
    pub fn inherent_method(&self, a: AdtId, name: &str) -> Option<FnId> {
        self.inherent_impls.get(&a).into_iter().flatten().find_map(|&i| {
            self.impl_(i).methods.iter().copied().find(|&f| self.func(f).name == name)
        })
    }

    pub fn new_param(&mut self, def: ParamDef) -> ParamId {
        self.params.push(def);
        ParamId(self.params.len() as u32 - 1)
    }

    /// Every generic parameter in scope for a function: its owner's, then its own.
    pub fn fn_all_generics(&self, f: FnId) -> Vec<ParamId> {
        let def = self.func(f);
        let mut out = match def.owner {
            FnOwner::Free | FnOwner::Const(_) => Vec::new(),
            FnOwner::Impl(i) => self.impl_(i).generics.clone(),
            FnOwner::Trait(t) => {
                let tr = self.trait_(t);
                let mut v = vec![tr.self_param];
                v.extend(&tr.generics);
                v
            }
        };
        out.extend(&def.generics);
        out
    }

    /// A function's qualified name for diagnostics: `Type::method` or `name`.
    pub fn fn_display_name(&self, f: FnId) -> String {
        let def = self.func(f);
        match def.owner {
            FnOwner::Free | FnOwner::Const(_) => def.name.clone(),
            FnOwner::Impl(i) => {
                let self_ty = self.impl_(i).self_ty;
                format!("{}::{}", self.display_ty(self_ty), def.name)
            }
            FnOwner::Trait(t) => format!("{}::{}", self.trait_(t).name, def.name),
        }
    }

    /// Notes that code names constant `c` (W0008).
    pub(crate) fn note_const_use(&self, c: ConstId) {
        self.used_consts.borrow_mut().insert(c);
    }

    /// Whether code names constant `c`.
    pub(crate) fn const_used(&self, c: ConstId) -> bool {
        self.used_consts.borrow().contains(&c)
    }

    /// A type as diagnostics show it; a long one is cut short.
    pub fn display_ty(&self, t: TyId) -> String {
        let mut s = String::new();
        self.write_ty(t, &mut s);
        if s.len() > MAX_TYPE_TEXT {
            let mut cut = MAX_TYPE_TEXT;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            s.push('…');
        }
        s
    }

    fn write_list(&self, ts: &[TyId], s: &mut String) {
        for (i, &t) in ts.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            self.write_ty(t, s);
            if s.len() > MAX_TYPE_TEXT {
                return;
            }
        }
    }

    fn write_ty(&self, t: TyId, s: &mut String) {
        if s.len() > MAX_TYPE_TEXT {
            return;
        }
        match self.types.kind(t) {
            TyKind::Bool => s.push_str("bool"),
            TyKind::Int(i) => s.push_str(i.name()),
            TyKind::Float(f) => s.push_str(f.name()),
            TyKind::Vec(e, n) => {
                let _ = write!(s, "vec{n}{}", e.suffix());
            }
            TyKind::Mat(c, r) if c == r => {
                let _ = write!(s, "mat{c}");
            }
            TyKind::Mat(c, r) => {
                let _ = write!(s, "mat{c}x{r}");
            }
            TyKind::Tuple(ts) => {
                s.push('(');
                self.write_list(ts, s);
                if ts.len() == 1 {
                    s.push(',');
                }
                s.push(')');
            }
            TyKind::Array(e, n) => {
                s.push('[');
                self.write_ty(*e, s);
                let _ = write!(s, "; {n}]");
            }
            TyKind::ArrayN(e, n) => {
                s.push('[');
                self.write_ty(*e, s);
                s.push_str("; ");
                self.write_ty(*n, s);
                s.push(']');
            }
            TyKind::ConstU32(n) => {
                let _ = write!(s, "{n}");
            }
            TyKind::Slice(e) => {
                s.push('[');
                self.write_ty(*e, s);
                s.push(']');
            }
            TyKind::Str => s.push_str("str"),
            // A job's value shows the job as it's written: `Job<realize<Coat, Fawn>>`.
            TyKind::Adt(a, args)
                if self.is_lang_adt(*a, Lang::Job)
                    && let Some(TyKind::FnDef(f, fargs)) =
                        args.first().map(|&t| self.types.kind(t)) =>
            {
                let _ = write!(s, "Job<{}", self.func(*f).name);
                if !fargs.is_empty() {
                    s.push('<');
                    self.write_list(fargs, s);
                    s.push('>');
                }
                s.push('>');
            }
            TyKind::Adt(a, args) => {
                s.push_str(&self.adt(*a).name);
                // A bound entry point's type shows as its entry point bound: `shade.bind(...)`.
                if !args.is_empty() && self.adt(*a).entry.is_none() {
                    s.push('<');
                    self.write_list(args, s);
                    s.push('>');
                }
            }
            TyKind::Param(p) => s.push_str(&self.param(*p).name),
            TyKind::Projection { self_ty, name, .. } => {
                self.write_ty(*self_ty, s);
                s.push_str("::");
                s.push_str(name);
            }
            // The type a function returns that's named by its traits: `impl Surface`.
            // An alias that names traits shows as its name.
            TyKind::Opaque(f, args)
                if args.is_empty()
                    && let Some(a) = self.alias_defined_by(*f) =>
            {
                s.push_str(&a.name);
            }
            TyKind::Opaque(f, _) => match &self.func(*f).opaque {
                Some(traits) if !traits.is_empty() => {
                    let names: Vec<String> =
                        traits.iter().map(|r| self.display_trait_ref(r)).collect();
                    let _ = write!(s, "impl {}", names.join(" + "));
                }
                _ => {
                    let _ = write!(s, "the type `{}` returns", self.func(*f).name);
                }
            },
            TyKind::FnPtr(ps, r, flags) => {
                for (name, has) in flags.attrs() {
                    if has {
                        let _ = write!(s, "@{name} ");
                    }
                }
                s.push_str("fn(");
                for (i, &t) in ps.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    match flags.mode(i) {
                        wrela_syntax::ast::Mode::Borrow => {}
                        m => {
                            s.push_str(m.keyword());
                            s.push(' ');
                        }
                    }
                    self.write_ty(t, s);
                }
                s.push(')');
                if *r != self.types.unit {
                    s.push_str(" -> ");
                    self.write_ty(*r, s);
                }
            }
            TyKind::Closure(..) => s.push_str("<closure>"),
            TyKind::FnDef(f, _) => {
                let _ = write!(s, "fn {}", self.func(*f).name);
            }
            TyKind::Var(_) => s.push('_'),
            TyKind::Never => s.push('!'),
            TyKind::Error => s.push_str("{error}"),
        }
    }

    pub fn display_trait_ref(&self, r: &TraitRef) -> String {
        let mut s = self.trait_(r.trait_).name.clone();
        if !r.args.is_empty() {
            s.push('<');
            s.push_str(&r.args.iter().map(|&a| self.display_ty(a)).collect::<Vec<_>>().join(", "));
            s.push('>');
        }
        s
    }

    /// The declared fields of a struct (`variant` is `None`) or of one of an enum's variants.
    pub fn adt_fields(&self, a: AdtId, variant: Option<u32>) -> &[FieldDef] {
        let def = self.adt(a);
        match variant {
            Some(v) => &def.variants()[v as usize].fields,
            None => def.fields(),
        }
    }

    /// The field types of a struct or of one of an enum's variants, in [`Self::adt_fields`]
    /// order, with the type's arguments substituted and projections on them resolved
    /// (`k: T::K` with `T = P` is `P`'s `K`).
    pub fn fields_of(&self, a: AdtId, args: &[TyId], variant: Option<u32>) -> Vec<TyId> {
        let subst = Subst::from_pairs(&self.adt(a).generics, args);
        self.adt_fields(a, variant).iter().map(|f| self.field_under(f.ty, &subst)).collect()
    }

    /// The parts of a plain aggregate of type `t`: a tuple's elements, or the fields of a
    /// struct that isn't a borrow struct or a type the language or std gives a meaning
    /// (`Text`, `Vec`, `Flat<T>`, ...). `None` for other types, and for one with no parts.
    pub fn plain_parts(&self, t: TyId) -> Option<Vec<TyId>> {
        let parts = match self.types.kind(t) {
            TyKind::Tuple(ts) => ts.clone(),
            TyKind::Adt(a, args)
                if self.lang_of_ty(t).is_none()
                    && !self.adt(*a).is_enum()
                    && !self.adt(*a).borrow =>
            {
                self.fields_of(*a, args, None)
            }
            _ => return None,
        };
        (!parts.is_empty()).then_some(parts)
    }

    /// The `variant` that `fields_of` takes for each of `a`'s field lists: each variant's
    /// index for an enum (none, for an enum without variants), and `None` for a struct.
    pub fn field_lists(&self, a: AdtId) -> Vec<Option<u32>> {
        let adt = self.adt(a);
        if adt.is_enum() {
            (0..adt.variants().len() as u32).map(Some).collect()
        } else {
            vec![None]
        }
    }

    /// The type of field `i` of a struct or of one of an enum's variants, as [`Self::fields_of`]
    /// gives it; `None` if there's no such field.
    pub fn field_ty(
        &self,
        a: AdtId,
        args: &[TyId],
        variant: Option<u32>,
        i: usize,
    ) -> Option<TyId> {
        let f = self.adt_fields(a, variant).get(i)?;
        Some(self.field_under(f.ty, &Subst::from_pairs(&self.adt(a).generics, args)))
    }

    fn field_under(&self, ty: TyId, subst: &Subst) -> TyId {
        let t = self.types.subst(ty, subst);
        // The declared type says whether there's a projection, cheaply: the arguments can be
        // large.
        if self.types.has_projections(ty) { crate::traits::normalize(self, t, None) } else { t }
    }

    /// A fix that opts a struct in to a trait in its declaration (`struct S: Trait`), for a
    /// diagnostic: where to insert, and what. `None` for std's types, and for generic ones
    /// (whose parameters sit between the name and the opt-in list).
    pub fn opt_in_fix(&self, a: AdtId, trait_name: &str) -> Option<(wrela_diag::Span, String)> {
        let adt = self.adt(a);
        if self.is_std(adt.module) || !adt.generics.is_empty() {
            return None;
        }
        Some(match adt.opt_in.last() {
            Some((_, span)) => (span.shrink_to_end(), format!(" + {trait_name}")),
            None => (adt.name_span.shrink_to_end(), format!(": {trait_name}")),
        })
    }

    /// Whether a module is part of std.
    /// Whether module `m` is std's: std's privileges come from its package (language.md §3).
    pub fn is_std(&self, m: ModuleId) -> bool {
        self.package_of(m).kind == PackageKind::Std
    }

    /// The package module `m` is in.
    pub fn package_of(&self, m: ModuleId) -> &PackageDef {
        &self.packages[self.modules[m.index()].package.index()]
    }

    /// Whether two modules are in one package.
    pub fn same_package(&self, a: ModuleId, b: ModuleId) -> bool {
        self.modules[a.index()].package == self.modules[b.index()].package
    }
}
