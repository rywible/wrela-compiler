//! GPU entry points' signatures (language.md §18.3, §18), checked where they're declared:
//! what each stage may take and return, and that a kernel's `mut` parameters are safe to share
//! across invocations. Lowering then only sees signatures it can map to WGSL.

use crate::defs::{Entry, FieldDef, Lang, Mode, RetMode};
use crate::program::Program;
use crate::ty::{FloatTy, FnId, IntTy, TyId, TyKind, VecElem};
use wrela_diag::{Diagnostic, Span, codes};

/// Every entry point's signature, and that only entry points take `const` parameters.
pub fn check_entries(p: &Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for i in 0..p.fns.len() {
        let f = FnId(i as u32);
        if let Some((entry, _)) = p.func(f).attrs.entry {
            check_entry(p, f, entry, &mut out);
        } else {
            for ps in p.func(f).params.iter().filter(|ps| ps.is_const) {
                out.push(
                    Diagnostic::new(
                        codes::E0602,
                        ps.span,
                        format!(
                            "`{}` is `const`, which only a GPU entry point's parameter is",
                            ps.name
                        ),
                    )
                    .with_note("a `const` parameter is its pipeline's build-time constant (§18.1)")
                    .with_help("remove `const`, or make the function an entry point"),
                );
            }
        }
    }
    out
}

/// Whether `t` may be a `const` parameter's type: `bool`, `u32`, `i32` or `f32`, the scalars a
/// pipeline-overridable constant has.
pub fn is_const_param_ty(p: &Program, t: TyId) -> bool {
    matches!(
        p.types.kind(t),
        TyKind::Bool | TyKind::Int(IntTy::U32 | IntTy::I32) | TyKind::Float(FloatTy::F32)
    )
}

/// The `ClipPosition` fields of an instantiated struct type; none for any other type.
fn clip_position_fields(p: &Program, t: TyId) -> impl Iterator<Item = &FieldDef> {
    let (fields, types) = match p.types.kind(t) {
        TyKind::Adt(a, args) => (p.adt(*a).fields(), p.fields_of(*a, args, None)),
        _ => (&[][..], Vec::new()),
    };
    fields
        .iter()
        .zip(types)
        .filter_map(|(f, ty)| (p.lang_of_ty(ty) == Some(Lang::ClipPosition)).then_some(f))
}

/// Whether kernel `f` shares workgroup memory (a `Shared` parameter).
pub fn shares_memory(p: &Program, f: FnId) -> bool {
    p.func(f).params.iter().any(|ps| p.lang_of_ty(ps.ty).is_some_and(Lang::is_workgroup_memory))
}

/// E0603: kernel `f`, which shares workgroup memory ([`shares_memory`]), dispatched over a
/// domain at `span`.
pub fn shared_over_domain(p: &Program, f: FnId, span: Span) -> Diagnostic {
    let name = &p.func(f).name;
    Diagnostic::new(
        codes::E0603,
        span,
        format!("`{name}` shares workgroup memory, so it isn't dispatched over a domain"),
    )
    .with_note("its invocations past the domain would still reach its barriers, and what they add to the memory is the kernel's to decide")
    .with_help("dispatch it by `groups:`, and check its `GlobalId` against what it covers")
}

/// Whether parameter `t` of an entry point of stage `entry` is a fragment shader's input from
/// the vertex shader: a struct with a `ClipPosition` field.
pub fn is_varyings(p: &Program, entry: Entry, t: TyId) -> bool {
    entry == Entry::Fragment && clip_position_fields(p, t).next().is_some()
}

/// A field of a bound entry point's type (§18): the parameter it binds, and how it holds its
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
        // A `const` parameter's value is its pipeline's, not the bound value's (§18.1).
        if ps.is_const
            || BuiltinInput::of(p, ty).is_some()
            || lang.is_some_and(Lang::is_workgroup_memory)
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
            (_, Some(Lang::TexelsArray)) => of(Lang::TextureArray, args).map(|t| (t, RetMode::Mut)),
            (_, Some(Lang::MipStep)) => of(Lang::TextureStep, args).map(|t| (t, RetMode::Owned)),
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
/// trait `l` with arguments `targs` (§18): a kernel `Kernel`, a vertex shader whose output is
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
        // A column a location.
        TyKind::Mat(c, _) => Some(usize::from(*c)),
        TyKind::Adt(_, args) if p.lang_of_ty(t) == Some(Lang::Flat) => {
            varying_count(p, *args.first()?)
        }
        _ => p.plain_parts(t)?.into_iter().map(|f| varying_count(p, f)).sum(),
    }
}

/// E0602: a vertex output's field that can't pass to the fragment shader.
pub fn not_varying(p: &Program, field: &str, ty: TyId, span: Span) -> Diagnostic {
    Diagnostic::new(
        codes::E0602,
        span,
        format!(
            "the vertex output's `{field}` is a `{}`, which can't pass to the fragment shader",
            p.display_ty(ty)
        ),
    ).with_help("pass numbers, vectors, matrices, or structs and tuples of those: an array's elements as fields of their own, a `bool` as a `u32`")
    .with_note(
        "WGSL passes numbers and vectors between stages (`f32`, `i32`, `u32`, `vec3`, `vec3i`, \
         `vec3u`), and wrela matrices and structs of them too, each interpolated or in a \
         `Flat<T>`",
    )
}

/// A GPU builtin input, named by the std type that carries it (language.md §18).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinInput {
    GlobalId,
    LocalId,
    WorkgroupId,
    VertexIndex,
    InstanceIndex,
    FragCoord,
    SubgroupInvocation,
    SubgroupSize,
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
            Lang::SubgroupInvocation => BuiltinInput::SubgroupInvocation,
            Lang::SubgroupSize => BuiltinInput::SubgroupSize,
            _ => return None,
        })
    }

    /// Whether an entry point of this stage receives the input.
    pub fn available(self, entry: Entry) -> bool {
        match self {
            BuiltinInput::GlobalId
            | BuiltinInput::LocalId
            | BuiltinInput::WorkgroupId
            | BuiltinInput::SubgroupInvocation
            | BuiltinInput::SubgroupSize => matches!(entry, Entry::Compute(_)),
            BuiltinInput::VertexIndex | BuiltinInput::InstanceIndex => entry == Entry::Vertex,
            BuiltinInput::FragCoord => entry == Entry::Fragment,
        }
    }
}

/// One colour a fragment shader writes: integers (to an integer format's target) or floats,
/// of 1 to 4 components.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Colour {
    pub uint: bool,
    pub components: u8,
}

impl Colour {
    /// The colour a value of type `t` is: an `f32`, `vec2`, `vec3` or `vec4`, or a `u32`,
    /// `vec2u`, `vec3u` or `vec4u`.
    pub fn of(p: &Program, t: TyId) -> Option<Colour> {
        Some(match p.types.kind(t) {
            TyKind::Float(FloatTy::F32) => Colour { uint: false, components: 1 },
            TyKind::Int(IntTy::U32) => Colour { uint: true, components: 1 },
            &TyKind::Vec(VecElem::F32, n) => Colour { uint: false, components: n },
            &TyKind::Vec(VecElem::U32, n) => Colour { uint: true, components: n },
            _ => return None,
        })
    }
}

/// What a fragment shader writes, by its return type (language.md §18): a colour for each of
/// its pass's targets, in order, each where it is in the returned value (field indexes), and,
/// from a `WithDepth`, its own depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentOutput {
    /// The colours, each with its path in the returned value.
    pub colours: Vec<(Colour, Vec<u32>)>,
    /// The depth's path, from a `WithDepth`.
    pub depth: Option<Vec<u32>>,
    /// It returns `Over`: its colour is drawn over the target's unless the draw says otherwise.
    pub over: bool,
}

/// A fragment shader's colours, each with its path in the returned value, and whether it
/// returns `Over`.
type Colours = (Vec<(Colour, Vec<u32>)>, bool);

impl FragmentOutput {
    /// What a fragment shader returning `t` writes; `None` if it can't return `t`: a colour
    /// ([`Colour::of`]); an `Over` (a `vec4` to blend); a `Targets2`, `Targets3` or `Targets4`
    /// of colours; or a `WithDepth` of any of those and a depth.
    pub fn of(p: &Program, t: TyId) -> Option<FragmentOutput> {
        let colours = |t: TyId, at: Vec<u32>| -> Option<Colours> {
            if let Some(c) = Colour::of(p, t) {
                return Some((vec![(c, at)], false));
            }
            let TyKind::Adt(_, args) = p.types.kind(t) else { return None };
            match p.lang_of_ty(t)? {
                Lang::Over => Some((
                    vec![(Colour { uint: false, components: 4 }, [at, vec![0]].concat())],
                    true,
                )),
                Lang::Targets2 | Lang::Targets3 | Lang::Targets4 => {
                    let mut out = Vec::new();
                    for (k, &a) in args.iter().enumerate() {
                        let c = Colour::of(p, a)?;
                        out.push((c, [at.clone(), vec![k as u32]].concat()));
                    }
                    Some((out, false))
                }
                _ => None,
            }
        };
        if let Some((colours, over)) = colours(t, Vec::new()) {
            return Some(FragmentOutput { colours, depth: None, over });
        }
        match (p.lang_of_ty(t)?, p.types.kind(t)) {
            (Lang::WithDepth, TyKind::Adt(_, args)) => {
                let (colours, over) = colours(*args.first()?, vec![0])?;
                Some(FragmentOutput { colours, depth: Some(vec![1]), over })
            }
            _ => None,
        }
    }

    /// Whether it gives the fragment's depth (`WithDepth`).
    pub fn writes_depth(&self) -> bool {
        self.depth.is_some()
    }
}

/// E0604: data a GPU entry point receives by value (a uniform) whose type isn't `GpuData`.
/// Reported where the entry point is declared, or, for a type a generic entry point is
/// instantiated with, where it's dispatched.
pub fn not_gpu_data(p: &Program, param: &str, ty: TyId, span: Span) -> Diagnostic {
    let shown = p.display_ty(ty);
    let mut d = Diagnostic::new(
        codes::E0604,
        span,
        format!("`{param}` is a `{shown}`, which isn't `GpuData`, so it can't go to the GPU"),
    )
    .with_note(
        "data crossing to the GPU must be `GpuData`, so its layout is the same on both sides \
         (§6.10)",
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
    } else {
        d = d.with_help(
            "pass the GPU numbers, vectors, matrices and structs of them; keep text, `Vec`s and handles on the CPU",
        );
    }
    d
}

/// E0604 for the elements `param` holds, of type `elem`, unless they're `GpuData` (or the type
/// waits on type parameters): `message` says what holds them, from `elem` as shown.
fn elems_not_gpu_data(
    p: &Program,
    param: &str,
    elem: TyId,
    span: Span,
    message: impl FnOnce(&str) -> String,
) -> Option<Diagnostic> {
    if p.types.has_params(elem) || crate::traits::implements_builtin(p, elem, Lang::GpuData) {
        return None;
    }
    let mut d = not_gpu_data(p, param, elem, span);
    d.message = message(&p.display_ty(elem));
    Some(d)
}

/// Whether `t` is a group (§18): a borrow struct (not one std gives a meaning, as `GpuSpan`),
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
    span: Span,
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
            {
                out.extend(elems_not_gpu_data(p, &at, elem, span, |e| {
                    format!("`{at}` is a span of `{e}`, which isn't `GpuData`, so its elements can't go to the GPU")
                }));
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
            if let TyKind::Adt(a, args) = p.types.kind(def.ret) {
                // What it passes on (a generic field's type is checked where it's drawn).
                let fields = p.adt(*a).fields();
                let mut count = 0;
                for (f, ty) in fields.iter().zip(p.fields_of(*a, args, None)).filter(|(_, ty)| {
                    p.lang_of_ty(*ty) != Some(Lang::ClipPosition)
                }) {
                    match varying_count(p, ty) {
                        Some(n) => count += n,
                        None if !p.types.has_params(ty) => {
                            out.push(not_varying(p, &f.name, ty, f.span));
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
            if FragmentOutput::of(p, def.ret).is_none()
                && !p.types.any(def.ret, &mut |k| matches!(k, TyKind::Error)) =>
        {
            out.push(
            Diagnostic::new(
                codes::E0602,
                def.sig_span,
                format!(
                    "{stage} returns a colour (an `f32`, `vec2`, `vec3`, `vec4`, or for an integer \
                     target a `u32`, `vec2u`, `vec3u` or `vec4u`), an `Over` colour to blend, a \
                     `Targets2`, `Targets3` or `Targets4` of colours, or a `WithDepth` of one of \
                     those and a depth"
                ),
            )
            .with_note("it writes a colour to each of its pass's targets: a colour replaces what's there unless the draw's `blend:` says otherwise, `std::gpu::Over` is drawn over it, a `Targets2` writes two targets, and `std::gpu::WithDepth<C>` writes its `C` with a depth of its own")
            .with_help("return one of those: `-> vec4` for a colour"),
            )
        }
        _ => {}
    }
    // What it takes.
    for ps in &def.params {
        if ps.is_const {
            if !is_const_param_ty(p, ps.ty)
                && !p.types.any(ps.ty, &mut |k| matches!(k, TyKind::Error))
            {
                let ty = p.display_ty(ps.ty);
                out.push(
                    Diagnostic::new(
                        codes::E0602,
                        ps.span,
                        format!("`{}` is a `const {ty}`; a `const` parameter is a `bool`, `u32`, `i32` or `f32`", ps.name),
                    )
                    .with_note("it's a pipeline-overridable constant, which WGSL has of those scalars")
                    .with_help("pass anything else as an ordinary parameter"),
                );
            }
            continue;
        }
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
                     (§18.4)",
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
        if let TyKind::Slice(elem) = *p.types.kind(ps.ty) {
            out.extend(elems_not_gpu_data(p, &ps.name, elem, ps.span, |e| {
                format!(
                    "`{}` is a run of `{e}`, which isn't `GpuData`, so its elements can't go to the GPU",
                    ps.name
                )
            }));
        }
        // So are a span's and a buffer-backed kernel output's elements: every way to make one
        // bounds them `GpuData`, and this keeps it so whatever makes one. (Workgroup memory,
        // `Shared<T, N>`, is the GPU's alone, so its elements needn't be.)
        if let TyKind::Adt(_, args) = p.types.kind(ps.ty)
            && matches!(
                lang,
                Some(Lang::GpuSpan | Lang::Slots | Lang::Atomics | Lang::One | Lang::Append)
            )
            && let Some(&elem) = args.first()
        {
            out.extend(elems_not_gpu_data(p, &ps.name, elem, ps.span, |e| {
                format!(
                    "`{}` holds `{e}`s, which aren't `GpuData`, so they can't be on the GPU",
                    ps.name
                )
            }));
        }
        // A fragment shader may count into atomics (a test's tally of what it shaded, say);
        // WebGPU forbids a vertex shader writable storage.
        let fragment_atomics = entry == Entry::Fragment && lang == Some(Lang::Atomics);
        if slots && !matches!(entry, Entry::Compute(_)) && !fragment_atomics {
            let why = match lang {
                Some(Lang::Slots) => "slots are indexed by a kernel's `GlobalId`",
                Some(Lang::One) => "one invocation of a kernel writes it",
                Some(l) if l.is_workgroup_memory() => "workgroup memory is a kernel's",
                Some(Lang::Atomics) => "WebGPU gives a vertex shader no writable buffers",
                Some(Lang::Texels | Lang::Texels3d | Lang::TexelsArray | Lang::MipStep) => {
                    "texels are written by a kernel's `GlobalId`"
                }
                _ => "a draw's shaders don't write buffers yet",
            };
            out.push(Diagnostic::new(
                codes::E0602,
                ps.span,
                format!("{stage} can't take `{}`: {why}", p.display_ty(ps.ty)),
            ).with_help("give it to a `@compute` kernel instead, or pass what the shader reads as a buffer it borrows"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature_errors(source: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let p = crate::collect::collect(
            crate::test_units(source),
            &crate::collect::PackageInfo::std_and_program(),
            &mut diagnostics,
        );
        assert!(!wrela_diag::has_errors(&diagnostics), "{diagnostics:#?}");
        check_entries(&p)
    }

    #[test]
    fn vertex_position_uses_instantiated_field_types() {
        let errors = signature_errors(
            "use std::gpu::ClipPosition\n\
             struct Output<P> { position: P, colour: vec3 }\n\
             @vertex\n\
             fn vertex() -> Output<ClipPosition> {\n\
                 Output { position: ClipPosition { position: vec4(0.0) }, colour: vec3(1.0) }\n\
             }\n\
             @fragment\n\
             fn fragment(input: Output<ClipPosition>) -> vec4 { vec4(input.colour, 1.0) }\n",
        );
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn vertex_varyings_validate_instantiated_field_types() {
        let errors = signature_errors(
            "use std::gpu::ClipPosition\n\
             struct Output<T> { position: ClipPosition, value: T }\n\
             @vertex\n\
             fn vertex() -> Output<f64> {\n\
                 Output { position: ClipPosition { position: vec4(0.0) }, value: 1.0 }\n\
             }\n",
        );
        assert!(
            errors.iter().any(|d| d.code == codes::E0602 && d.message.contains("`value`")),
            "{errors:#?}"
        );
        let errors = signature_errors(
            "use std::gpu::ClipPosition\n\
             struct Output<T> { position: ClipPosition, value: T }\n\
             @vertex\n\
             fn vertex() -> Output<(mat4, mat4, mat4, mat4, mat4)> {\n\
                 Output { position: ClipPosition { position: vec4(0.0) }, value: (mat4(), mat4(), mat4(), mat4(), mat4()) }\n\
             }\n",
        );
        assert!(
            errors.iter().any(|d| d.code == codes::E0602 && d.message.contains("20 values")),
            "{errors:#?}"
        );
    }

    /// E0602 lists the integer colours that a fragment shader may return: each is accepted.
    #[test]
    fn fragment_colours_are_the_ones_listed() {
        for colour in ["u32", "vec2u", "vec3u", "vec4u"] {
            let errors = signature_errors(&format!(
                "@fragment\nfn fragment() -> {colour} {{ {colour}(1) }}\n"
            ));
            assert!(errors.is_empty(), "{colour}: {errors:#?}");
        }
        let errors = signature_errors("@fragment\nfn fragment() -> i32 { 1 }\n");
        assert!(
            errors.iter().any(|d| d.code == codes::E0602
                && d.message.contains("a `u32`, `vec2u`, `vec3u` or `vec4u`")),
            "{errors:#?}"
        );
    }
}
