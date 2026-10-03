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
    pub params: Vec<ParamDef>,
    /// The root of the user's package and of std.
    pub package_root: Option<ModuleId>,
    pub std_root: Option<ModuleId>,
    /// The entry module (`main.wrela`).
    pub main: Option<ModuleId>,
    pub lang: HashMap<Lang, LangRes>,
    /// Impls by trait, for trait solving.
    pub impls_of_trait: HashMap<TraitId, Vec<ImplId>>,
    /// Inherent impls by the ADT they're for.
    pub inherent_impls: HashMap<AdtId, Vec<ImplId>>,
    /// Whether a type implements a structural trait (`Copy`, `Clone`, `GpuData`): worked out
    /// once per type, since struct types share their fields' types.
    pub(crate) builtin_impls: std::cell::RefCell<HashMap<(TyId, Lang), bool>>,
    /// The goals the trait solver is working on: an impl's bounds can lead back to one of
    /// them. The first one's size, while there are any.
    pub(crate) solving: std::cell::RefCell<(HashSet<(TyId, TraitRef)>, u32)>,
    /// Where the source's syntax errors are.
    pub syntax_errors: Vec<wrela_diag::Span>,
}

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

    /// The lang item a type is, if it's a lang struct or enum (with any arguments).
    pub fn lang_of_ty(&self, t: TyId) -> Option<Lang> {
        match self.types.kind(t) {
            TyKind::Adt(a, _) => self.adt(*a).lang,
            _ => None,
        }
    }

    /// The impls of a trait.
    pub fn impls_of(&self, t: TraitId) -> &[ImplId] {
        self.impls_of_trait.get(&t).map_or(&[], Vec::as_slice)
    }

    pub fn new_param(&mut self, def: ParamDef) -> ParamId {
        self.params.push(def);
        ParamId(self.params.len() as u32 - 1)
    }

    /// Every generic parameter in scope for a function: its owner's, then its own.
    pub fn fn_all_generics(&self, f: FnId) -> Vec<ParamId> {
        let def = self.func(f);
        let mut out = match def.owner {
            FnOwner::Free => Vec::new(),
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
            FnOwner::Free => def.name.clone(),
            FnOwner::Impl(i) => {
                let self_ty = self.impl_(i).self_ty;
                format!("{}::{}", self.display_ty(self_ty), def.name)
            }
            FnOwner::Trait(t) => format!("{}::{}", self.trait_(t).name, def.name),
        }
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
            TyKind::Vec(n) => {
                let _ = write!(s, "vec{n}");
            }
            TyKind::Mat(n) => {
                let _ = write!(s, "mat{n}");
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
            TyKind::Slice(e) => {
                s.push('[');
                self.write_ty(*e, s);
                s.push(']');
            }
            TyKind::Adt(a, args) => {
                s.push_str(&self.adt(*a).name);
                if !args.is_empty() {
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
            TyKind::FnPtr(ps, r) => {
                s.push_str("fn(");
                self.write_list(ps, s);
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
    pub fn is_std(&self, m: ModuleId) -> bool {
        self.modules[m.index()].is_std
    }
}
