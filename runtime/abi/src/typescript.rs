//! The browser runtime's copy of the ABI's constants, generated so they're defined once.

use crate::{hash, manifest, stream};
use std::fmt::Write;

/// The checked-in TypeScript module, relative to the repository root.
pub const TS_PATH: &str = "runtime/browser/src/abi.gen.ts";

/// The TypeScript module `runtime/browser/src/abi.gen.ts`.
pub fn typescript() -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "// Generated from the wrela-abi crate (runtime/abi): the one definition of the"
    );
    let _ = writeln!(
        s,
        "// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin gen-ts`."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "export const STREAM_VERSION = {};", stream::VERSION);
    let _ = writeln!(s, "export const MANIFEST_VERSION = {};", manifest::VERSION);
    let magic = u32::from_le_bytes(stream::MAGIC);
    let _ = writeln!(s, "/** The bytes `WRCS`, read as a little-endian u32. */");
    let _ = writeln!(s, "export const STREAM_MAGIC = 0x{magic:08x};");
    let _ = writeln!(s, "export const HEADER_LEN = {};", stream::HEADER_LEN);
    let _ = writeln!(s, "export const COMMAND_HEADER_LEN = {};", stream::COMMAND_HEADER_LEN);
    let _ = writeln!(s);
    let _ = writeln!(s, "export const Opcode = {{");
    for op in stream::Opcode::ALL {
        let _ = writeln!(s, "  {}: {},", op.name(), op as u32);
    }
    let _ = writeln!(s, "}} as const;");
    let _ = writeln!(s);
    let _ = writeln!(s, "export const IMPORT_MODULE = {:?};", crate::IMPORT_MODULE);
    let _ = writeln!(s, "export const IMPORT_SUBMIT = {:?};", crate::IMPORT_SUBMIT);
    let _ = writeln!(s, "export const EXPORT_FRAME = {:?};", crate::EXPORT_FRAME);
    let _ = writeln!(s, "export const EXPORT_MEMORY = {:?};", crate::EXPORT_MEMORY);
    let _ = writeln!(s, "export const SCREEN_FORMAT = {:?};", manifest::SCREEN_FORMAT);
    let _ = writeln!(
        s,
        "export const MAX_WORKGROUP_SIZE = [{}, {}, {}];",
        manifest::MAX_WORKGROUP_SIZE[0],
        manifest::MAX_WORKGROUP_SIZE[1],
        manifest::MAX_WORKGROUP_SIZE[2]
    );
    let _ = writeln!(
        s,
        "export const MAX_WORKGROUP_INVOCATIONS = {};",
        manifest::MAX_WORKGROUP_INVOCATIONS
    );
    let _ = writeln!(
        s,
        "export const MAX_STORAGE_BUFFERS_PER_STAGE = {};",
        manifest::MAX_STORAGE_BUFFERS_PER_STAGE
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "export const FNV_OFFSET = 0x{:016x}n;", hash::FNV_OFFSET);
    let _ = writeln!(s, "export const FNV_PRIME = 0x{:016x}n;", hash::FNV_PRIME);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The browser runtime's constants match this crate's.
    #[test]
    fn checked_in_copy_is_current() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join(TS_PATH);
        let actual = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            actual == typescript(),
            "{} is stale; run `cargo run -p wrela-abi --bin gen-ts`",
            path.display()
        );
    }
}
