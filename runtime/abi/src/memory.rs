//! # The program's memory
//!
//! A compiled program's linear memory, laid out by the compiler; hosts read the parts named here.
//!
//! | Addresses | What |
//! |---|---|
//! | 0 – 7 | never a valid address |
//! | [`DATA_READY`] – 15 | whether the constants are in place (the module's start function's) |
//! | [`ALLOC_STATE`] – 255 | the allocator's state (std's own) |
//! | [`WORKERS_STATE`] – 511 | the workers' job (std's own; hosts write [`WORKERS_FAILED`] and [`WORKERS_SHUTDOWN`]) |
//! | [`PANIC_MESSAGE`] – 4095 | a panic's message: a `u32` byte count, then UTF-8 |
//! | [`CMD_BASE`] – | the command buffer: a batch header, then up to [`CMD_CAP`] bytes |
//! | [`STACK_LIMIT`] – [`STACK_TOP`] | the shadow stack, which grows down |
//! | [`STACK_TOP`] – [`AUDIO_OUT`] | each worker's shadow stack, [`WORKER_STACK_SIZE`] bytes each |
//! | [`AUDIO_OUT`] – [`AUDIO_STACK_LIMIT`] | a render quantum's samples, from the audio thread |
//! | [`AUDIO_STACK_LIMIT`] – [`DATA_BASE`] | the audio thread's shadow stack |
//! | [`DATA_BASE`] – | the program's constants |
//! | then, from a page boundary | the heap, which grows up |
//!
//! The memory starts with [`HEAP_START_PAGES`] pages of heap after the constants, and grows up to
//! [`MAX_PAGES`] (1 GiB) as the heap needs. It's shared: the module imports it (`wrela.memory`),
//! and a host that runs workers gives each of its instances the same one.
//!
//! ## Workers
//!
//! A program that uses parallelism (`par_each_mut`, `par_map_reduce`) exports
//! `__worker(index: i32)`, which a host calls once on each worker thread, with an index below
//! [`MAX_WORKERS`], after instantiating the module there with the same memory. It runs jobs
//! until the host sets [`WORKERS_SHUTDOWN`] and wakes it (`memory.atomic.notify` on
//! [`WORKERS_GENERATION`]). If a worker traps, its host sets [`WORKERS_FAILED`], sets
//! [`WORKERS_DONE_FAILED`] in [`WORKERS_DONE`] (so a thread about to wait on its old value doesn't
//! wait), and wakes the thread waiting for the job (`notify` on [`WORKERS_DONE`]); that thread
//! traps in turn, with the worker's panic message if it had one.

/// A WASM page.
pub const PAGE: u32 = 65536;
/// 2 once the module's start function has copied the constants into the memory, which happens
/// once however many instances share it: 1 while one copies.
pub const DATA_READY: u32 = 8;
/// The allocator's state: std's core owns it.
pub const ALLOC_STATE: u32 = 16;
/// The workers' job: std's core owns it, but for the two words hosts write.
pub const WORKERS_STATE: u32 = 256;
/// Bumped when a job starts: workers wait on it.
pub const WORKERS_GENERATION: u32 = WORKERS_STATE;
/// How many of the job's chunks are done: the thread that started it waits on it.
pub const WORKERS_DONE: u32 = WORKERS_STATE + 20;
/// Nonzero once a worker has trapped: the host writes it.
pub const WORKERS_FAILED: u32 = WORKERS_STATE + 24;
/// The bit a host sets in [`WORKERS_DONE`] when a worker traps: it changes the value the job's
/// thread waits on, so the wake-up can't be lost.
pub const WORKERS_DONE_FAILED: u32 = 1 << 31;
/// Nonzero once the host wants its workers to return: the host writes it.
pub const WORKERS_SHUTDOWN: u32 = WORKERS_STATE + 28;
/// How many chunks the workers (not the thread that started a job) have run, ever: a host
/// reads it to show that they help.
pub const WORKERS_HELPED: u32 = WORKERS_STATE + 32;
/// The most workers a program runs on besides its own thread.
pub const MAX_WORKERS: u32 = 8;
/// Each worker's shadow stack.
pub const WORKER_STACK_SIZE: u32 = 1 << 20;
/// A panic's message: a `u32` byte count, then the message's UTF-8, cut to [`PANIC_CAP`] bytes.
/// A host reads it after a trap; the count is 0 when the trap wasn't a panic.
pub const PANIC_MESSAGE: u32 = 512;
pub const PANIC_CAP: u32 = 4096 - PANIC_MESSAGE - 4;
/// The command buffer: a batch header, then commands.
pub const CMD_BASE: u32 = 4096;
pub const CMD_CAP: u32 = 1 << 20;
/// The shadow stack: from [`STACK_TOP`] down to [`STACK_LIMIT`], 8 MiB.
pub const STACK_LIMIT: u32 = 2 << 20;
pub const STACK_SIZE: u32 = 8 << 20;
pub const STACK_TOP: u32 = STACK_LIMIT + STACK_SIZE;
/// The samples `__audio` renders: [`crate::AUDIO_QUANTUM`] `f32`s, mono.
pub const AUDIO_OUT: u32 = STACK_TOP + MAX_WORKERS * WORKER_STACK_SIZE;
/// The audio thread's shadow stack: from [`AUDIO_STACK_TOP`] down to [`AUDIO_STACK_LIMIT`].
pub const AUDIO_STACK_LIMIT: u32 = AUDIO_OUT + 4096;
pub const AUDIO_STACK_SIZE: u32 = 256 << 10;
pub const AUDIO_STACK_TOP: u32 = AUDIO_STACK_LIMIT + AUDIO_STACK_SIZE;
/// Where the constants start: after the other threads' stacks.
pub const DATA_BASE: u32 = AUDIO_STACK_TOP;
/// The most the memory grows to: 1 GiB.
pub const MAX_PAGES: u32 = 16384;
/// The heap's room when the program starts, in pages: 1 MiB.
pub const HEAP_START_PAGES: u32 = 16;

/// A panic's message, from the memory's bytes at [`PANIC_MESSAGE`] on: `None` if the count is 0
/// or the message runs past `bytes`. Bytes that aren't UTF-8 are replaced.
pub fn panic_message(bytes: &[u8]) -> Option<String> {
    let count = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?).min(PANIC_CAP) as usize;
    if count == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(bytes.get(4..4 + count)?).into_owned())
}

// A quantum's samples fit below the audio stack.
const _: () = assert!(crate::AUDIO_QUANTUM * 4 <= AUDIO_STACK_LIMIT - AUDIO_OUT);
// The command buffer and its header end below the stack.
const _: () = assert!(CMD_BASE + crate::stream::HEADER_LEN as u32 + CMD_CAP <= STACK_LIMIT);
