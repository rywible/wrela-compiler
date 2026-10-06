//! GPU entry points' signatures (language.md §6.13, §12), checked where they're declared:
//! what each stage may take and return, and that a kernel's `mut` parameters are safe to share
//! across invocations. Lowering then only sees signatures it can map to WGSL.

use crate::defs::{Entry, FieldDef, Lang, Mode};
use crate::program::Program;
use crate::ty::{FloatTy, FnId, IntTy, TyId, TyKind};
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

/// The `ClipPosition` fields of a struct type, as declared; none for any other type.
fn clip_position_fields(p: &Program, t: TyId) -> impl Iterator<Item = &FieldDef> {
    let fields = match p.types.kind(t) {
        TyKind::Adt(a, _) => p.adt(*a).fields(),
        _ => &[],
    };
    fields.iter().filter(|f| p.lang_of_ty(f.ty) == Some(Lang::ClipPosition))
}

/// Whether a type is a vertex shader's output: a `ClipPosition`, or a struct with one.
pub fn is_vertex_output(p: &Program, t: TyId) -> bool {
    p.lang_of_ty(t) == Some(Lang::ClipPosition) || clip_position_fields(p, t).next().is_some()
}

/// WebGPU's default limit on the values a vertex shader passes to a fragment shader
/// (`maxInterStageShaderVariables`).
pub const MAX_VARYINGS: usize = 16;

/// Whether a vertex output's field of type `t` can pass to the fragment shader: WGSL passes
/// numbers and float vectors, interpolated or (in a `Flat<T>`) not.
pub fn is_varying(p: &Program, t: TyId) -> bool {
    let number = |t: TyId| {
        matches!(
            p.types.kind(t),
            TyKind::Float(FloatTy::F32) | TyKind::Vec(_) | TyKind::Int(IntTy::I32 | IntTy::U32)
        )
    };
    match p.types.kind(t) {
        TyKind::Adt(_, args) if p.lang_of_ty(t) == Some(Lang::Flat) => {
            args.first().is_some_and(|&a| number(a))
        }
        _ => number(t),
    }
}

/// E0602: a vertex output's field that can't pass to the fragment shader.
pub fn not_varying(p: &Program, field: &str, ty: TyId, span: wrela_diag::Span) -> Diagnostic {
    Diagnostic::new(
        codes::E0602,
        span,
        format!(
            "the vertex output's `{field}` is a `{}`, which can't pass to the fragment shader",
            p.display_ty(ty)
        ),
    )
    .with_note(
        "WGSL passes numbers and float vectors between stages (`f32`, `i32`, `u32`, `vecN`), \
         each interpolated or in a `Flat<T>`",
    )
}

/// A GPU builtin input, named by the std type that carries it (language.md §12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinInput {
    GlobalId,
    LocalId,
    WorkgroupId,
    VertexIndex,
    InstanceIndex,
    FragCoord,
}

impl BuiltinInput {
    /// The input a parameter of type `t` receives, if `t` is one of the input types.
    pub fn of(p: &Program, t: TyId) -> Option<BuiltinInput> {
        Some(match p.lang_of_ty(t)? {
            Lang::GlobalId => BuiltinInput::GlobalId,
            Lang::LocalId => BuiltinInput::LocalId,
            Lang::WorkgroupId => BuiltinInput::WorkgroupId,
            Lang::VertexIndex => BuiltinInput::VertexIndex,
            Lang::InstanceIndex => BuiltinInput::InstanceIndex,
            Lang::FragCoord => BuiltinInput::FragCoord,
            _ => return None,
        })
    }

    /// Whether an entry point of this stage receives the input.
    pub fn available(self, entry: Entry) -> bool {
        match self {
            BuiltinInput::GlobalId | BuiltinInput::LocalId | BuiltinInput::WorkgroupId => {
                matches!(entry, Entry::Compute(_))
            }
            BuiltinInput::VertexIndex | BuiltinInput::InstanceIndex => entry == Entry::Vertex,
            BuiltinInput::FragCoord => entry == Entry::Fragment,
        }
    }
}

/// E0604: data a GPU entry point receives by value (a uniform) whose type isn't `GpuData`.
/// Reported where the entry point is declared, or, for a type a generic entry point is
/// instantiated with, where it's dispatched.
pub fn not_gpu_data(p: &Program, param: &str, ty: TyId, span: wrela_diag::Span) -> Diagnostic {
    let shown = p.display_ty(ty);
    let mut d = Diagnostic::new(
        codes::E0604,
        span,
        format!("`{param}` is a `{shown}`, which isn't `GpuData`, so it can't go to the GPU"),
    )
    .with_note(
        "data crossing to the GPU must be `GpuData`, so its layout is the same on both sides \
         (§6.13)",
    );
    // Opting in is the fix only if every field is `GpuData` (an enum never is).
    if let TyKind::Adt(a, args) = p.types.kind(ty)
        && !p.adt(*a).is_enum()
        && p.fields_of(*a, args, None)
            .into_iter()
            .all(|ft| crate::traits::implements_builtin(p, ft, Lang::GpuData))
        && let Some((at, text)) = p.opt_in_fix(*a, "GpuData")
    {
        d = d.with_fix(format!("opt `{shown}` in to `GpuData`"), at, text);
    }
    d
}

fn check_entry(p: &Program, f: FnId, entry: Entry, out: &mut Vec<Diagnostic>) {
    let def = p.func(f);
    let stage = entry.describe();
    let unit = p.types.unit;
    // What it returns.
    match entry {
        Entry::Compute(_) if def.ret != unit => out.push(
            Diagnostic::new(codes::E0602, def.sig_span, format!("{stage} doesn't return a value"))
                .with_help("write results through a `mut Slots<T>` parameter"),
        ),
        Entry::Vertex if p.lang_of_ty(def.ret) == Some(Lang::ClipPosition) => {}
        Entry::Vertex if clip_position_fields(p, def.ret).count() != 1 => out.push(
            Diagnostic::new(
                codes::E0602,
                def.sig_span,
                format!("{stage} returns its clip position"),
            )
            .with_help(
                "return a `ClipPosition`, or a struct with one `ClipPosition` field and the \
                 values to pass to the fragment shader",
            ),
        ),
        Entry::Vertex => {
            if let TyKind::Adt(a, _) = p.types.kind(def.ret) {
                // What it passes on (a generic field's type is checked where it's drawn).
                let fields = p.adt(*a).fields();
                let varyings =
                    fields.iter().filter(|f| p.lang_of_ty(f.ty) != Some(Lang::ClipPosition));
                for f in varyings.clone() {
                    if !p.types.has_params(f.ty) && !is_varying(p, f.ty) {
                        out.push(not_varying(p, &f.name, f.ty, f.span));
                    }
                }
                if varyings.count() > MAX_VARYINGS {
                    out.push(
                        Diagnostic::new(
                            codes::E0602,
                            def.sig_span,
                            format!("{stage} passes more than {MAX_VARYINGS} values to the fragment shader"),
                        )
                        .with_note(format!("WebGPU's default limit is {MAX_VARYINGS} (`maxInterStageShaderVariables`)")),
                    );
                }
            }
        }
        Entry::Fragment
            if !matches!(p.types.kind(def.ret), TyKind::Vec(4))
                && p.lang_of_ty(def.ret) != Some(Lang::Over) =>
        {
            out.push(
                Diagnostic::new(
                    codes::E0602,
                    def.sig_span,
                    format!("{stage} returns a `vec4` colour, or an `Over` colour to blend"),
                )
                .with_note("it writes one colour to its target: a `vec4` replaces what's there, and `std::gpu::Over` is drawn over it"),
            )
        }
        _ => {}
    }
    // What it takes.
    for ps in &def.params {
        let lang = p.lang_of_ty(ps.ty);
        let builtin_stage = BuiltinInput::of(p, ps.ty).map(|b| b.available(entry));
        if builtin_stage == Some(false) {
            let ty = p.display_ty(ps.ty);
            out.push(Diagnostic::new(
                codes::E0602,
                ps.span,
                format!("`{ty}` isn't an input {stage} has"),
            ));
            continue;
        }
        let slots = lang.is_some_and(Lang::is_invocation_safe);
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
                    format!(
                        "`{}` is written through, so it's `mut {}`",
                        ps.name,
                        p.display_ty(ps.ty)
                    ),
                )
                .with_help("write `mut` before its type")
                .with_note("to read a buffer, take a run `[T]`"),
            ),
            _ => {}
        }
        // Data passed by value travels as a uniform, so its layout must be the GPU's. Only a
        // fragment shader takes the vertex output.
        let varyings = entry == Entry::Fragment && clip_position_fields(p, ps.ty).next().is_some();
        let resource = lang.is_some_and(Lang::is_texture_or_sampler);
        let passed = builtin_stage.is_none()
            && !slots
            && !resource
            && !varyings
            && !matches!(p.types.kind(ps.ty), TyKind::Slice(_))
            && !p.types.has_params(ps.ty);
        if passed && !crate::traits::implements_builtin(p, ps.ty, Lang::GpuData) {
            out.push(not_gpu_data(p, &ps.name, ps.ty, ps.span));
        }
        // A run is a buffer's elements, which cross to the GPU too.
        if let TyKind::Slice(elem) = *p.types.kind(ps.ty)
            && !p.types.has_params(elem)
            && !crate::traits::implements_builtin(p, elem, Lang::GpuData)
        {
            let mut d = not_gpu_data(p, &ps.name, elem, ps.span);
            d.message = format!(
                "`{}` is a run of `{}`, which isn't `GpuData`, so its elements can't go to the GPU",
                ps.name,
                p.display_ty(elem)
            );
            out.push(d);
        }
        if slots && !matches!(entry, Entry::Compute(_)) {
            let why = match lang {
                Some(Lang::Slots) => "slots are indexed by a kernel's `GlobalId`",
                Some(Lang::Shared) => "workgroup memory is a kernel's",
                _ => "a draw's shaders don't write buffers yet",
            };
            out.push(Diagnostic::new(
                codes::E0602,
                ps.span,
                format!("{stage} can't take `{}`: {why}", p.display_ty(ps.ty)),
            ));
        }
    }
}
