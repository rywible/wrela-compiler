//! The browser runtime's copy of the ABI's constants, generated so they're defined once.

use crate::{check, hash, input, manifest, stream};

/// The checked-in TypeScript module, relative to the repository root.
pub const TS_PATH: &str = "runtime/browser/src/abi.gen.ts";

/// The TypeScript module `runtime/browser/src/abi.gen.ts`.
pub fn typescript() -> String {
    let opcodes: String = stream::Opcode::ALL
        .iter()
        .map(|&op| format!("  {}: {},\n", op.name(), op as u32))
        .collect();
    let texture_formats: String = stream::TextureFormat::ALL
        .iter()
        .enumerate()
        .map(|(i, &f)| {
            assert_eq!(f as usize, i, "texture formats are numbered from 0");
            let (name, bytes, depth) = (f.name(), f.bytes_per_texel(), f.is_depth());
            let (storable, filterable, uint) = (f.storable(), f.filterable(), f.is_uint());
            format!(
                "  {{ name: {name:?}, bytes: {bytes}, depth: {depth}, storable: {storable}, filterable: {filterable}, uint: {uint} }},\n"
            )
        })
        .collect();
    let compares: String = stream::Compare::ALL
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            assert_eq!(c as usize, i + 1, "comparisons are numbered from 1");
            format!(", {:?}", c.name())
        })
        .collect();
    let binding_kinds = list(manifest::BindingKind::ALL.iter().map(|k| format!("{:?}", k.name())));
    let binding_kind_traits: String = manifest::BindingKind::ALL
        .iter()
        .map(|k| {
            let (name, format, storage, three) = (k.name(), k.has_format(), k.is_storage(), k.is_3d());
            format!("  {{ name: {name:?}, has_format: {format}, is_storage: {storage}, is_3d: {three} }},\n")
        })
        .collect();
    let host_functions: String = crate::HOST_FUNCTIONS
        .iter()
        .map(|f| format!("  [{:?}, {:?}],\n", f.name, f.signature()))
        .collect();
    let event_kinds: String =
        input::EventKind::ALL.iter().map(|&k| format!("  {}: {},\n", k.name(), k as u32)).collect();
    let keys = list(input::KEYS.iter().map(|k| format!("{k:?}")));
    let buttons = list(input::BUTTONS.iter().map(|b| format!("{b:?}")));
    let texture_writable = stream::WRITABLE;
    let max_texture_3d = stream::MAX_TEXTURE_3D;
    let [size_x, size_y, size_z] = manifest::MAX_WORKGROUP_SIZE;
    format!(
        r#"// Generated from the wrela-abi crate (runtime/abi): the one definition of the
// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin generate`.

export const STREAM_VERSION = {stream_version};
export const MANIFEST_VERSION = {manifest_version};
/** The bytes `WRCS`, read as a little-endian u32. */
export const STREAM_MAGIC = 0x{magic:08x};
export const HEADER_LEN = {header_len};
export const COMMAND_HEADER_LEN = {command_header_len};
/** A pass's attachment that isn't there, and a pass's colour attachment that is the screen. */
export const NONE = 0x{none:08x};
export const SCREEN = 0x{screen:08x};
/** The upload ring's starting size, and the largest write it takes: a bigger one goes through
 * the queue, between submissions. */
export const UPLOAD_START = {upload_start};
export const UPLOAD_MAX = {upload_max};

export const Opcode = {{
{opcodes}}} as const;

/** A texture's format, by its number in the stream: WebGPU's name, the bytes of one texel, and
 * whether it's a depth format. */
export const TEXTURE_FORMATS = [
{texture_formats}] as const;
/** `CreateTexture`'s format word with this bit set: kernels write the texture. */
export const TEXTURE_WRITABLE = {texture_writable};
/** The most texels a side of a 3D texture. */
export const MAX_TEXTURE_3D = {max_texture_3d};
/** A comparison sampler's test, by its number in the stream: WebGPU's name (0 is a sampler that
 * doesn't compare). */
export const COMPARES = [null{compares}] as const;

export const IMPORT_MODULE = {import_module:?};
export const IMPORT_SUBMIT = {import_submit:?};
export const IMPORT_REQUEST_STATUS = {import_request_status:?};
export const IMPORT_REQUEST_TAKE = {import_request_take:?};
export const IMPORT_LIMIT = {import_limit:?};
export const IMPORT_MEMORY = {import_memory:?};
export const EXPORT_WORKER = {export_worker:?};
export const IMPORT_AUDIO = {import_audio:?};
export const IMPORT_INPUT = {import_input:?};
export const IMPORT_TICK = {import_tick:?};
export const EXPORT_TICK = {export_tick:?};
/** Hot reload: what a host calls to change a lifted literal, and keeps for the next build. */
export const EXPORT_LIFT_SET = {export_lift_set:?};
export const IMPORT_KEEP = {import_keep:?};
export const IMPORT_KEPT = {import_kept:?};
/** Input (runtime/abi `input`): an event is EVENT_SIZE bytes, six words: its kind, its
 * modifiers, then four words that depend on the kind. */
export const EVENT_SIZE = {event_size};
export const EventKind = {{
{event_kinds}}} as const;
export const MODIFIER_SHIFT = {shift};
export const MODIFIER_CONTROL = {control};
export const MODIFIER_ALT = {alt};
export const MODIFIER_META = {meta};
/** A pointer's buttons, by number, as a script names them. */
export const BUTTONS = [{buttons}] as const;
/** The physical keys, by number: DOM's `KeyboardEvent.code` (0: one not listed). */
export const KEYS = [{keys}] as const;
/** Every function a program may import besides its memory, with its type as both hosts word
 * it. */
export const HOST_FUNCTIONS = [
{host_functions}] as const;
export const EXPORT_AUDIO = {export_audio:?};
/** The audio thread's rate and render quantum, and where `__audio` leaves a quantum's samples. */
export const AUDIO_SAMPLE_RATE = {audio_sample_rate};
export const AUDIO_QUANTUM = {audio_quantum};
export const AUDIO_OUT = {audio_out};
/** The threads (runtime/abi `memory`): their numbers, their blocks, and the words of a block and
 * of a job slot a host reads and writes. */
export const THREAD_MAIN = {thread_main};
export const THREAD_TICK = {thread_tick};
export const THREAD_AUDIO = {thread_audio};
export const THREAD_HELPER0 = {thread_helper0};
export const MAX_WORKERS = {max_workers};
export const THREAD_BLOCKS = {thread_blocks};
export const THREAD_BLOCK_SIZE = {thread_block_size};
export const THREAD_BLOCKS_END = {thread_blocks_end};
export const JOB_DONE = {job_done};
export const JOB_FAILED = {job_failed};
export const JOB_DONE_FAILED = {job_done_failed};
export const RUNNING = {running};
export const JOB_SLOTS = {job_slots};
export const JOB_SLOTS_END = {job_slots_end};
export const SLOT_STATE = {slot_state};
export const SLOT_THREAD = {slot_thread};
export const SLOT_FAILED = {slot_failed};
/** How many chunks the helpers have run (one of their shared words). */
export const PAR_HELPED = {par_helped};
/** The helpers' wake count and shutdown flag: a host stops a program's helpers with them. */
export const PAR_WAKE = {par_wake};
export const PAR_SHUTDOWN = {par_shutdown};
/** The ticker's words (runtime/abi `memory`), and its records' region. */
export const TICK_WANT_HASH = {tick_want_hash};
export const TICK_HASH = {tick_hash};
export const TICK_ORIGIN = {tick_origin};
export const TICK_RECORDS = {tick_records};
export const MAX_TICK_RECORDS = {max_tick_records};
/** The tick log (runtime/abi `ticks`): its magic, the bytes `WRTL` read as a little-endian
 * u32. */
export const TICK_LOG_MAGIC = 0x{tick_log_magic:08x};
export const TICK_LOG_VERSION = {tick_log_version};
export const TICK_LOG_HEADER_LEN = {tick_log_header_len};
export const REQUEST_PENDING = {request_pending};
export const REQUEST_FAILED = {request_failed};
export const EXPORT_FRAME = {export_frame:?};
export const EXPORT_INIT = {export_init:?};
export const EXPORT_MEMORY = {export_memory:?};
export const SCREEN_FORMAT = {screen_format:?};
/** What a pipeline binds at a binding, as the manifest names it. */
export const BINDING_KINDS = [{binding_kinds}] as const;
/** What each binding kind is (runtime/abi `BindingKind`): a colour texture, which names its
 * format; a texture kernels write (a storage texture); a 3D texture. */
export const BINDING_KIND_TRAITS = [
{binding_kind_traits}] as const;
/** `minStorageBufferOffsetAlignment`: where a bound range of a buffer may start. */
export const BINDING_OFFSET_ALIGNMENT = {binding_offset_alignment};
export const MAX_WORKGROUP_SIZE = [{size_x}, {size_y}, {size_z}];
export const MAX_WORKGROUP_INVOCATIONS = {max_invocations};
export const MAX_STORAGE_BUFFERS_PER_STAGE = {max_storage_buffers};
export const MAX_UNIFORM_BUFFER_BINDING_SIZE = {max_uniform_size};
/** WebGPU's default limits, in `wrela.limit`'s order (runtime/abi `Limits`). */
export const DEFAULT_LIMITS = [{default_limits}];

export const FNV_OFFSET = 0x{fnv_offset:016x}n;
export const FNV_PRIME = 0x{fnv_prime:016x}n;

/** Where in a thread's block a panic leaves its message: a u32 byte count, then UTF-8 (at most
 * PANIC_CAP bytes). */
export const PANIC = {panic};
export const PANIC_CAP = {panic_cap};
"#,
        stream_version = stream::VERSION,
        manifest_version = manifest::VERSION,
        magic = u32::from_le_bytes(stream::MAGIC),
        header_len = stream::HEADER_LEN,
        command_header_len = stream::COMMAND_HEADER_LEN,
        none = stream::NONE,
        screen = stream::SCREEN,
        upload_start = stream::UPLOAD_START,
        upload_max = stream::UPLOAD_MAX,
        import_module = crate::IMPORT_MODULE,
        import_submit = crate::IMPORT_SUBMIT,
        import_request_status = crate::IMPORT_REQUEST_STATUS,
        import_request_take = crate::IMPORT_REQUEST_TAKE,
        import_limit = crate::IMPORT_LIMIT,
        import_memory = crate::IMPORT_MEMORY,
        export_worker = crate::EXPORT_WORKER,
        import_audio = crate::IMPORT_AUDIO,
        import_input = input::IMPORT_INPUT,
        import_tick = crate::IMPORT_TICK,
        export_tick = crate::EXPORT_TICK,
        export_lift_set = crate::EXPORT_LIFT_SET,
        import_keep = crate::IMPORT_KEEP,
        import_kept = crate::IMPORT_KEPT,
        event_size = input::EVENT_SIZE,
        shift = input::SHIFT,
        control = input::CONTROL,
        alt = input::ALT,
        meta = input::META,
        export_audio = crate::EXPORT_AUDIO,
        audio_sample_rate = crate::AUDIO_SAMPLE_RATE,
        audio_quantum = crate::AUDIO_QUANTUM,
        audio_out = crate::memory::AUDIO_OUT,
        thread_main = crate::memory::THREAD_MAIN,
        thread_tick = crate::memory::THREAD_TICK,
        thread_audio = crate::memory::THREAD_AUDIO,
        thread_helper0 = crate::memory::THREAD_HELPER0,
        max_workers = crate::memory::MAX_WORKERS,
        thread_blocks = crate::memory::THREAD_BLOCKS,
        thread_block_size = crate::memory::THREAD_BLOCK_SIZE,
        thread_blocks_end = crate::memory::THREAD_BLOCKS_END,
        job_done = crate::memory::JOB_DONE,
        job_failed = crate::memory::JOB_FAILED,
        job_done_failed = crate::memory::JOB_DONE_FAILED,
        running = crate::memory::RUNNING,
        job_slots = crate::memory::JOB_SLOTS,
        job_slots_end = crate::memory::JOB_SLOTS_END,
        slot_state = crate::memory::SLOT_STATE,
        slot_thread = crate::memory::SLOT_THREAD,
        slot_failed = crate::memory::SLOT_FAILED,
        par_helped = crate::memory::PAR_HELPED,
        par_wake = crate::memory::PAR_WAKE,
        par_shutdown = crate::memory::PAR_SHUTDOWN,
        tick_want_hash = crate::memory::TICK_WANT_HASH,
        tick_hash = crate::memory::TICK_HASH,
        tick_origin = crate::memory::TICK_ORIGIN,
        tick_records = crate::memory::TICK_RECORDS,
        max_tick_records = crate::memory::MAX_TICK_RECORDS,
        tick_log_magic = u32::from_le_bytes(crate::ticks::MAGIC),
        tick_log_version = crate::ticks::VERSION,
        tick_log_header_len = crate::ticks::HEADER_LEN,
        request_pending = crate::REQUEST_PENDING,
        request_failed = crate::REQUEST_FAILED,
        export_frame = crate::EXPORT_FRAME,
        export_init = crate::EXPORT_INIT,
        export_memory = crate::EXPORT_MEMORY,
        screen_format = manifest::SCREEN_FORMAT,
        binding_offset_alignment = check::BINDING_OFFSET_ALIGNMENT,
        max_invocations = manifest::MAX_WORKGROUP_INVOCATIONS,
        max_storage_buffers = manifest::MAX_STORAGE_BUFFERS_PER_STAGE,
        max_uniform_size = manifest::MAX_UNIFORM_BUFFER_BINDING_SIZE,
        default_limits = list((0..crate::Limits::COUNT).map(|i| crate::Limits::DEFAULT.get(i))),
        fnv_offset = hash::FNV_OFFSET,
        fnv_prime = hash::FNV_PRIME,
        panic = crate::memory::PANIC,
        panic_cap = crate::memory::PANIC_CAP,
    )
}

/// `items`, separated by commas.
fn list<T: std::fmt::Display>(items: impl Iterator<Item = T>) -> String {
    items.map(|x| x.to_string()).collect::<Vec<_>>().join(", ")
}
