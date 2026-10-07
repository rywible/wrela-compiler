//! # The program's memory
//!
//! A compiled program's linear memory, laid out by the compiler; hosts read the parts named here.
//!
//! | Addresses | What |
//! |---|---|
//! | 0 – 7 | never a valid address |
//! | [`DATA_READY`] – 15 | whether the constants are in place (the module's start function's) |
//! | [`ALLOC_STATE`] – 255 | the allocator's state (std's own) |
//! | [`PAR_STATE`] – 511 | the helpers' shared words (std's own; hosts write [`PAR_SHUTDOWN`] and [`PAR_HOLD`]) |
//! | [`TICK_STATE`] – 4095 | the ticker's words, which hosts and std's `std::tick` share |
//! | [`CMD_BASE`] – | the command buffer: a batch header, then up to [`CMD_CAP`] bytes |
//! | [`THREAD_BLOCKS`] – | a block for each thread ([`THREADS`] of them, [`THREAD_BLOCK_SIZE`] bytes each) |
//! | [`JOB_SLOTS`] – | the long jobs in flight (`std::par::job`), [`JOB_SLOT_COUNT`] slots |
//! | [`TICK_RECORDS`] – | the records of the tick about to run: a `u32` count, then up to [`MAX_TICK_RECORDS`] events |
//! | [`AUDIO_OUT`] – | a render quantum's samples, from the audio thread |
//! | [`STACK_LIMIT`] – [`STACK_TOP`] | the program's thread's shadow stack, which grows down |
//! | [`STACK_TOP`] – [`DATA_BASE`] | every other thread's shadow stack, [`THREAD_STACK_SIZE`] bytes each |
//! | [`DATA_BASE`] – | the program's constants |
//! | then, from a page boundary | the heap, which grows up |
//!
//! The memory starts with [`HEAP_START_PAGES`] pages of heap after the constants, and grows up to
//! [`MAX_PAGES`] (1 GiB) as the heap needs. It's shared: the module imports it (`wrela.memory`),
//! and every thread's instance of the module gets the same one.
//!
//! ## Threads
//!
//! A program runs on up to [`THREADS`] threads, each numbered: [`THREAD_MAIN`], the program's
//! own (the one that calls `init`, `frame` and the other exports); [`THREAD_TICK`], its ticker's
//! (`std::tick`); [`THREAD_AUDIO`], its voice's (`std::audio`); and [`THREAD_HELPER0`] on, its
//! helpers, at most [`MAX_WORKERS`]. Each thread but the program's runs its own instance of the
//! module, on the same memory, and enters it through a **thread entry**: an export std's
//! unsafe core declares (`@thread_entry`, language.md §17), named `__` and the function's name,
//! whose first argument is the thread's number. The entry runs on that thread's own stack and
//! uses that thread's block. Its other arguments are `u32`s:
//!
//! | Export | Thread | Arguments after the thread's number |
//! |---|---|---|
//! | `__worker` | each helper | none: it runs jobs until [`PAR_SHUTDOWN`] |
//! | `__audio` | the voice's | the task and context `wrela.audio` gave: one render quantum |
//! | `__tick` | the ticker's | the task and context `wrela.tick` gave, and the tick's number |
//!
//! ## A thread's block
//!
//! Each thread has a block of [`THREAD_BLOCK_SIZE`] bytes ([`thread_block`]): the parallel job
//! it started ([`JOB_GENERATION`] to [`JOB_FAILED`]), how deep it is in work it runs for others
//! ([`DEPTH`]), what it's running for another thread ([`RUNNING`]), how many blocks it has
//! allocated ([`ALLOCATIONS`]), how many times it waited at a job's join ([`JOIN_WAITS`]), how
//! many times it found the allocator's lock taken and the tries it spun on it ([`LOCK_WAITS`],
//! [`LOCK_SPINS`]), and its panic message ([`PANIC`]).
//!
//! ## Helpers
//!
//! A program that uses parallelism exports `__worker`. A host calls it once on each helper,
//! with numbers from [`THREAD_HELPER0`], after instantiating the module there. It runs chunks of
//! any thread's parallel job and long jobs until the host sets [`PAR_SHUTDOWN`] and wakes it
//! (`memory.atomic.notify` on [`PAR_WAKE`]). If a helper traps, its host reads the helper's
//! [`RUNNING`] word and tells the thread that waits:
//! - a thread block's job ([`JOB_GENERATION`]'s address): it stores the helper's number plus 1 in
//!   [`JOB_FAILED`], sets [`JOB_DONE_FAILED`] in [`JOB_DONE`] (so a thread about to wait on its
//!   old value doesn't wait) and wakes the thread waiting there;
//! - a job slot's address: it stores the helper's number in the slot's [`SLOT_THREAD`], stores
//!   [`SLOT_FAILED`] in its [`SLOT_STATE`] and wakes the thread waiting there.
//!
//! That thread traps in turn, with the helper's panic message, which std copies into its own
//! block. The helper that trapped runs nothing more.

/// A WASM page.
pub const PAGE: u32 = 65536;
/// 2 once the module's start function has copied the constants into the memory, which happens
/// once however many instances share it: 1 while one copies.
pub const DATA_READY: u32 = 8;
/// The allocator's state: std's core owns it.
pub const ALLOC_STATE: u32 = 16;

/// The helpers' shared words.
pub const PAR_STATE: u32 = 256;
/// Bumped whenever work appears (a parallel job starts, a long job is queued): helpers wait on
/// it.
pub const PAR_WAKE: u32 = PAR_STATE;
/// Nonzero once the host wants its helpers to return: the host writes it.
pub const PAR_SHUTDOWN: u32 = PAR_STATE + 4;
/// How many chunks the helpers (not the threads that started their jobs) have run, ever: a host
/// reads it to show that they help.
pub const PAR_HELPED: u32 = PAR_STATE + 8;
/// Microseconds a helper holds back each long job's result once it's ready: 0 normally. A
/// test writes it to slow jobs down, so the threads that take them wait.
pub const PAR_HOLD: u32 = PAR_STATE + 12;

/// The ticker's words.
pub const TICK_STATE: u32 = 512;
/// Nonzero when the host wants the ticker's state hash after each step (a tick log, test
/// mode): the host writes it.
pub const TICK_WANT_HASH: u32 = TICK_STATE;
/// The state hash the ticker reported last, a `u64`: after `start`, the first world's; after
/// each step, the stepped world's. std writes it when [`TICK_WANT_HASH`] is set.
pub const TICK_HASH: u32 = TICK_STATE + 8;
/// The sim clock's origin, an `f64`: the frame time, in seconds, at which tick 0 was due. The
/// host writes it, and moves it forward when it drops ticks or the page was hidden.
pub const TICK_ORIGIN: u32 = TICK_STATE + 16;

/// The command buffer: a batch header, then commands.
pub const CMD_BASE: u32 = 4096;
pub const CMD_CAP: u32 = 1 << 20;

/// The most threads a program runs on: its own, its ticker's, its voice's and its helpers.
pub const THREADS: u32 = 3 + MAX_WORKERS;
/// The program's thread.
pub const THREAD_MAIN: u32 = 0;
/// The ticker's thread (`std::tick`).
pub const THREAD_TICK: u32 = 1;
/// The voice's thread (`std::audio`).
pub const THREAD_AUDIO: u32 = 2;
/// The first helper's thread: helper `i` is `THREAD_HELPER0 + i`.
pub const THREAD_HELPER0: u32 = 3;
/// The most helpers a program runs on besides its own thread.
pub const MAX_WORKERS: u32 = 8;

/// The threads' blocks.
pub const THREAD_BLOCKS: u32 = 0x11_0000;
pub const THREAD_BLOCK_SIZE: u32 = 8192;
/// A thread's parallel job (`par_each_mut`, `par_map_reduce`): its generation, bumped when the
/// thread starts one; helpers claim its chunks through the ticket (the generation's low 16
/// bits, then the next chunk).
pub const JOB_GENERATION: u32 = 0;
pub const JOB_TICKET: u32 = 4;
pub const JOB_TASK: u32 = 8;
pub const JOB_CONTEXT: u32 = 12;
pub const JOB_CHUNKS: u32 = 16;
/// How many of the job's chunks are done: the thread that started it waits on it.
pub const JOB_DONE: u32 = 20;
/// 0, or the number of a helper that trapped running one of the job's chunks, plus 1: the
/// host writes it.
pub const JOB_FAILED: u32 = 24;
/// The bit a host sets in [`JOB_DONE`] when a helper traps: it changes the value the job's
/// thread waits on, so the wake-up can't be lost.
pub const JOB_DONE_FAILED: u32 = 1 << 31;
/// How many chunks of other threads' jobs this thread is running, one inside another: a
/// parallel job started inside a chunk runs on this thread alone.
pub const DEPTH: u32 = 28;
/// What this thread is running for another: the address of a thread's job
/// ([`JOB_GENERATION`]'s), or of a job slot; 0 for nothing. A host reads it after a helper traps.
pub const RUNNING: u32 = 32;
/// How many blocks this thread has allocated (`std::alloc::allocations`).
pub const ALLOCATIONS: u32 = 36;
/// How many times this thread has waited at a `Job::join` for a job a helper was still running:
/// on the ticker's thread, the sim waiting for an answer due (`std::par::Job::join`). Hosts log
/// it.
pub const JOIN_WAITS: u32 = 40;
/// How many times this thread found the allocator's lock taken (`std::alloc`): hosts measure
/// the time threads lose to one another on the heap with it.
pub const LOCK_WAITS: u32 = 44;
/// How many tries this thread spun on the allocator's lock, over all its waits.
pub const LOCK_SPINS: u32 = 48;
/// A panic's message: a `u32` byte count, then the message's UTF-8, cut to [`PANIC_CAP`] bytes.
/// A host reads it after a trap on the thread; the count is 0 when the trap wasn't a panic.
pub const PANIC: u32 = 4096;
pub const PANIC_CAP: u32 = THREAD_BLOCK_SIZE - PANIC - 4;

/// The address of thread `thread`'s block.
pub const fn thread_block(thread: u32) -> u32 {
    THREAD_BLOCKS + thread * THREAD_BLOCK_SIZE
}

/// The end of the threads' blocks.
pub const THREAD_BLOCKS_END: u32 = thread_block(THREADS);

/// Where thread `thread`'s panic message is: [`PANIC`] in its block.
pub const fn panic_at(thread: u32) -> u32 {
    thread_block(thread) + PANIC
}

/// The long jobs in flight (`std::par::job`): a slot each, [`JOB_SLOT_SIZE`] bytes.
pub const JOB_SLOTS: u32 = 0x12_8000;
pub const JOB_SLOT_COUNT: u32 = 256;
pub const JOB_SLOT_SIZE: u32 = 16;
/// The end of the job slots.
pub const JOB_SLOTS_END: u32 = JOB_SLOTS + JOB_SLOT_COUNT * JOB_SLOT_SIZE;
/// A slot's state: [`SLOT_FREE`], being filled, queued, running, done or [`SLOT_FAILED`].
pub const SLOT_STATE: u32 = 0;
/// The thread a job ran on: the host writes it when the job trapped.
pub const SLOT_THREAD: u32 = 12;
pub const SLOT_FREE: u32 = 0;
pub const SLOT_FAILED: u32 = 5;

/// The records of the tick about to run (`std::tick`): a `u32` count, then each an input event
/// ([`crate::input`]'s layout), oldest first. The host writes them before calling `__tick`.
pub const TICK_RECORDS: u32 = 0x12_C000;
/// The most records a tick takes: a host keeps the rest for the next tick.
pub const MAX_TICK_RECORDS: u32 = 256;

/// The samples `__audio` renders: [`crate::AUDIO_QUANTUM`] `f32`s, mono.
pub const AUDIO_OUT: u32 = 0x13_0000;

/// The program's thread's shadow stack: from [`STACK_TOP`] down to [`STACK_LIMIT`], 16 MiB.
pub const STACK_LIMIT: u32 = 2 << 20;
pub const STACK_SIZE: u32 = 16 << 20;
pub const STACK_TOP: u32 = STACK_LIMIT + STACK_SIZE;
/// Each other thread's shadow stack.
pub const THREAD_STACK_SIZE: u32 = 1 << 20;
/// Where the constants start: after the threads' stacks.
pub const DATA_BASE: u32 = STACK_TOP + (THREADS - 1) * THREAD_STACK_SIZE;
/// The most the memory grows to: 1 GiB.
pub const MAX_PAGES: u32 = 16384;
/// The heap's room when the program starts, in pages: 1 MiB.
pub const HEAP_START_PAGES: u32 = 16;

/// A panic's message, from the memory's bytes at a thread's [`PANIC`] on: `None` if the count is
/// 0 or the message runs past `bytes`. Bytes that aren't UTF-8 are replaced.
pub fn panic_message(bytes: &[u8]) -> Option<String> {
    let count = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?).min(PANIC_CAP) as usize;
    if count == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(bytes.get(4..4 + count)?).into_owned())
}

// The regions don't overlap, and each fits below the next.
const _: () = assert!(TICK_STATE + 24 <= CMD_BASE);
const _: () = assert!(CMD_BASE + crate::stream::HEADER_LEN as u32 + CMD_CAP <= THREAD_BLOCKS);
const _: () = assert!(THREAD_BLOCKS_END <= JOB_SLOTS);
const _: () = assert!(JOB_SLOTS_END <= TICK_RECORDS);
const _: () = assert!(TICK_RECORDS + 4 + MAX_TICK_RECORDS * crate::input::EVENT_SIZE <= AUDIO_OUT);
const _: () = assert!(AUDIO_OUT + crate::AUDIO_QUANTUM * 4 <= STACK_LIMIT);
const _: () = assert!(DATA_BASE.is_multiple_of(16));
