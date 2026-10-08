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
        /// GPU only: for each parameter bound to resources (a run, a span, a `Slots`, a
        /// texture, a sampler, or a group of them), what it's bound to.
        resources: Vec<Option<Bound>>,
    },
    /// A closure in `owner`'s body, lifted to a function of its captures and parameters.
    Closure { owner: Rc<InstanceKey>, id: ClosureId },
    /// A derived interpretation of a callable (language.md §13).
    /// `output`: an interval's result type (its range is that type's box), or what a value and
    /// gradient carries beside them (`value_gradient_with`'s `T`).
    Derived { of: Callable, kind: DeriveKind, input: TyId, output: Option<TyId> },
    /// Drop or clone glue for a concrete type (`crate::glue`).
    Glue { kind: crate::glue::GlueKind, ty: TyId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeriveKind {
    /// The value and its gradient: `(f32, X)`; with an `output` `T`, `(f32, X, T)`.
    ValueAndGradient,
    /// The interval over a box.
    Interval,
    /// A parameter gradient (§22): the value and its derivative by each of some lifted
    /// literals. `input` is the literals' array type, `output` the result's.
    Literals,
    /// The lifted literals the callable can read (§22): `output` is the run's type.
    Reads,
}

/// GPU only: what a parameter is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Bound {
    /// A run, a span, a `Slots`, a texture or a sampler: its resource.
    One(ir::ResourceId),
    /// A group (a borrow struct, §12): each field's, `None` for a field passed as a value.
    Group(Vec<Option<Bound>>),
}

impl Bound {
    /// The resource it is, if it's one.
    pub fn one(&self) -> Option<ir::ResourceId> {
        match self {
            Bound::One(r) => Some(*r),
            Bound::Group(_) => None,
        }
    }
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
            InstanceKey::Derived { .. } | InstanceKey::Glue { .. } => None,
        }
    }

    /// Whether the instance specializes its function by type arguments or callables, the
    /// parts that polymorphic recursion makes larger at each level.
    pub fn specializes(&self) -> bool {
        match self {
            InstanceKey::Fn { substs, callables, .. } => {
                !substs.is_empty() || callables.iter().any(Option::is_some)
            }
            InstanceKey::Closure { .. }
            | InstanceKey::Derived { .. }
            | InstanceKey::Glue { .. } => false,
        }
    }

    pub fn substs(&self) -> &[TyId] {
        match self {
            InstanceKey::Fn { substs, .. } => substs,
            InstanceKey::Closure { owner, .. } => owner.substs(),
            InstanceKey::Derived { .. } | InstanceKey::Glue { .. } => &[],
        }
    }

    pub fn resources(&self) -> &[Option<Bound>] {
        match self {
            InstanceKey::Fn { resources, .. } => resources,
            InstanceKey::Closure { owner, .. } => owner.resources(),
            InstanceKey::Derived { .. } | InstanceKey::Glue { .. } => &[],
        }
    }
}

impl Callable {
    /// The instance that a call of it runs.
    pub fn instance_key(&self) -> InstanceKey {
        match self {
            Callable::Closure { owner, id } => {
                InstanceKey::Closure { owner: owner.clone(), id: *id }
            }
            Callable::Func { func, substs } => InstanceKey::plain(*func, substs.clone()),
        }
    }
}
