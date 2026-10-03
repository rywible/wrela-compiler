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

export const Opcode = {{
{opcodes}}} as const;

export const IMPORT_MODULE = {import_module:?};
export const IMPORT_SUBMIT = {import_submit:?};
export const EXPORT_FRAME = {export_frame:?};
export const EXPORT_MEMORY = {export_memory:?};
export const SCREEN_FORMAT = {screen_format:?};
export const MAX_WORKGROUP_SIZE = [{size_x}, {size_y}, {size_z}];
export const MAX_WORKGROUP_INVOCATIONS = {max_invocations};
export const MAX_STORAGE_BUFFERS_PER_STAGE = {max_storage_buffers};
export const MAX_UNIFORM_BUFFER_BINDING_SIZE = {max_uniform_size};
export const MAX_BUFFER_SIZE = {max_buffer_size};
export const MAX_WORKGROUPS_PER_DIMENSION = {max_workgroups};

export const FNV_OFFSET = 0x{fnv_offset:016x}n;
export const FNV_PRIME = 0x{fnv_prime:016x}n;
"#,
        stream_version = stream::VERSION,
        manifest_version = manifest::VERSION,
        magic = u32::from_le_bytes(stream::MAGIC),
        header_len = stream::HEADER_LEN,
        command_header_len = stream::COMMAND_HEADER_LEN,
        import_module = crate::IMPORT_MODULE,
        import_submit = crate::IMPORT_SUBMIT,
        export_frame = crate::EXPORT_FRAME,
        export_memory = crate::EXPORT_MEMORY,
        screen_format = manifest::SCREEN_FORMAT,
        max_invocations = manifest::MAX_WORKGROUP_INVOCATIONS,
        max_storage_buffers = manifest::MAX_STORAGE_BUFFERS_PER_STAGE,
        max_uniform_size = manifest::MAX_UNIFORM_BUFFER_BINDING_SIZE,
        max_buffer_size = check::MAX_BUFFER_SIZE,
        max_workgroups = check::MAX_WORKGROUPS_PER_DIMENSION,
        fnv_offset = hash::FNV_OFFSET,
        fnv_prime = hash::FNV_PRIME,
    )
}
