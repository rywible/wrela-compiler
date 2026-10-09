//! std's copy of the ABI's numbers (`compiler/std/abi.wrela`), generated so they're defined once:
//! std is wrela, and can't import this crate.

use crate::memory::*;
use crate::stream::{Compare, TextureFormat};

/// The checked-in wrela module, relative to the repository root.
pub const WRELA_PATH: &str = "compiler/std/abi.wrela";

/// Every `u32` std reads, each with its doc comment there.
const NUMBERS: &[(&str, u32, &str)] = &[
    ("ALLOC_STATE", ALLOC_STATE, "The allocator's state: std's own words, from here on."),
    ("PAR_WAKE", PAR_WAKE, "Bumped whenever work appears: the helpers wait on it."),
    ("PAR_SHUTDOWN", PAR_SHUTDOWN, "Nonzero once the host wants its helpers to return."),
    ("PAR_HELPED", PAR_HELPED, "How many chunks the helpers have run, ever."),
    ("PAR_HOLD", PAR_HOLD, "Microseconds a helper holds back each long job's result (tests)."),
    ("TICK_WANT_HASH", TICK_WANT_HASH, "Nonzero when the host wants the ticker's state hash."),
    ("TICK_HASH", TICK_HASH, "The state hash the ticker reported last, a `u64`."),
    ("TICK_ORIGIN", TICK_ORIGIN, "The sim clock's origin, an `f64`: when tick 0 was due."),
    ("THREADS", THREADS, "The most threads a program runs on."),
    ("THREAD_BLOCKS", THREAD_BLOCKS, "The threads' blocks, one after another."),
    ("THREAD_BLOCK_SIZE", THREAD_BLOCK_SIZE, "The bytes of each thread's block."),
    ("JOB_GENERATION", JOB_GENERATION, "A block's parallel job: its generation."),
    ("JOB_TICKET", JOB_TICKET, "Its ticket: the generation's low 16 bits, then the next chunk."),
    ("JOB_TASK", JOB_TASK, "Its task."),
    ("JOB_CONTEXT", JOB_CONTEXT, "Its task's context."),
    ("JOB_CHUNKS", JOB_CHUNKS, "How many chunks it has."),
    ("JOB_DONE", JOB_DONE, "How many of its chunks are done: its thread waits on it."),
    ("JOB_FAILED", JOB_FAILED, "0, or 1 + the number of a helper that trapped in a chunk."),
    ("JOB_DONE_FAILED", JOB_DONE_FAILED, "The bit a host sets in `JOB_DONE` when a helper traps."),
    ("DEPTH", DEPTH, "How many chunks of other threads' jobs a thread runs, one inside another."),
    ("RUNNING", RUNNING, "What a thread runs for another: a block's job, a slot, or 0."),
    ("ALLOCATIONS", ALLOCATIONS, "How many blocks a thread has allocated."),
    ("JOIN_WAITS", JOIN_WAITS, "How many times a thread waited at a join for a job a helper ran."),
    ("LOCK_WAITS", LOCK_WAITS, "How many times a thread found the allocator's lock taken."),
    ("LOCK_SPINS", LOCK_SPINS, "How many tries a thread spun on the allocator's lock."),
    ("PANIC", PANIC, "Where in a block a panic's message is: a `u32` byte count, then UTF-8."),
    ("JOB_SLOTS", JOB_SLOTS, "The long jobs' slots."),
    ("JOB_SLOT_COUNT", JOB_SLOT_COUNT, "How many slots there are."),
    ("JOB_SLOT_SIZE", JOB_SLOT_SIZE, "The bytes of each slot."),
    ("SLOT_THREAD", SLOT_THREAD, "The thread a slot's job ran on: the host writes it on a trap."),
    ("SLOT_FREE", SLOT_FREE, "A slot's state when it holds no job."),
    ("SLOT_FAILED", SLOT_FAILED, "A slot's state when its job trapped: the host writes it."),
    ("TICK_RECORDS", TICK_RECORDS, "The next tick's records: a `u32` count, then input events."),
    ("MAX_TICK_RECORDS", MAX_TICK_RECORDS, "The most records a tick takes."),
    ("EVENT_SIZE", crate::input::EVENT_SIZE, "The bytes of an input event."),
    ("AUDIO_OUT", AUDIO_OUT, "Where `__audio` leaves a quantum's samples."),
    ("AUDIO_SAMPLE_RATE", crate::AUDIO_SAMPLE_RATE, "The audio thread's sample rate, in hertz."),
    ("AUDIO_QUANTUM", crate::AUDIO_QUANTUM, "How many samples one `__audio` call renders."),
    (
        "MAX_TEXTURE_3D",
        crate::stream::MAX_TEXTURE_3D,
        "The widest, tallest and deepest 3D texture.",
    ),
    (
        "TEXTURE_WRITABLE",
        crate::stream::WRITABLE,
        "A texture's format code with this bit set: kernels write its texels.",
    ),
];

/// The same, for the `i32`s.
const SIGNED: &[(&str, i32, &str)] = &[
    ("REQUEST_PENDING", crate::REQUEST_PENDING, "A request's status while it isn't answered."),
    ("REQUEST_FAILED", crate::REQUEST_FAILED, "A request's status once it has failed."),
];

/// The wrela module `compiler/std/abi.wrela`.
pub fn wrela() -> String {
    let mut out = String::from(
        "// The ABI's numbers that std shares with the hosts (runtime/abi), for std's modules alone.
// Generated from the wrela-abi crate: don't edit; run `cargo run -p wrela-abi --bin generate`.
",
    );
    for (name, value, doc) in NUMBERS {
        out += &format!("\n/// {doc}\npub(package) const {name}: u32 = {value}\n");
    }
    for (name, value, doc) in SIGNED {
        out += &format!("\n/// {doc}\npub(package) const {name}: i32 = {value}\n");
    }
    // Each texture format's code and its bytes a texel, and each comparison's code, named as
    // WebGPU names them (`rgba8unorm` is `FORMAT_RGBA8UNORM`, `less-equal` `COMPARE_LESS_EQUAL`).
    let upper = |name: &str| name.replace('-', "_").to_uppercase();
    for f in TextureFormat::ALL {
        let (name, code, bytes) = (f.name(), f as u32, f.bytes_per_texel());
        let n = upper(name);
        out += &format!(
            "\n/// The stream's code for the texture format `{name}`.\npub(package) const FORMAT_{n}: u32 = {code}\n"
        );
        out += &format!(
            "\n/// The bytes of a texel of `{name}`.\npub(package) const FORMAT_{n}_BYTES: u32 = {bytes}\n"
        );
    }
    for c in Compare::ALL {
        let (name, code) = (c.name(), c as u32);
        let n = upper(name);
        out += &format!(
            "\n/// The stream's code for the comparison `{name}`.\npub(package) const COMPARE_{n}: u32 = {code}\n"
        );
    }
    out
}
