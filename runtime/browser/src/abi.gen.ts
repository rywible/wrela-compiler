// Generated from the wrela-abi crate (runtime/abi): the one definition of the
// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin gen-ts`.

export const STREAM_VERSION = 2;
export const MANIFEST_VERSION = 1;
/** The bytes `WRCS`, read as a little-endian u32. */
export const STREAM_MAGIC = 0x53435257;
export const HEADER_LEN = 12;
export const COMMAND_HEADER_LEN = 8;

export const Opcode = {
  CreateBuffer: 1,
  WriteBuffer: 2,
  Dispatch: 3,
  BeginScreenPass: 4,
  Draw: 5,
  Present: 6,
  DestroyBuffer: 7,
} as const;

export const IMPORT_MODULE = "wrela";
export const IMPORT_SUBMIT = "submit";
export const EXPORT_FRAME = "frame";
export const EXPORT_MEMORY = "memory";
export const SCREEN_FORMAT = "rgba8unorm";
export const MAX_WORKGROUP_SIZE = [256, 256, 64];
export const MAX_WORKGROUP_INVOCATIONS = 256;
export const MAX_STORAGE_BUFFERS_PER_STAGE = 8;

export const FNV_OFFSET = 0xcbf29ce484222325n;
export const FNV_PRIME = 0x00000100000001b3n;
