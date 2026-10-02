//! wrela's semantics (language.md §§3–8, 12): modules and names, types, traits and generics,
//! type inference, the memory model (§6), and the GPU rules (§12).

pub mod builtins;
pub mod check;
pub mod collect;
pub mod defs;
pub mod memory;
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

/// A checked program: its definitions, every function body's typed tree, and every constant.
#[derive(Clone, Debug)]
pub struct Checked {
    pub program: Program,
    pub bodies: BTreeMap<ty::FnId, thir::Body>,
    pub consts: BTreeMap<ty::ConstId, (ty::TyId, thir::Expr)>,
}

/// Collects and type-checks a whole program.
pub fn check_program(units: Vec<SourceUnit>, diags: &mut Vec<Diagnostic>) -> Checked {
    let mut program = collect::collect(units, diags);
    let mut bodies = BTreeMap::new();
    for i in 0..program.fns.len() {
        let f = ty::FnId(i as u32);
        let (body, d) = check::check_fn(&mut program, f);
        let typed = !d.iter().any(|x| x.is_error());
        diags.extend(d);
        if let Some(b) = body {
            // The memory checker needs a well-typed body; it would only add noise otherwise.
            if typed {
                diags.extend(memory::check_fn(&mut program, f, &b));
            }
            bodies.insert(f, b);
        }
    }
    let mut consts = BTreeMap::new();
    for i in 0..program.consts.len() {
        let c = ty::ConstId(i as u32);
        let (r, d) = check::check_const(&mut program, c);
        diags.extend(d);
        if let Some(r) = r {
            consts.insert(c, r);
        }
    }
    Checked { program, bodies, consts }
}
