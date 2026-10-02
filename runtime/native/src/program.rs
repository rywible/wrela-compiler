//! The CPU side: a program's WASM under wasmtime, its one import, and the batches it submits.
//!
//! [`Program`] is generic over the [`Executor`] that carries out decoded commands, so the ABI
//! rules (imports, exports, bounds, decoding, sequencing, the state hash) are tested here
//! without a GPU; [`crate::gpu::Gpu`] is the real executor.

use crate::check::Checker;
use crate::error::{Error, Result};
use wasmtime::{
    Engine, Extern, ExternType, Func, Instance, Linker, Module, Store, TypedFunc, Val, ValType,
};
use wrela_abi::hash::StateHash;
use wrela_abi::stream::{self, Command, Sequencer};
use wrela_abi::{EXPORT_FRAME, EXPORT_MEMORY, IMPORT_MODULE, IMPORT_SUBMIT};

/// Carries out commands that have already been decoded, sequenced and checked.
pub(crate) trait Executor: 'static {
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()>;
    /// Called after each call of `frame` returns: finish (submit) the frame's work.
    fn end_frame(&mut self) -> Result<()>;
}

/// A numeric WASM value, for calling a program's other exports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Value {
    fn to_val(self) -> Val {
        match self {
            Value::I32(v) => Val::I32(v),
            Value::I64(v) => Val::I64(v),
            Value::F32(v) => Val::F32(v.to_bits()),
            Value::F64(v) => Val::F64(v.to_bits()),
        }
    }

    fn from_val(v: &Val) -> Option<Value> {
        Some(match *v {
            Val::I32(v) => Value::I32(v),
            Val::I64(v) => Value::I64(v),
            Val::F32(bits) => Value::F32(f32::from_bits(bits)),
            Val::F64(bits) => Value::F64(f64::from_bits(bits)),
            _ => return None,
        })
    }

    fn ty(self) -> &'static str {
        match self {
            Value::I32(_) => "i32",
            Value::I64(_) => "i64",
            Value::F32(_) => "f32",
            Value::F64(_) => "f64",
        }
    }
}

/// The store's data: what `wrela.submit` needs.
struct State<E> {
    checker: Checker,
    executor: E,
    hash: StateHash,
    sequencer: Sequencer,
    /// The typed error behind the most recent trap raised by `submit`, so the caller gets it
    /// rather than wasmtime's flattened message.
    failure: Option<Error>,
}

impl<E: Executor> State<E> {
    fn submit(&mut self, batch: &[u8]) -> Result<()> {
        // The state hash covers every submitted byte, valid or not.
        self.hash.update(batch);
        for cmd in stream::decode(batch)? {
            self.sequencer.step(&cmd)?;
            self.checker.check(&cmd)?;
            self.executor.execute(&cmd)?;
        }
        Ok(())
    }
}

pub(crate) struct Program<E: 'static> {
    store: Store<State<E>>,
    instance: Instance,
    frame: TypedFunc<(f32, u32, u32), ()>,
}

/// Checks a module against the program ABI before instantiating it: it may import only
/// `wrela.submit(i32, i32)`, and must export `memory` and `frame(f32, i32, i32)`.
pub(crate) fn check_abi(module: &Module) -> Result<()> {
    for import in module.imports() {
        let (m, n) = (import.module(), import.name());
        if (m, n) != (IMPORT_MODULE, IMPORT_SUBMIT) {
            return Err(Error::Program(format!(
                "it imports `{m}.{n}`, but a wrela program may import only `{IMPORT_MODULE}.{IMPORT_SUBMIT}`"
            )));
        }
        let ok = match import.ty() {
            ExternType::Func(f) => {
                f.params().all(|p| matches!(p, ValType::I32))
                    && f.params().len() == 2
                    && f.results().len() == 0
            }
            _ => false,
        };
        if !ok {
            return Err(Error::Program(format!(
                "its import `{m}.{n}` must be a function (i32, i32) -> (), not {}",
                describe(&import.ty())
            )));
        }
    }
    match module.get_export(EXPORT_MEMORY) {
        Some(ExternType::Memory(_)) => {}
        _ => {
            return Err(Error::Program(format!(
                "it doesn't export its memory as `{EXPORT_MEMORY}`"
            )));
        }
    }
    let frame_ok = match module.get_export(EXPORT_FRAME) {
        Some(ExternType::Func(f)) => {
            let params: Vec<_> = f.params().collect();
            params.len() == 3
                && matches!(params[0], ValType::F32)
                && matches!(params[1], ValType::I32)
                && matches!(params[2], ValType::I32)
                && f.results().len() == 0
        }
        _ => false,
    };
    if !frame_ok {
        return Err(Error::Program(format!(
            "it must export `{EXPORT_FRAME}(time: f32, width: i32, height: i32)` with no results"
        )));
    }
    Ok(())
}

/// What an import is, as both hosts word it: `a function (i32) -> ()`, `a memory`, ...
fn describe(ty: &ExternType) -> String {
    let list = |types: &mut dyn Iterator<Item = ValType>| {
        types.map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
    };
    match ty {
        ExternType::Func(f) => {
            format!("a function ({}) -> ({})", list(&mut f.params()), list(&mut f.results()))
        }
        ExternType::Global(_) => "a global".into(),
        ExternType::Table(_) => "a table".into(),
        ExternType::Memory(_) => "a memory".into(),
        ExternType::Tag(_) => "a tag".into(),
    }
}

/// A module that satisfies the program ABI, ready to instantiate.
pub(crate) struct Compiled {
    engine: Engine,
    module: Module,
}

/// Compiles a program and checks it against the ABI ([`check_abi`]).
pub(crate) fn compile(wasm: &[u8]) -> Result<Compiled> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).map_err(|e| Error::Program(format!("{e:#}")))?;
    check_abi(&module)?;
    Ok(Compiled { engine, module })
}

impl<E: Executor> Program<E> {
    pub(crate) fn instantiate(
        compiled: &Compiled,
        checker: Checker,
        executor: E,
    ) -> Result<Program<E>> {
        let mut linker = Linker::new(&compiled.engine);
        linker
            .func_wrap(IMPORT_MODULE, IMPORT_SUBMIT, submit::<E>)
            .map_err(|e| Error::Program(format!("{e:#}")))?;
        let state = State {
            checker,
            executor,
            hash: StateHash::new(),
            sequencer: Sequencer::new(),
            failure: None,
        };
        let mut store = Store::new(&compiled.engine, state);
        let instance = linker
            .instantiate(&mut store, &compiled.module)
            .map_err(|e| Error::Program(format!("{e:#}")))?;
        let frame = instance
            .get_typed_func::<(f32, u32, u32), ()>(&mut store, EXPORT_FRAME)
            .map_err(|e| Error::Program(format!("{e:#}")))?;
        Ok(Program { store, instance, frame })
    }

    /// Calls `frame(time, width, height)`, then ends the frame.
    pub(crate) fn frame(&mut self, time: f32, width: u32, height: u32) -> Result<()> {
        let result = self.frame.call(&mut self.store, (time, width, height));
        self.settle(result)?;
        let state = self.store.data_mut();
        state.sequencer.end_frame()?;
        state.executor.end_frame()
    }

    /// Calls another export with numeric arguments.
    pub(crate) fn call(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        let func: Func = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| Error::Program(format!("it has no exported function `{name}`")))?;
        let ty = func.ty(&self.store);
        let params: Vec<ValType> = ty.params().collect();
        let matches = params.len() == args.len()
            && params.iter().zip(args).all(|(p, a)| {
                matches!(
                    (p, a),
                    (ValType::I32, Value::I32(_))
                        | (ValType::I64, Value::I64(_))
                        | (ValType::F32, Value::F32(_))
                        | (ValType::F64, Value::F64(_))
                )
            });
        if !matches {
            let given: Vec<&str> = args.iter().map(|a| a.ty()).collect();
            return Err(Error::Program(format!(
                "`{name}` has type {ty}; it can't take ({})",
                given.join(", ")
            )));
        }
        let args: Vec<Val> = args.iter().map(|a| a.to_val()).collect();
        let mut results: Vec<Val> = ty.results().map(|_| Val::I32(0)).collect();
        let result = func.call(&mut self.store, &args, &mut results);
        self.settle(result)?;
        results
            .iter()
            .map(|v| {
                Value::from_val(v)
                    .ok_or_else(|| Error::Program(format!("`{name}` returns a non-numeric value")))
            })
            .collect()
    }

    /// Turns a call's outcome into ours, preferring the typed error `submit` recorded.
    fn settle(&mut self, result: wasmtime::Result<()>) -> Result<()> {
        let failure = self.store.data_mut().failure.take();
        match (result, failure) {
            (Ok(()), _) => Ok(()),
            (Err(_), Some(failure)) => Err(failure),
            (Err(trap), None) => Err(Error::Trap(format!("{trap:#}"))),
        }
    }

    /// FNV-1a 64 of every byte submitted since the program loaded.
    pub(crate) fn hash(&self) -> StateHash {
        self.store.data().hash
    }

    pub(crate) fn executor(&mut self) -> &mut E {
        &mut self.store.data_mut().executor
    }
}

/// `wrela.submit(ptr, len)`: decode and execute one batch of `len` bytes at `ptr`.
fn submit<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    len: u32,
) -> wasmtime::Result<()> {
    let Some(Extern::Memory(memory)) = caller.get_export(EXPORT_MEMORY) else {
        return Err(wasmtime::format_err!("the program has no `{EXPORT_MEMORY}` export"));
    };
    let (memory, state) = memory.data_and_store_mut(&mut caller);
    let result = match (ptr as usize).checked_add(len as usize).filter(|&end| end <= memory.len()) {
        Some(end) => state.submit(&memory[ptr as usize..end]),
        None => Err(Error::Trap(format!(
            "submit({ptr}, {len}) reaches past the end of the program's memory ({} bytes)",
            memory.len()
        ))),
    };
    result.map_err(|e| {
        let message = e.to_string();
        state.failure = Some(e);
        wasmtime::format_err!("{message}")
    })
}

#[cfg(test)]
mod tests;
