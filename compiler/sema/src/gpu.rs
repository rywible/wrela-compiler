//! GPU entry points' signatures (language.md §6.13, §12), checked where they're declared:
//! what each stage may take and return, and that a kernel's `mut` parameters are safe to share
//! across invocations. Lowering then only sees signatures it can map to WGSL.

use crate::defs::{Entry, Lang, Mode};
use crate::program::Program;
use crate::ty::{FnId, TyId, TyKind};
use wrela_diag::{Diagnostic, codes};

/// Every entry point's signature.
pub fn check_entries(p: &Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for i in 0..p.fns.len() {
        let f = FnId(i as u32);
        if let Some((entry, _)) = p.func(f).attrs.entry {
            check_entry(p, f, entry, &mut out);
        }
    }
    out
}

fn lang_of(p: &Program, t: TyId) -> Option<Lang> {
    match p.types.kind(t) {
        TyKind::Adt(a, _) => p.adt(*a).lang,
        _ => None,
    }
}

fn stage_name(e: Entry) -> &'static str {
    match e {
        Entry::Compute(_) => "a `@compute` kernel",
        Entry::Vertex => "a `@vertex` shader",
        Entry::Fragment => "a `@fragment` shader",
    }
}

fn check_entry(p: &Program, f: FnId, entry: Entry, out: &mut Vec<Diagnostic>) {
    let def = p.func(f).clone();
    let stage = stage_name(entry);
    let unit = p.types.unit;
    // What it returns.
    match entry {
        Entry::Compute(_) if def.ret != unit => out.push(
            Diagnostic::new(codes::E0602, def.sig_span, format!("{stage} doesn't return a value"))
                .with_help("write results through a `mut Slots<T>` parameter"),
        ),
        Entry::Vertex => {
            let ok = lang_of(p, def.ret) == Some(Lang::ClipPosition)
                || match p.types.kind(def.ret) {
                    TyKind::Adt(a, _) if !p.adt(*a).is_enum() => {
                        p.adt(*a)
                            .fields()
                            .iter()
                            .filter(|fd| lang_of(p, fd.ty) == Some(Lang::ClipPosition))
                            .count()
                            == 1
                    }
                    _ => false,
                };
            if !ok {
                out.push(
                    Diagnostic::new(
                        codes::E0602,
                        def.sig_span,
                        format!("{stage} returns its clip position"),
                    )
                    .with_help(
                        "return a `ClipPosition`, or a struct with one `ClipPosition` field and the \
                         values to pass to the fragment shader",
                    ),
                );
            }
        }
        Entry::Fragment if !matches!(p.types.kind(def.ret), TyKind::Vec(4)) => out.push(
            Diagnostic::new(codes::E0602, def.sig_span, format!("{stage} returns a `vec4` colour"))
                .with_note("it writes one colour to the screen"),
        ),
        _ => {}
    }
    // What it takes.
    for ps in &def.params {
        let lang = lang_of(p, ps.ty);
        let builtin_stage = match lang {
            Some(Lang::GlobalId | Lang::LocalId | Lang::WorkgroupId) => {
                Some(matches!(entry, Entry::Compute(_)))
            }
            Some(Lang::VertexIndex | Lang::InstanceIndex) => Some(entry == Entry::Vertex),
            Some(Lang::FragCoord) => Some(entry == Entry::Fragment),
            _ => None,
        };
        if builtin_stage == Some(false) {
            let ty = p.display_ty(ps.ty);
            out.push(Diagnostic::new(
                codes::E0602,
                ps.span,
                format!("`{ty}` isn't an input {stage} has"),
            ));
            continue;
        }
        let slots = lang == Some(Lang::Slots);
        match ps.mode {
            Mode::Take => out.push(
                Diagnostic::new(
                    codes::E0602,
                    ps.span,
                    format!("{stage} borrows its parameters; it can't take `{}`", ps.name),
                )
                .with_help("drop `take`"),
            ),
            Mode::Mut if !slots => out.push(
                Diagnostic::new(
                    codes::E0601,
                    ps.span,
                    format!(
                        "every invocation of {stage} would hold `mut {}` at once, so it can't be \
                         a plain `mut` parameter",
                        ps.name
                    ),
                )
                .with_note(
                    "a kernel's `mut` parameters must be safe to share across invocations \
                     (§6.13)",
                )
                .with_help("take a `mut Slots<T>`: each invocation writes only its own slot"),
            ),
            Mode::Borrow if slots => out.push(
                Diagnostic::new(
                    codes::E0602,
                    ps.span,
                    format!("`{}` is written through, so it's `mut Slots<T>`", ps.name),
                )
                .with_help("write `mut` before its type")
                .with_note("to read a buffer, take a run `[T]`"),
            ),
            _ => {}
        }
        // Data passed by value travels as a uniform, so its layout must be the GPU's.
        let varyings = matches!(p.types.kind(ps.ty), TyKind::Adt(a, _) if p.adt(*a).fields().iter().any(|fd| lang_of(p, fd.ty) == Some(Lang::ClipPosition)));
        let passed = builtin_stage.is_none()
            && !slots
            && !varyings
            && !matches!(p.types.kind(ps.ty), TyKind::Slice(_) | TyKind::Param(_))
            && !p.types.has_params(ps.ty);
        if passed && !crate::traits::implements_builtin(p, ps.ty, Lang::GpuData) {
            let ty = p.display_ty(ps.ty);
            let mut d = Diagnostic::new(
                codes::E0604,
                ps.span,
                format!(
                    "`{}` is a `{ty}`, which isn't `GpuData`, so it can't go to the GPU",
                    ps.name
                ),
            )
            .with_note(
                "data crossing to the GPU must be `GpuData`, so its layout is the same on both \
                 sides (§6.13)",
            );
            if let TyKind::Adt(a, _) = p.types.kind(ps.ty)
                && let Some((at, text)) = p.opt_in_fix(*a, "GpuData")
            {
                d = d.with_fix(format!("opt `{ty}` in to `GpuData`"), at, text);
            }
            out.push(d);
        }
        if slots && !matches!(entry, Entry::Compute(_)) {
            out.push(Diagnostic::new(
                codes::E0602,
                ps.span,
                format!(
                    "{stage} can't take `Slots<T>`: slots are indexed by a kernel's `GlobalId`"
                ),
            ));
        }
    }
}
