//! The whole program's definitions: every module, item and generic parameter, plus the type
//! interner. Built by `collect`, then read by the checker, the memory checker and lowering.

use crate::defs::*;
use crate::ty::*;
use std::collections::HashMap;

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
}

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

    pub fn display_ty(&self, t: TyId) -> String {
        display(&self.types, self, t)
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

    /// The fields of a struct type, with the type's arguments substituted.
    pub fn struct_fields(&self, adt: AdtId, args: &[TyId]) -> Vec<(String, TyId)> {
        let def = &self.adts[adt.index()];
        let subst = Subst::from_pairs(&def.generics, args);
        let fields: Vec<(String, TyId)> =
            def.fields().iter().map(|f| (f.name.clone(), f.ty)).collect();
        fields.into_iter().map(|(n, t)| (n, self.types.subst(t, &subst))).collect()
    }

    /// A variant's fields, substituted.
    pub fn variant_fields(&self, adt: AdtId, args: &[TyId], variant: usize) -> Vec<(String, TyId)> {
        let def = &self.adts[adt.index()];
        let subst = Subst::from_pairs(&def.generics, args);
        let fields: Vec<(String, TyId)> =
            def.variants()[variant].fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        fields.into_iter().map(|(n, t)| (n, self.types.subst(t, &subst))).collect()
    }

    /// Whether a module is part of std.
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

    pub fn is_std(&self, m: ModuleId) -> bool {
        self.modules[m.index()].is_std
    }
}

impl TyNames for Program {
    fn adt_name(&self, a: AdtId) -> String {
        self.adts[a.index()].name.clone()
    }
    fn param_name(&self, p: ParamId) -> String {
        self.params[p.index()].name.clone()
    }
    fn fn_name(&self, f: FnId) -> String {
        self.fns[f.index()].name.clone()
    }
    fn trait_name(&self, t: TraitId) -> String {
        self.traits[t.index()].name.clone()
    }
    fn opaque_name(&self, f: FnId) -> String {
        match &self.fns[f.index()].opaque {
            Some(traits) if !traits.is_empty() => {
                let names: Vec<String> = traits.iter().map(|r| self.display_trait_ref(r)).collect();
                format!("impl {}", names.join(" + "))
            }
            _ => format!("the type `{}` returns", self.fns[f.index()].name),
        }
    }
}
