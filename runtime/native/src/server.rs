//! A server build's rooms (language.md §19): `server/game.wasm`, run one instance per room, each
//! with a memory of its own (not shared), a cap on that memory, and a deadline on every call
//! into it. A room is started by `serve_init` and woken by `serve`; nothing it calls blocks: a
//! request it makes is answered when the call has returned, for the next call to read.
//!
//! A call that fails says why ([`Failure`]): the program's own trap (a panic, a failed
//! `assert`, an index out of range), or the host's limits: the deadline, the memory cap, or a
//! stack too deep. Only the first is the program's; the others say nothing about it, so a room
//! that hits one may succeed on another try with more room or time.

use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, TypedFunc};
use wrela_abi::input::{EVENT_SIZE, Event, Queue};
use wrela_abi::stream::{self, Command};
use wrela_abi::{
    EXPORT_MEMORY, EXPORT_SERVE, EXPORT_SERVE_INIT, IMPORT_CLOCK, IMPORT_INPUT, IMPORT_LIMIT,
    IMPORT_MODULE, IMPORT_PAYLOAD, IMPORT_PHASE, IMPORT_PRINT, IMPORT_REQUEST_STATUS,
    IMPORT_REQUEST_TAKE, IMPORT_SUBMIT,
};

/// How often the engines' epoch advances: a deadline is a whole number of these.
const EPOCH: Duration = Duration::from_millis(1);

/// At most this many printed lines are kept for each room; past it, lines are counted, not
/// kept, so printing can't grow the host's memory or make it wait.
const PRINTED_CAP: usize = 256;

/// The server engine: a call can be interrupted at its deadline (epochs advance on a thread of
/// their own), and modules may use atomics on their own memory.
static ENGINE: LazyLock<Engine> = LazyLock::new(|| {
    let mut config = Config::new();
    config.wasm_threads(true).epoch_interruption(true);
    let engine = Engine::new(&config).expect("the server engine's configuration is valid");
    let ticking = engine.clone();
    std::thread::Builder::new()
        .name("wrela-server-epochs".into())
        .spawn(move || {
            loop {
                std::thread::sleep(EPOCH);
                ticking.increment_epoch();
            }
        })
        .expect("can't start the server's epoch thread");
    engine
});

/// Why a call into a room failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The program trapped of its own accord, with its panic's message if it panicked.
    Program(String),
    /// The call ran past its deadline.
    Deadline,
    /// The program asked for memory past the room's cap.
    Memory,
    /// The call went deeper than the stack allows.
    Stack,
    /// The host couldn't do what the program asked, or the module isn't a server build.
    Host(String),
}

impl Failure {
    /// Whether the failure is the program's own: the same call, on the same state, fails the
    /// same way wherever it runs. The others are the host's limits.
    pub fn is_program(&self) -> bool {
        matches!(self, Failure::Program(_))
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Program(why) => write!(f, "the program trapped: {why}"),
            Failure::Deadline => f.write_str("the call ran past its deadline"),
            Failure::Memory => f.write_str("the program asked for more memory than its room has"),
            Failure::Stack => f.write_str("the call went deeper than the stack allows"),
            Failure::Host(why) => write!(f, "the host: {why}"),
        }
    }
}

impl std::error::Error for Failure {}

/// What each room may use.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Its memory, in bytes.
    pub memory: usize,
    /// How long one call into it may run.
    pub deadline: Duration,
    /// The bytes one call's network commands may hold (what it keeps and sends): past them, the
    /// call fails at the host's limits.
    pub commands: usize,
}

impl Default for Limits {
    /// 64 MiB, 50 ms a call, and 16 MiB of commands a call (a 64 KiB message to 256
    /// connections).
    fn default() -> Limits {
        Limits { memory: 64 << 20, deadline: Duration::from_millis(50), commands: 16 << 20 }
    }
}

/// A server build, compiled once and shared by its rooms.
pub struct ServerBuild {
    module: Module,
    /// The program's pair id (`wrela_abi::PAIR_SECTION`), which its clients carry too.
    pub pair: Option<u64>,
}

impl ServerBuild {
    /// The server build in the build directory `dir`: its `server/game.wasm`.
    pub fn load(dir: &Path) -> Result<ServerBuild, Failure> {
        let path = dir.join("server").join("game.wasm");
        let wasm = std::fs::read(&path)
            .map_err(|e| Failure::Host(format!("reading {}: {e}", path.display())))?;
        ServerBuild::from_wasm(&wasm)
    }

    /// A server build from its module's bytes. Its memory must be its own, and it must export
    /// `serve`.
    pub fn from_wasm(wasm: &[u8]) -> Result<ServerBuild, Failure> {
        let module = Module::new(&ENGINE, wasm)
            .map_err(|e| Failure::Host(format!("compiling the server build: {e:#}")))?;
        let shared = module
            .imports()
            .any(|i| matches!(i.ty(), wasmtime::ExternType::Memory(m) if m.is_shared()));
        if shared || module.get_export(EXPORT_MEMORY).is_none() {
            return Err(Failure::Host(
                "this isn't a server build: its memory isn't its own".into(),
            ));
        }
        if module.get_export(EXPORT_SERVE).is_none() {
            return Err(Failure::Host(
                "this isn't a server build: it doesn't export `serve`".into(),
            ));
        }
        Ok(ServerBuild { module, pair: wrela_abi::read_pair(wasm) })
    }
}

/// What answers a room's requests to the host (`std::host::request`): given the service and
/// the bytes, the answer's bytes or why there's none. It mustn't wait.
pub type Answers = std::sync::Arc<dyn Fn(&str, &[u8]) -> Result<Vec<u8>, String> + Send + Sync>;

/// A room's part of the host.
struct RoomState {
    limit: usize,
    /// Whether the cap refused the memory the program asked for, in the current call.
    denied: bool,
    answers: Option<Answers>,
    /// Requests answered, which the program may read; and answered during the current call,
    /// which it may read from the next.
    answered: std::collections::HashMap<u32, Result<Vec<u8>, String>>,
    arriving: Vec<(u32, Result<Vec<u8>, String>)>,
    /// What the host couldn't do, which fails the call.
    refused: Option<String>,
    printed: Vec<String>,
    dropped: u64,
    /// The events (and messages' bytes) the room hasn't read.
    input: Queue,
    /// The network commands of the calls since they were last taken (`std::net`), in
    /// order: never answered. And their bytes, and the most they may hold.
    commands: Vec<Net>,
    command_bytes: usize,
    command_cap: usize,
}

/// A room's network command (`std::net`): what the host does with a call's, once it
/// returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Net {
    /// Keep a record at the end of the journal (`net/keep`).
    Keep(Vec<u8>),
    /// Replace the journal with this snapshot record (`net/compact`).
    Compact(Vec<u8>),
    /// Send bytes on a connection, once the call's records are kept (`net/send`).
    Send(u32, Vec<u8>),
    /// Close a connection, after the call's sends (`net/close`).
    Close(u32),
    /// Tag a connection, once the call's records are kept (`net/tag`): its `Connection` event
    /// carries the tag when the room starts again.
    Tag(u32, u32),
    /// Write to the server's store, committed with the call's other writes before its records
    /// are kept (`store/write`, [`crate::services`]).
    Write(Vec<u8>),
}

/// A network command from a host request's service and body, if it's one.
fn net_command(service: &str, body: &[u8]) -> Option<Result<Net, String>> {
    let conn = |b: &[u8]| -> Result<u32, String> {
        b.get(..4)
            .map(|w| u32::from_le_bytes(w.try_into().expect("four bytes")))
            .ok_or_else(|| format!("`{service}` needs a connection's number"))
    };
    Some(match service {
        "net/keep" => Ok(Net::Keep(body.to_vec())),
        "net/compact" => Ok(Net::Compact(body.to_vec())),
        "store/write" => Ok(Net::Write(body.to_vec())),
        "net/send" => conn(body).map(|c| Net::Send(c, body[4..].to_vec())),
        "net/close" => conn(body).map(Net::Close),
        "net/tag" => match body.get(4..8) {
            Some(t) if body.len() == 8 => {
                conn(body).map(|c| Net::Tag(c, u32::from_le_bytes(t.try_into().expect("four"))))
            }
            _ => Err("`net/tag` needs a connection's number and a tag".to_string()),
        },
        // Instances are the Cloudflare host's (language.md §19.5): a room here has no other
        // instance to call or send to, and no request to dispatch.
        wrela_abi::http::SERVICE_DISPATCH
        | wrela_abi::http::SERVICE_CALL
        | wrela_abi::http::SERVICE_REPLY
        | wrela_abi::http::SERVICE_SEND
        | wrela_abi::http::SERVICE_ACK
        | wrela_abi::http::SERVICE_CALL_BUILD => Err(format!(
            "`{service}`: this host runs rooms, whose instance is `server`; calls and messages between instances are the Cloudflare host's"
        )),
        _ => return None,
    })
}

impl RoomState {
    /// The host can't do what the program asked: `why` fails the call, as the host's.
    fn refuse(&mut self, why: String) -> wasmtime::Error {
        let error = wasmtime::format_err!("{why}");
        self.refused = Some(why);
        error
    }
}

impl wasmtime::ResourceLimiter for RoomState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limit {
            // An error, not a refusal: a refused `memory.grow` would make the program's
            // allocator panic, which would look like the program's own failure.
            self.denied = true;
            return Err(wasmtime::format_err!("the room's memory cap"));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

/// One room: an instance of a server build, started by `serve_init`.
pub struct Room {
    store: Store<RoomState>,
    serve: TypedFunc<f32, ()>,
    memory: wasmtime::Memory,
    limits: Limits,
    /// Whether its last call's requests were answered: its next call can read them.
    answered: bool,
}

impl Room {
    /// Starts a room of `build`: instantiates it and calls `serve_init` (if it has one), each
    /// within `limits`. `answers` answers its host requests (none: each fails).
    pub fn start(
        build: &ServerBuild,
        limits: Limits,
        answers: Option<Answers>,
    ) -> Result<Room, Failure> {
        let state = RoomState {
            limit: limits.memory,
            denied: false,
            answers,
            answered: std::collections::HashMap::new(),
            arriving: Vec::new(),
            refused: None,
            printed: Vec::new(),
            dropped: 0,
            input: Queue::default(),
            commands: Vec::new(),
            command_bytes: 0,
            command_cap: limits.commands,
        };
        let mut store = Store::new(&ENGINE, state);
        store.limiter(|s| s);
        let linker = linker().map_err(|e| Failure::Host(format!("{e:#}")))?;
        store.set_epoch_deadline(ticks(limits.deadline));
        let instance = linker
            .instantiate(&mut store, &build.module)
            .map_err(|e| classify(&mut store, None, &e))?;
        let serve = instance
            .get_typed_func::<f32, ()>(&mut store, EXPORT_SERVE)
            .map_err(|e| Failure::Host(format!("`serve`: {e:#}")))?;
        let memory = instance
            .get_memory(&mut store, EXPORT_MEMORY)
            .ok_or_else(|| Failure::Host("the program has no memory export".into()))?;
        let mut room = Room { store, serve, memory, limits, answered: false };
        if instance.get_export(&mut room.store, EXPORT_SERVE_INIT).is_some() {
            let init = instance
                .get_typed_func::<(), ()>(&mut room.store, EXPORT_SERVE_INIT)
                .map_err(|e| Failure::Host(format!("`serve_init`: {e:#}")))?;
            room.call(|store| init.call(store, ()))?;
        }
        Ok(room)
    }

    /// Wakes the room: `serve(time)`, within its deadline.
    pub fn serve(&mut self, time: f32) -> Result<(), Failure> {
        let serve = self.serve.clone();
        self.call(|store| serve.call(store, time))
    }

    /// Queues a message of `bytes` from connection `conn`: the room reads it (`std::input`) from
    /// its next call on.
    pub fn push_message(&mut self, conn: u32, bytes: Vec<u8>) {
        self.store.data_mut().input.push_message(conn, bytes);
    }

    /// Queues that connection `conn` opened, or closed; `tag` is the tag the program gave it.
    pub fn push_connection(&mut self, conn: u32, open: bool, tag: u32) {
        self.store.data_mut().input.push(Event::connection(conn, open, tag));
    }

    /// How many events wait for its next call.
    pub fn queued(&self) -> usize {
        self.store.data().input.len()
    }

    /// Whether requests its last call made were answered: its next call can read them, so it
    /// should come though no event waits.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// The network commands of its calls since the last call to this, in order.
    pub fn take_commands(&mut self) -> Vec<Net> {
        self.store.data_mut().command_bytes = 0;
        std::mem::take(&mut self.store.data_mut().commands)
    }

    /// The lines it printed since the last call to this, and how many more it printed than were
    /// kept.
    pub fn take_printed(&mut self) -> (Vec<String>, u64) {
        let s = self.store.data_mut();
        (std::mem::take(&mut s.printed), std::mem::take(&mut s.dropped))
    }

    /// How many bytes its memory holds now.
    pub fn memory_bytes(&self) -> usize {
        self.memory.data_size(&self.store)
    }

    fn call(
        &mut self,
        f: impl FnOnce(&mut Store<RoomState>) -> wasmtime::Result<()>,
    ) -> Result<(), Failure> {
        self.store.set_epoch_deadline(ticks(self.limits.deadline));
        let s = self.store.data_mut();
        s.denied = false;
        s.refused = None;
        let memory = self.memory;
        let result = f(&mut self.store).map_err(|e| classify(&mut self.store, Some(memory), &e));
        // Requests made during the call are answered now, for the next call to read; the
        // messages it was given lose their bytes.
        let s = self.store.data_mut();
        let arriving = std::mem::take(&mut s.arriving);
        self.answered = !arriving.is_empty();
        s.answered.extend(arriving);
        s.input.end_call();
        result
    }
}

/// Epochs in `d`, at least one.
fn ticks(d: Duration) -> u64 {
    (d.as_nanos() / EPOCH.as_nanos()).max(1) as u64
}

/// Why `e`, a call's error, happened: the host's limits first, then the program's own trap,
/// with its panic's message from `memory` if it panicked.
fn classify(
    store: &mut Store<RoomState>,
    memory: Option<wasmtime::Memory>,
    e: &wasmtime::Error,
) -> Failure {
    let s = store.data_mut();
    if s.denied {
        return Failure::Memory;
    }
    if let Some(why) = s.refused.take() {
        return Failure::Host(why);
    }
    match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::Interrupt) => Failure::Deadline,
        Some(wasmtime::Trap::StackOverflow) => Failure::Stack,
        Some(t) => {
            use wrela_abi::memory::{PANIC_CAP, THREAD_MAIN, panic_at, panic_message};
            let at = panic_at(THREAD_MAIN) as usize;
            let message = memory.and_then(|m| {
                let bytes = m.data(&*store).get(at..at + 4 + PANIC_CAP as usize)?.to_vec();
                // Cleared, so a later trap that isn't a panic doesn't repeat it.
                m.data_mut(&mut *store)[at..at + 4].fill(0);
                panic_message(&bytes)
            });
            Failure::Program(message.unwrap_or_else(|| t.to_string()))
        }
        // An error that isn't a trap is the host's: an unknown import, say.
        None => Failure::Host(format!("{e:#}")),
    }
}

/// The program's memory, from a host function's caller.
fn memory(c: &mut Caller<'_, RoomState>) -> wasmtime::Result<wasmtime::Memory> {
    c.get_export(EXPORT_MEMORY)
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::format_err!("the program has no memory export"))
}

/// Bytes `[at, at + len)` of the program's memory.
fn read(c: &mut Caller<'_, RoomState>, at: u32, len: u32) -> wasmtime::Result<Vec<u8>> {
    let m = memory(c)?;
    let data = m.data(&*c);
    let end = (at as usize).checked_add(len as usize).filter(|&e| e <= data.len());
    let Some(end) = end else {
        return Err(wasmtime::format_err!(
            "({at}, {len}) reaches past the end of the program's memory"
        ));
    };
    Ok(data[at as usize..end].to_vec())
}

/// The host functions a server build may import; any other import refuses to link.
fn linker() -> wasmtime::Result<Linker<RoomState>> {
    let mut linker = Linker::new(&ENGINE);
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_SUBMIT,
        |mut c: Caller<'_, RoomState>, at: u32, len: u32| {
            let batch = read(&mut c, at, len)?;
            let commands = match stream::decode(&batch) {
                Ok(commands) => commands,
                Err(e) => return Err(wasmtime::format_err!("a malformed command batch: {e}")),
            };
            for cmd in commands {
                let s = c.data_mut();
                match cmd {
                    Command::HostRequest { request, service, body } => {
                        // A network command is the host's to carry out after the call: never
                        // answered, so it holds nothing for the program to take.
                        if let Some(net) = net_command(service, body) {
                            s.command_bytes += body.len();
                            if s.command_bytes > s.command_cap {
                                let cap = s.command_cap;
                                return Err(s.refuse(format!(
                                    "a call's network commands past {cap} bytes"
                                )));
                            }
                            let net = net.map_err(|why| s.refuse(why))?;
                            s.commands.push(net);
                            continue;
                        }
                        // A room's instance is `server`, whose store it uses: answered at once.
                        if service == wrela_abi::http::SERVICE_ME {
                            let name = wrela_abi::http::SERVER_INSTANCE.as_bytes().to_vec();
                            s.answered.insert(request, Ok(name));
                            continue;
                        }
                        let answer = match &s.answers {
                            Some(answer) => answer(service, body),
                            None => Err("this host has no capability handler".to_string()),
                        };
                        s.arriving.push((request, answer));
                    }
                    other => {
                        // A server build records no GPU work and makes no other request yet: the
                        // host refuses, which fails the call as the host's.
                        let why = format!("a server host can't carry out {:?}", other.opcode());
                        return Err(s.refuse(why));
                    }
                }
            }
            Ok(())
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_REQUEST_STATUS,
        |c: Caller<'_, RoomState>, request: u32| -> i32 {
            match c.data().answered.get(&request) {
                None => -1,
                Some(Err(_)) => -2,
                Some(Ok(bytes)) => bytes.len() as i32,
            }
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_REQUEST_TAKE,
        |mut c: Caller<'_, RoomState>, request: u32, at: u32| {
            let m = memory(&mut c)?;
            let Some(answer) = c.data_mut().answered.remove(&request) else {
                return Err(wasmtime::format_err!(
                    "request_take({request}): the request isn't answered"
                ));
            };
            let bytes = answer.as_ref().map_or(&[][..], Vec::as_slice);
            if m.write(&mut c, at as usize, bytes).is_err() {
                c.data_mut().answered.insert(request, answer);
                return Err(wasmtime::format_err!(
                    "request_take({request}, {at}) reaches past the end of the program's memory"
                ));
            }
            Ok(())
        },
    )?;
    // A server has no GPU: every limit is 0.
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_LIMIT,
        |_: Caller<'_, RoomState>, _index: u32| -> i32 { 0 },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_PRINT,
        |mut c: Caller<'_, RoomState>, at: u32, len: u32| {
            let line = String::from_utf8_lossy(&read(&mut c, at, len)?).into_owned();
            let s = c.data_mut();
            if s.printed.len() < PRINTED_CAP {
                s.printed.push(line);
            } else {
                s.dropped += 1;
            }
            Ok(())
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_INPUT,
        |mut c: Caller<'_, RoomState>, at: u32, cap: u32| -> wasmtime::Result<i32> {
            let m = memory(&mut c)?;
            let n = (cap as usize).min(c.data().input.len());
            let fits = (at as usize)
                .checked_add(n * EVENT_SIZE as usize)
                .is_some_and(|end| end <= m.data_size(&c));
            if !fits {
                return Err(wasmtime::format_err!(
                    "input({at}, {cap}) reaches past the end of the program's memory"
                ));
            }
            let bytes = c.data_mut().input.take(cap);
            m.write(&mut c, at as usize, &bytes).map_err(|_| {
                wasmtime::format_err!(
                    "input({at}, {cap}) reaches past the end of the program's memory"
                )
            })?;
            Ok((bytes.len() / EVENT_SIZE as usize) as i32)
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_PAYLOAD,
        |mut c: Caller<'_, RoomState>,
         lo: u32,
         hi: u32,
         at: u32,
         cap: u32|
         -> wasmtime::Result<i32> {
            let serial = u64::from(lo) | (u64::from(hi) << 32);
            let Some(bytes) = c.data().input.payload(serial).map(<[u8]>::to_vec) else {
                return Ok(-1);
            };
            let n = bytes.len().min(cap as usize);
            let m = memory(&mut c)?;
            m.write(&mut c, at as usize, &bytes[..n]).map_err(|_| {
                wasmtime::format_err!(
                    "payload(.., {at}, {cap}) reaches past the end of the program's memory"
                )
            })?;
            Ok(bytes.len() as i32)
        },
    )?;
    // Every room reads the host's one clock, a room started again after a failure too.
    linker.func_wrap(IMPORT_MODULE, IMPORT_CLOCK, |mut c: Caller<'_, RoomState>, at: u32| {
        let bytes = crate::program::clock_seconds().to_le_bytes();
        let m = memory(&mut c)?;
        m.write(&mut c, at as usize, &bytes).map_err(|_| {
            wasmtime::format_err!("clock({at}) reaches past the end of the program's memory")
        })
    })?;
    // A phase's timing is for frames; a room ignores it (§17.3's `trace`).
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_PHASE,
        |_: Caller<'_, RoomState>, _at: u32, _len: u32| {},
    )?;
    Ok(linker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_input_copy_keeps_the_rooms_events() {
        let wasm = wat::parse_str(
            r#"(module
                (import "wrela" "input" (func $input (param i32 i32) (result i32)))
                (memory (export "memory") 1)
                (func (export "serve") (param $at f32)
                    local.get $at i32.trunc_f32_u i32.const 1 call $input drop))"#,
        )
        .expect("server module");
        let build = ServerBuild::from_wasm(&wasm).expect("server build");
        let mut room = Room::start(&build, Limits::default(), None).expect("room");
        room.push_connection(7, true, 3);
        assert!(room.serve(65536.0).is_err(), "the event does not fit");
        assert_eq!(room.queued(), 1, "a failed copy must not consume the event");
        room.serve(0.0).expect("copy the event inside memory");
        assert_eq!(room.queued(), 0);
        assert_eq!(
            &room.memory.data(&room.store)[..EVENT_SIZE as usize],
            Event::connection(7, true, 3).bytes()
        );
    }

    #[test]
    fn failed_answer_copy_keeps_the_rooms_answer() {
        let batch = stream::Encoder::new().host_request(7, "echo", &[4, 5, 6]).finish();
        let escaped: String = batch.iter().map(|b| format!("\\{b:02x}")).collect();
        let wasm = wat::parse_str(format!(
            r#"(module
                (import "wrela" "submit" (func $submit (param i32 i32)))
                (import "wrela" "request_take" (func $take (param i32 i32)))
                (memory (export "memory") 1)
                (data (i32.const 100) "{escaped}")
                (func (export "serve") (param $at f32)
                    local.get $at f32.const 0 f32.eq
                    if
                        i32.const 100 i32.const {} call $submit
                    else
                        i32.const 7 local.get $at i32.trunc_f32_u call $take
                    end))"#,
            batch.len()
        ))
        .expect("server module");
        let build = ServerBuild::from_wasm(&wasm).expect("server build");
        let answer: Answers = std::sync::Arc::new(|_, body| Ok(body.to_vec()));
        let mut room = Room::start(&build, Limits::default(), Some(answer)).expect("room");
        room.serve(0.0).expect("request an answer");
        assert!(room.serve(65536.0).is_err(), "the answer does not fit");
        room.serve(4.0).expect("copy the answer inside memory");
        assert_eq!(&room.memory.data(&room.store)[4..7], &[4, 5, 6]);
        assert!(room.store.data().answered.is_empty(), "a successful copy consumes the answer");
    }

    #[test]
    fn a_server_does_not_skip_an_invalid_initializer() {
        for init in [
            r#"(func (export "serve_init") (param i32))"#,
            r#"(func (export "serve_init") (result i32) i32.const 1)"#,
            r#"(global (export "serve_init") i32 (i32.const 1))"#,
        ] {
            let wasm = wat::parse_str(format!(
                r#"(module (memory (export "memory") 1)
                    {init}
                    (func (export "serve") (param f32)))"#,
            ))
            .expect("server module");
            let build = ServerBuild::from_wasm(&wasm).expect("server build");
            let error = match Room::start(&build, Limits::default(), None) {
                Ok(_) => panic!("the invalid initializer was skipped: {init}"),
                Err(error) => error,
            };
            assert!(matches!(&error, Failure::Host(why) if why.contains("serve_init")), "{error}");
        }
    }

    #[test]
    fn a_server_reads_a_monotonic_clock() {
        let wasm = wat::parse_str(
            r#"(module
                (import "wrela" "clock" (func $clock (param i32)))
                (memory (export "memory") 1)
                (func (export "serve") (param f32)
                    i32.const 0 call $clock))"#,
        )
        .expect("server module");
        let build = ServerBuild::from_wasm(&wasm).expect("server build");
        let mut room = Room::start(&build, Limits::default(), None).expect("room with a clock");
        room.serve(0.0).expect("first clock read");
        let read = |room: &Room| {
            f64::from_le_bytes(room.memory.data(&room.store)[..8].try_into().expect("clock"))
        };
        let first = read(&room);
        room.serve(1.0).expect("second clock read");
        let second = read(&room);
        assert!(first >= 0.0 && second >= first, "the clock went back: {first}, {second}");
        let mut later = Room::start(&build, Limits::default(), None).expect("a later room");
        later.serve(0.0).expect("another room's clock");
        assert!(read(&later) >= second, "rooms share the server's clock");
    }
}
