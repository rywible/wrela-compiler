//! A small incremental query layer (milestone 1's answer to Q2: hand-rolled rather than salsa,
//! whose API churned through 2024–25; it can be swapped behind the same `get`/`set` shape when
//! the LSP needs more).
//!
//! - **Inputs** are set from outside (`set_input`); each set starts a new revision if the value
//!   changed.
//! - **Derived queries** (`get`) are memoized by key. While one runs, every query it reads is
//!   recorded as a dependency.
//! - **Revalidation:** in a later revision a memo is reused if none of its dependencies changed
//!   since it was verified (checked recursively, red/green). A recomputed value equal to the old
//!   one keeps its old `changed_at`, so dependents don't recompute (early cutoff).
//! - **Cycles** are detected and panic with the cycle's queries named; the compiler's queries are
//!   acyclic by construction (each stage reads only earlier stages).
//!
//! Single-threaded; memos are never evicted. Both are fine for a command-line compiler run.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::rc::Rc;

pub type Revision = u64;

/// A derived query: a pure function of its key and the queries it reads through `db`.
pub trait Query: 'static {
    type Key: Clone + Eq + Hash + Debug + 'static;
    type Value: Clone + PartialEq + 'static;
    const NAME: &'static str;
    fn compute(db: &Db, key: &Self::Key) -> Self::Value;
}

/// An input: a value set from outside the database.
pub trait Input: 'static {
    type Key: Clone + Eq + Hash + Debug + 'static;
    type Value: Clone + PartialEq + 'static;
    const NAME: &'static str;
}

/// A dependency, type-erased: can it tell whether it changed after a revision?
trait Dep {
    fn changed_after(&self, db: &Db, rev: Revision) -> bool;
}

struct QueryDep<Q: Query>(Q::Key);
struct InputDep<I: Input>(I::Key);

impl<Q: Query> Dep for QueryDep<Q> {
    fn changed_after(&self, db: &Db, rev: Revision) -> bool {
        db.refresh::<Q>(&self.0) > rev
    }
}

impl<I: Input> Dep for InputDep<I> {
    fn changed_after(&self, db: &Db, rev: Revision) -> bool {
        db.input_changed_at::<I>(&self.0).is_none_or(|c| c > rev)
    }
}

struct Memo<V> {
    value: V,
    /// The revision in which the value last changed.
    changed_at: Revision,
    /// The revision in which the memo was last known to be current.
    verified_at: Revision,
    deps: Vec<Rc<dyn Dep>>,
}

struct InputSlot<V> {
    value: V,
    changed_at: Revision,
}

#[derive(Default)]
pub struct Db {
    revision: Cell<Revision>,
    /// Per query type: `HashMap<Key, Memo<Value>>`, type-erased.
    memos: RefCell<HashMap<TypeId, Box<dyn Any>>>,
    inputs: RefCell<HashMap<TypeId, Box<dyn Any>>>,
    /// Dependencies being collected, one frame per running query.
    stack: RefCell<Vec<Vec<Rc<dyn Dep>>>>,
    /// Running queries, for cycle detection: (query name, key text).
    active: RefCell<Vec<(TypeId, String, String)>>,
    /// How many times each query actually ran (for tests of incrementality).
    runs: RefCell<HashMap<&'static str, u32>>,
}

impl Db {
    pub fn new() -> Db {
        Db { revision: Cell::new(1), ..Db::default() }
    }

    pub fn revision(&self) -> Revision {
        self.revision.get()
    }

    /// How many times `Q` has run (computed, not reused).
    pub fn runs<Q: Query>(&self) -> u32 {
        self.runs.borrow().get(Q::NAME).copied().unwrap_or(0)
    }

    pub fn set_input<I: Input>(&self, key: I::Key, value: I::Value) {
        let mut inputs = self.inputs.borrow_mut();
        let slots = inputs
            .entry(TypeId::of::<I>())
            .or_insert_with(|| Box::new(HashMap::<I::Key, InputSlot<I::Value>>::new()))
            .downcast_mut::<HashMap<I::Key, InputSlot<I::Value>>>()
            .expect("input storage has the input's type");
        if let Some(slot) = slots.get(&key)
            && slot.value == value
        {
            return;
        }
        let rev = self.revision.get() + 1;
        self.revision.set(rev);
        slots.insert(key, InputSlot { value, changed_at: rev });
    }

    /// Reads an input, recording the dependency. Panics if it was never set: that's a bug in
    /// the driver, not a user error.
    pub fn input<I: Input>(&self, key: &I::Key) -> I::Value {
        self.record(Rc::new(InputDep::<I>(key.clone())));
        let inputs = self.inputs.borrow();
        let slots = inputs
            .get(&TypeId::of::<I>())
            .and_then(|s| s.downcast_ref::<HashMap<I::Key, InputSlot<I::Value>>>());
        match slots.and_then(|s| s.get(key)) {
            Some(slot) => slot.value.clone(),
            None => panic!("input {}({key:?}) was read before it was set", I::NAME),
        }
    }

    fn input_changed_at<I: Input>(&self, key: &I::Key) -> Option<Revision> {
        let inputs = self.inputs.borrow();
        let slots = inputs
            .get(&TypeId::of::<I>())?
            .downcast_ref::<HashMap<I::Key, InputSlot<I::Value>>>()?;
        slots.get(key).map(|s| s.changed_at)
    }

    fn record(&self, dep: Rc<dyn Dep>) {
        if let Some(frame) = self.stack.borrow_mut().last_mut() {
            frame.push(dep);
        }
    }

    /// The value of query `Q` at `key`, reusing a memo when it's still valid.
    pub fn get<Q: Query>(&self, key: &Q::Key) -> Q::Value {
        self.record(Rc::new(QueryDep::<Q>(key.clone())));
        self.refresh::<Q>(key);
        self.with_memo::<Q, _>(key, |m| m.map(|m| m.value.clone())).expect("refresh leaves a memo")
    }

    fn with_memo<Q: Query, R>(
        &self,
        key: &Q::Key,
        f: impl FnOnce(Option<&Memo<Q::Value>>) -> R,
    ) -> R {
        let memos = self.memos.borrow();
        let table = memos
            .get(&TypeId::of::<Q>())
            .and_then(|t| t.downcast_ref::<HashMap<Q::Key, Memo<Q::Value>>>());
        f(table.and_then(|t| t.get(key)))
    }

    /// Brings the memo for `key` up to date and returns its `changed_at`.
    fn refresh<Q: Query>(&self, key: &Q::Key) -> Revision {
        let rev = self.revision.get();
        // Current, or revalidated because no dependency changed since it was verified.
        let state = self
            .with_memo::<Q, _>(key, |m| m.map(|m| (m.verified_at, m.changed_at, m.deps.clone())));
        if let Some((verified, changed, deps)) = state {
            if verified == rev {
                return changed;
            }
            if !deps.iter().any(|d| d.changed_after(self, verified)) {
                self.update_memo::<Q>(key, |m| m.verified_at = rev);
                return changed;
            }
        }
        // Recompute.
        let id = TypeId::of::<Q>();
        let key_text = format!("{key:?}");
        if self.active.borrow().iter().any(|(t, _, k)| *t == id && *k == key_text) {
            let chain: Vec<String> =
                self.active.borrow().iter().map(|(_, n, k)| format!("{n}({k})")).collect();
            panic!("query cycle: {} -> {}({key_text})", chain.join(" -> "), Q::NAME);
        }
        self.active.borrow_mut().push((id, Q::NAME.to_string(), key_text));
        self.stack.borrow_mut().push(Vec::new());
        let value = Q::compute(self, key);
        let deps = self.stack.borrow_mut().pop().unwrap_or_default();
        self.active.borrow_mut().pop();
        *self.runs.borrow_mut().entry(Q::NAME).or_default() += 1;
        let old_changed =
            self.with_memo::<Q, _>(key, |m| m.filter(|m| m.value == value).map(|m| m.changed_at));
        let changed_at = old_changed.unwrap_or(rev);
        let mut memos = self.memos.borrow_mut();
        let table = memos
            .entry(id)
            .or_insert_with(|| Box::new(HashMap::<Q::Key, Memo<Q::Value>>::new()))
            .downcast_mut::<HashMap<Q::Key, Memo<Q::Value>>>()
            .expect("memo storage has the query's type");
        table.insert(key.clone(), Memo { value, changed_at, verified_at: rev, deps });
        changed_at
    }

    fn update_memo<Q: Query>(&self, key: &Q::Key, f: impl FnOnce(&mut Memo<Q::Value>)) {
        let mut memos = self.memos.borrow_mut();
        if let Some(m) = memos
            .get_mut(&TypeId::of::<Q>())
            .and_then(|t| t.downcast_mut::<HashMap<Q::Key, Memo<Q::Value>>>())
            .and_then(|t| t.get_mut(key))
        {
            f(m);
        }
    }
}

#[cfg(test)]
mod tests;
