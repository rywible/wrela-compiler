//! The browser runtime's copy of the ABI's constants, generated so they're defined once.

use crate::{check, hash, manifest, stream};

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
            format!("  {{ name: {name:?}, bytes: {bytes}, depth: {depth} }},\n")
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
    let host_functions: String = crate::HOST_FUNCTIONS
        .iter()
        .map(|f| format!("  [{:?}, {:?}],\n", f.name, f.signature()))
        .collect();
    let [size_x, size_y, size_z] = manifest::MAX_WORKGROUP_SIZE;
    format!(
        r#"// Generated from the wrela-abi crate (runtime/abi): the one definition of the
// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin gen-ts`.

export const STREAM_VERSION = {stream_version};
export const MANIFEST_VERSION = {manifest_version};
/** The bytes `WRCS`, read as a little-endian u32. */
export const STREAM_MAGIC = 0x{magic:08x};
export const HEADER_LEN = {header_len};
export const COMMAND_HEADER_LEN = {command_header_len};
/** A pass's attachment that isn't there, and a pass's colour attachment that is the screen. */
export const NONE = 0x{none:08x};
export const SCREEN = 0x{screen:08x};

export const Opcode = {{
{opcodes}}} as const;

/** A texture's format, by its number in the stream: WebGPU's name, the bytes of one texel, and
 * whether it's a depth format. */
export const TEXTURE_FORMATS = [
{texture_formats}] as const;
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
/** Every function a program may import besides its memory, with its type as both hosts word
 * it. */
export const HOST_FUNCTIONS = [
{host_functions}] as const;
export const EXPORT_AUDIO = {export_audio:?};
/** The audio thread's rate and render quantum, and where `__audio` leaves a quantum's samples. */
export const AUDIO_SAMPLE_RATE = {audio_sample_rate};
export const AUDIO_QUANTUM = {audio_quantum};
export const AUDIO_OUT = {audio_out};
/** The workers' job's words a host writes, and how many workers a program runs at most
 * (runtime/abi `memory`). */
export const WORKERS_GENERATION = {workers_generation};
export const WORKERS_DONE = {workers_done};
export const WORKERS_FAILED = {workers_failed};
export const WORKERS_DONE_FAILED = {workers_done_failed};
export const WORKERS_SHUTDOWN = {workers_shutdown};
export const WORKERS_HELPED = {workers_helped};
export const MAX_WORKERS = {max_workers};
export const REQUEST_PENDING = {request_pending};
export const REQUEST_FAILED = {request_failed};
export const EXPORT_FRAME = {export_frame:?};
export const EXPORT_INIT = {export_init:?};
export const EXPORT_MEMORY = {export_memory:?};
export const SCREEN_FORMAT = {screen_format:?};
/** What a pipeline binds at a binding, as the manifest names it. */
export const BINDING_KINDS = [{binding_kinds}] as const;
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

/** Where a panic leaves its message: a u32 byte count, then UTF-8 (at most PANIC_CAP bytes). */
export const PANIC_MESSAGE = {panic_message};
export const PANIC_CAP = {panic_cap};
"#,
        stream_version = stream::VERSION,
        manifest_version = manifest::VERSION,
        magic = u32::from_le_bytes(stream::MAGIC),
        header_len = stream::HEADER_LEN,
        command_header_len = stream::COMMAND_HEADER_LEN,
        none = stream::NONE,
        screen = stream::SCREEN,
        import_module = crate::IMPORT_MODULE,
        import_submit = crate::IMPORT_SUBMIT,
        import_request_status = crate::IMPORT_REQUEST_STATUS,
        import_request_take = crate::IMPORT_REQUEST_TAKE,
        import_limit = crate::IMPORT_LIMIT,
        import_memory = crate::IMPORT_MEMORY,
        export_worker = crate::EXPORT_WORKER,
        import_audio = crate::IMPORT_AUDIO,
        export_audio = crate::EXPORT_AUDIO,
        audio_sample_rate = crate::AUDIO_SAMPLE_RATE,
        audio_quantum = crate::AUDIO_QUANTUM,
        audio_out = crate::memory::AUDIO_OUT,
        workers_generation = crate::memory::WORKERS_GENERATION,
        workers_done = crate::memory::WORKERS_DONE,
        workers_failed = crate::memory::WORKERS_FAILED,
        workers_done_failed = crate::memory::WORKERS_DONE_FAILED,
        workers_shutdown = crate::memory::WORKERS_SHUTDOWN,
        workers_helped = crate::memory::WORKERS_HELPED,
        max_workers = crate::memory::MAX_WORKERS,
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
        panic_message = crate::memory::PANIC_MESSAGE,
        panic_cap = crate::memory::PANIC_CAP,
    )
}

/// `items`, separated by commas.
fn list<T: std::fmt::Display>(items: impl Iterator<Item = T>) -> String {
    items.map(|x| x.to_string()).collect::<Vec<_>>().join(", ")
}
