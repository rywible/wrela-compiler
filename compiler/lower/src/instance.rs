//! Instance keys: what makes two lowerings of the same source function different.

use std::rc::Rc;
use wrela_ir as ir;
use wrela_sema::ty::{ClosureId, FnId, TyId};

/// One monomorphized function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InstanceKey {
    Fn {
        func: FnId,
        /// Concrete types for all the function's generics (owner's, then its own).
        substs: Vec<TyId>,
        /// For each parameter of function type: what's passed for it.
        callables: Vec<Option<Callable>>,
        /// GPU only: for each run or `Slots` parameter, the resource it's bound to.
        resources: Vec<Option<ir::ResourceId>>,
    },
    /// A closure in `owner`'s body, lifted to a function of its captures and parameters.
    Closure { owner: Rc<InstanceKey>, id: ClosureId },
    /// A derived interpretation of a callable (language.md §13).
    Derived { of: Callable, kind: DeriveKind, input: TyId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeriveKind {
    /// The value and its gradient: `(f32, X)`.
    ValueAndGradient,
    /// The interval over a box.
    Interval,
}

/// What's passed for a parameter of function type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Callable {
    Closure { owner: Rc<InstanceKey>, id: ClosureId },
    Func { func: FnId, substs: Vec<TyId> },
}

impl InstanceKey {
    pub fn plain(func: FnId, substs: Vec<TyId>) -> InstanceKey {
        InstanceKey::Fn { func, substs, callables: Vec::new(), resources: Vec::new() }
    }

    /// The source function whose body this instance lowers (the owner, for a closure).
    pub fn source_fn(&self) -> Option<FnId> {
        match self {
            InstanceKey::Fn { func, .. } => Some(*func),
            InstanceKey::Closure { owner, .. } => owner.source_fn(),
            InstanceKey::Derived { .. } => None,
        }
    }

    pub fn substs(&self) -> &[TyId] {
        match self {
            InstanceKey::Fn { substs, .. } => substs,
            InstanceKey::Closure { owner, .. } => owner.substs(),
            InstanceKey::Derived { .. } => &[],
        }
    }

    /// The function-typed arguments in scope in this instance's body.
    pub fn callables(&self) -> &[Option<Callable>] {
        match self {
            InstanceKey::Fn { callables, .. } => callables,
            InstanceKey::Closure { owner, .. } => owner.callables(),
            InstanceKey::Derived { .. } => &[],
        }
    }

    pub fn resources(&self) -> &[Option<ir::ResourceId>] {
        match self {
            InstanceKey::Fn { resources, .. } => resources,
            InstanceKey::Closure { owner, .. } => owner.resources(),
            InstanceKey::Derived { .. } => &[],
        }
    }
}
