//! wrela's semantics (language.md §§3–8, 12): modules and names, types, traits and generics,
//! type inference, the memory model (§6), and the GPU rules (§12).

pub mod borrowck;
pub mod builtins;
pub mod check;
pub mod collect;
pub mod defs;
pub mod gpu;
pub mod mir;
pub mod program;
pub mod resolve;
pub mod thir;
pub mod traits;
pub mod ty;

use std::collections::BTreeMap;
use wrela_diag::Diagnostic;

pub use collect::SourceUnit;
pub use program::Program;

/// The std library's sources, embedded in the compiler: (module path, text).
pub const STD_SOURCES: &[(&str, &str)] = &[
    ("std::prelude", include_str!("../../std/prelude.wrela")),
    ("std::gpu", include_str!("../../std/gpu.wrela")),
    ("std::derive", include_str!("../../std/derive.wrela")),
    ("std::field", include_str!("../../std/field.wrela")),
    ("std::math", include_str!("../../std/math.wrela")),
];

/// A checked program: its definitions, every function body's MIR, and every constant.
#[derive(Debug)]
pub struct Checked {
    pub program: Program,
    /// Each function body's MIR.
    pub mir: BTreeMap<ty::FnId, mir::Body>,
    pub consts: BTreeMap<ty::ConstId, (ty::TyId, thir::Expr)>,
}

/// Collects and type-checks a whole program.
pub fn check_program(units: Vec<SourceUnit>, diags: &mut Vec<Diagnostic>) -> Checked {
    let program = collect::collect(units, diags);
    // From here on the definitions are read-only.
    let p = &program;
    diags.extend(gpu::check_entries(p));
    let (const_tys, consts) = check::check_consts(p, diags);
    let mut mir = BTreeMap::new();
    for i in 0..p.fns.len() {
        let f = ty::FnId(i as u32);
        let (body, d) = check::check_fn(p, &const_tys, f);
        let typed = !d.iter().any(|x| x.is_error());
        diags.extend(d);
        if let Some(b) = body {
            // The memory checker needs a well-typed body; it would only add noise otherwise.
            if typed {
                let (m, d) = mir::build::build(p, &consts, f, &b);
                diags.extend(d);
                diags.extend(borrowck::check(p, &m));
                mir.insert(f, m);
            }
        }
    }
    for i in 0..p.adts.len() {
        diags.extend(check::check_field_defaults(p, &const_tys, ty::AdtId(i as u32)));
    }
    Checked { program, mir, consts }
}
