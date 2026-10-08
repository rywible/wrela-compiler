//! GPU entry points' signatures (language.md §6.13, §12), checked where they're declared:
//! what each stage may take and return, and that a kernel's `mut` parameters are safe to share
//! across invocations. Lowering then only sees signatures it can map to WGSL.

use crate::defs::{Entry, FieldDef, Lang, Mode, RetMode};
use crate::program::Program;
use crate::ty::{FloatTy, FnId, IntTy, TyId, TyKind, VecElem};
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

/// Whether kernel `f` shares workgroup memory (a `Shared` parameter).
pub fn shares_memory(p: &Program, f: FnId) -> bool {
    p.func(f).params.iter().any(|ps| p.lang_of_ty(ps.ty) == Some(Lang::Shared))
}

/// Whether parameter `t` of an entry point of stage `entry` is a fragment shader's input from
/// the vertex shader: a struct with a `ClipPosition` field.
pub fn is_varyings(p: &Program, entry: Entry, t: TyId) -> bool {
    entry == Entry::Fragment && clip_position_fields(p, t).next().is_some()
}

/// A field of a bound entry point's type (§12): the parameter it binds, and how it holds its
/// argument.
#[derive(Clone, Copy, Debug)]
pub struct BoundField {
    pub param: usize,
    pub ty: TyId,
    pub mode: RetMode,
}

/// The parameters of entry point `f` that a command binds, in order, as a bound entry point's
/// fields hold their arguments: every parameter but the GPU's builtins, workgroup memory and a
/// fragment shader's input from the vertex shader. A buffer the GPU reads is held as a
/// `GpuSpan<T>` and one it writes as a `GpuSpanMut<T>`; what the GPU writes through a container
/// (`Append<T>`'s `AppendBuffer<T>`, `Texels<F>`'s `Texture<F>`) is held `mut`; a texture, a
/// sampler and a value that isn't `Copy` are borrowed; and anything else is held by value.
pub fn bound_fields(p: &Program, f: FnId) -> Vec<BoundField> {
    let Some((entry, _)) = p.func(f).attrs.entry else { return Vec::new() };
    let of = |l: Lang, args: Vec<TyId>| p.lang_adt(l).map(|a| p.types.adt(a, args));
    let mut out = Vec::new();
    for (param, ps) in p.func(f).params.iter().enumerate() {
        let ty = ps.ty;
        let lang = p.lang_of_ty(ty);
        if BuiltinInput::of(p, ty).is_some()
            || lang == Some(Lang::Shared)
            || is_varyings(p, entry, ty)
        {
            continue;
        }
        let args = match p.types.kind(ty) {
            TyKind::Adt(_, args) => args.clone(),
            _ => Vec::new(),
        };
        let held = match (p.types.kind(ty), lang) {
            (&TyKind::Slice(elem), _) => of(Lang::GpuSpan, vec![elem]).map(|t| (t, RetMode::Owned)),
            (_, Some(Lang::Slots | Lang::Atomics | Lang::One)) => {
                of(Lang::GpuSpanMut, args).map(|t| (t, RetMode::Owned))
            }
            (_, Some(Lang::Append)) => of(Lang::AppendBuffer, args).map(|t| (t, RetMode::Mut)),
            (_, Some(Lang::AtomicMap)) => {
                of(Lang::AtomicMapBuffer, Vec::new()).map(|t| (t, RetMode::Mut))
            }
            (_, Some(Lang::Texels)) => of(Lang::Texture, args).map(|t| (t, RetMode::Mut)),
            (_, Some(Lang::Texels3d)) => of(Lang::Texture3d, args).map(|t| (t, RetMode::Mut)),
            (_, Some(l)) if l.is_texture_or_sampler() => Some((ty, RetMode::Borrow)),
            _ if p.is_borrow_struct(ty) || crate::traits::implements_builtin(p, ty, Lang::Copy) => {
                Some((ty, RetMode::Owned))
            }
            _ => Some((ty, RetMode::Borrow)),
        };
        let (ty, mode) = held.unwrap_or((p.types.error, RetMode::Owned));
        out.push(BoundField { param, ty, mode });
    }
    out
}

/// What a bound entry point's type passes between a draw's shaders: a vertex shader's output,
/// or a fragment shader's input from the vertex shader (`None` for one that takes none). Of
/// entry point `f` with generic arguments `args`.
pub fn bound_varyings(p: &Program, f: FnId, args: &[TyId]) -> Option<TyId> {
    let def = p.func(f);
    let (entry, _) = def.attrs.entry?;
    let subst = crate::ty::Subst::from_pairs(&p.fn_all_generics(f), args);
    let t = match entry {
        Entry::Vertex => def.ret,
        Entry::Fragment => def.params.iter().find(|ps| is_varyings(p, entry, ps.ty))?.ty,
        Entry::Compute(_) => return None,
    };
    Some(p.types.subst(t, &subst))
}

/// Whether entry point `f`, bound with generic arguments `args`, has the bound entry point's
/// trait `l` with arguments `targs` (§12): a kernel `Kernel`, a vertex shader whose output is
/// `V` `VertexShader<V>`, and a fragment shader whose input is `V`, or that takes none,
/// `FragmentShader<V>`.
pub fn bound_has(p: &Program, f: FnId, args: &[TyId], l: Lang, targs: &[TyId]) -> bool {
    let Some((entry, _)) = p.func(f).attrs.entry else { return false };
    let varyings = bound_varyings(p, f, args);
    match (l, entry) {
        (Lang::Kernel, Entry::Compute(_)) => true,
        (Lang::VertexShader, Entry::Vertex) => varyings == targs.first().copied(),
        (Lang::FragmentShader, Entry::Fragment) => {
            varyings.is_none() || varyings == targs.first().copied()
        }
        _ => false,
    }
}

/// Whether a type is a vertex shader's output: a `ClipPosition`, or a struct with one.
pub fn is_vertex_output(p: &Program, t: TyId) -> bool {
    p.lang_of_ty(t) == Some(Lang::ClipPosition) || clip_position_fields(p, t).next().is_some()
}

/// WebGPU's default limit on the values a vertex shader passes to a fragment shader
/// (`maxInterStageShaderVariables`).
pub const MAX_VARYINGS: usize = 16;

/// Whether a vertex output's field of type `t` can pass to the fragment shader.
pub fn is_varying(p: &Program, t: TyId) -> bool {
    varying_count(p, t).is_some()
}

/// How many values a vertex output's field of type `t` passes to the fragment shader, each in
/// its own WGSL location: one for a number or a vector of `f32`s, `i32`s or `u32`s, one per
/// column for a matrix, and its fields' for a struct or a tuple of those; each interpolated or,
/// in a `Flat<T>`, not. `None` if it can't pass.
pub fn varying_count(p: &Program, t: TyId) -> Option<usize> {
    match p.types.kind(t) {
        TyKind::Float(FloatTy::F32) | TyKind::Int(IntTy::I32 | IntTy::U32) => Some(1),
        TyKind::Vec(e, _) if e.on_gpu() => Some(1),
        TyKind::Mat(n) => Some(usize::from(*n)),
        TyKind::Adt(_, args) if p.lang_of_ty(t) == Some(Lang::Flat) => {
            varying_count(p, *args.first()?)
        }
        _ => p.plain_parts(t)?.into_iter().map(|f| varying_count(p, f)).sum(),
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
        "WGSL passes numbers and vectors between stages (`f32`, `i32`, `u32`, `vec3`, `vec3i`, \
         `vec3u`), and wrela matrices and structs of them too, each interpolated or in a \
         `Flat<T>`",
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

/// Whether `t` is a group (§12): a borrow struct (not one std gives a meaning, as `GpuSpan`),
/// which GPU code takes as its fields: their bindings and values.
pub fn is_group(p: &Program, t: TyId) -> bool {
    matches!(p.types.kind(t), TyKind::Adt(a, _) if p.adt(*a).borrow && p.adt(*a).lang.is_none() && p.adt(*a).entry.is_none())
}

/// What an entry point can't take in group `t`, reached as `path` (`lit.probes`): each field
/// is a texture or sampler, borrowed; a `GpuSpan<T>` of `GpuData`; a `GpuData` value; or a
/// group. A group passes what the GPU reads, so nothing in it is `mut`.
fn check_group(
    p: &Program,
    path: &str,
    t: TyId,
    span: wrela_diag::Span,
    stage: &str,
    out: &mut Vec<Diagnostic>,
) {
    let TyKind::Adt(a, args) = p.types.kind(t) else { return };
    let defs = p.adt_fields(*a, None);
    let tys = p.fields_of(*a, args, None);
    for (d, &ft) in defs.iter().zip(&tys) {
        let at = format!("{path}.{}", d.name);
        let lang = p.lang_of_ty(ft);
        if d.mode == RetMode::Mut || lang == Some(Lang::GpuSpanMut) {
            out.push(
                Diagnostic::new(
                    codes::E0602,
                    span,
                    format!("{stage} can't take `{at}`: a group passes what the GPU reads, and `{at}` is written"),
                )
                .with_help("pass what the GPU writes as a parameter of its own"),
            );
        } else if lang.is_some_and(Lang::is_texture_or_sampler) {
            if d.mode == RetMode::Owned {
                out.push(Diagnostic::new(
                    codes::E0602,
                    span,
                    format!("`{at}` holds a `{}` by value; a group borrows it", p.display_ty(ft)),
                ));
            }
        } else if lang == Some(Lang::GpuSpan) {
            if let TyKind::Adt(_, sargs) = p.types.kind(ft)
                && let Some(&elem) = sargs.first()
                && !p.types.has_params(elem)
                && !crate::traits::implements_builtin(p, elem, Lang::GpuData)
            {
                let mut d = not_gpu_data(p, &at, elem, span);
                d.message = format!(
                    "`{at}` is a span of `{}`, which isn't `GpuData`, so its elements can't go to the GPU",
                    p.display_ty(elem)
                );
                out.push(d);
            }
        } else if lang.is_some_and(Lang::is_invocation_safe) {
            out.push(Diagnostic::new(
                codes::E0602,
                span,
                format!("{stage} can't take `{at}` in a group: it's written through"),
            ));
        } else if is_group(p, ft) {
            check_group(p, &at, ft, span, stage, out);
        } else if let TyKind::Slice(_) = p.types.kind(ft) {
            out.push(
                Diagnostic::new(
                    codes::E0602,
                    span,
                    format!("`{at}` is a run of the CPU's memory, which the GPU can't read"),
                )
                .with_help("hold the buffer's elements as a `GpuSpan<T>`"),
            );
        } else if !p.types.has_params(ft)
            && !crate::traits::implements_builtin(p, ft, Lang::GpuData)
        {
            out.push(not_gpu_data(p, &at, ft, span));
        }
    }
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
                let mut count = 0;
                for f in fields.iter().filter(|f| p.lang_of_ty(f.ty) != Some(Lang::ClipPosition)) {
                    match varying_count(p, f.ty) {
                        Some(n) => count += n,
                        None if !p.types.has_params(f.ty) => {
                            out.push(not_varying(p, &f.name, f.ty, f.span));
                        }
                        None => {}
                    }
                }
                if count > MAX_VARYINGS {
                    out.push(
                        Diagnostic::new(
                            codes::E0602,
                            def.sig_span,
                            format!("{stage} passes {count} values to the fragment shader, more than {MAX_VARYINGS}"),
                        )
                        .with_note(format!("WebGPU's default limit is {MAX_VARYINGS} (`maxInterStageShaderVariables`): a matrix passes a value per column, and a struct one per field")),
                    );
                }
            }
        }
        Entry::Fragment
            if !matches!(p.types.kind(def.ret), TyKind::Vec(VecElem::F32, 4))
                && def.ret != p.types.u32
                && !matches!(p.lang_of_ty(def.ret), Some(Lang::Over | Lang::WithDepth)) =>
        {
            out.push(
                Diagnostic::new(
                    codes::E0602,
                    def.sig_span,
                    format!(
                        "{stage} returns a `vec4` colour, an `Over` colour to blend, a \
                         `WithDepth` colour and depth, or a `u32`"
                    ),
                )
                .with_note("it writes one colour to its target: a `vec4` replaces what's there, `std::gpu::Over` is drawn over it, `std::gpu::WithDepth` replaces it with a depth of its own, and a `u32` is an `R32Uint` texture's texel"),
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
        let varyings = is_varyings(p, entry, ps.ty);
        let resource = lang.is_some_and(Lang::is_texture_or_sampler) || lang == Some(Lang::GpuSpan);
        let group = is_group(p, ps.ty);
        if group {
            check_group(p, &ps.name, ps.ty, ps.span, stage, out);
        }
        let passed = builtin_stage.is_none()
            && !slots
            && !resource
            && !group
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
        // A fragment shader may count into atomics (a test's tally of what it shaded, say);
        // WebGPU forbids a vertex shader writable storage.
        let fragment_atomics = entry == Entry::Fragment && lang == Some(Lang::Atomics);
        if slots && !matches!(entry, Entry::Compute(_)) && !fragment_atomics {
            let why = match lang {
                Some(Lang::Slots) => "slots are indexed by a kernel's `GlobalId`",
                Some(Lang::One) => "one invocation of a kernel writes it",
                Some(Lang::Shared) => "workgroup memory is a kernel's",
                Some(Lang::Atomics) => "WebGPU gives a vertex shader no writable buffers",
                Some(Lang::Texels | Lang::Texels3d) => {
                    "texels are written by a kernel's `GlobalId`"
                }
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
