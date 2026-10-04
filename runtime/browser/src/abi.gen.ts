// Generated from the wrela-abi crate (runtime/abi): the one definition of the
// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin gen-ts`.

export const STREAM_VERSION = 4;
export const MANIFEST_VERSION = 2;
/** The bytes `WRCS`, read as a little-endian u32. */
export const STREAM_MAGIC = 0x53435257;
export const HEADER_LEN = 12;
export const COMMAND_HEADER_LEN = 8;
/** A pass's attachment that isn't there, and a pass's colour attachment that is the screen. */
export const NONE = 0xffffffff;
export const SCREEN = 0xfffffffe;

export const Opcode = {
  CreateBuffer: 1,
  WriteBuffer: 2,
  Dispatch: 3,
  BeginScreenPass: 4,
  Draw: 5,
  Present: 6,
  DestroyBuffer: 7,
  CopyBuffer: 8,
  CreateTexture: 9,
  WriteTexture: 10,
  DestroyTexture: 11,
  CreateSampler: 12,
  DestroySampler: 13,
  BeginPass: 14,
  EndPass: 15,
  DispatchIndirect: 16,
  DrawIndirect: 17,
  ReadBuffer: 18,
  StorageRead: 19,
  StorageWrite: 20,
  Fetch: 21,
  Log: 22,
} as const;

/** A texture's format, by its number in the stream: WebGPU's name, the bytes of one texel, and
 * whether it's a depth format. */
export const TEXTURE_FORMATS = [
  { name: "rgba8unorm", bytes: 4, depth: false },
  { name: "rgba16float", bytes: 8, depth: false },
  { name: "depth32float", bytes: 4, depth: true },
] as const;
/** A comparison sampler's test, by its number in the stream: WebGPU's name (0 is a sampler that
 * doesn't compare). */
export const COMPARES = [null, "less", "less-equal", "greater", "greater-equal"] as const;

export const IMPORT_MODULE = "wrela";
export const IMPORT_SUBMIT = "submit";
export const IMPORT_REQUEST_STATUS = "request_status";
export const IMPORT_REQUEST_TAKE = "request_take";
export const IMPORT_LIMIT = "limit";
export const IMPORT_MEMORY = "memory";
export const EXPORT_WORKER = "__worker";
export const IMPORT_AUDIO = "audio";
/** Every function a program may import besides its memory, with its type as both hosts word
 * it. */
export const HOST_FUNCTIONS = [
  ["submit", "(i32, i32) -> ()"],
  ["request_status", "(i32) -> (i32)"],
  ["request_take", "(i32, i32) -> ()"],
  ["limit", "(i32) -> (i32)"],
  ["audio", "(i32, i32) -> ()"],
] as const;
export const EXPORT_AUDIO = "__audio";
/** The audio thread's rate and render quantum, and where `__audio` leaves a quantum's samples. */
export const AUDIO_SAMPLE_RATE = 48000;
export const AUDIO_QUANTUM = 128;
export const AUDIO_OUT = 18874368;
/** The workers' job's words a host writes, and how many workers a program runs at most
 * (runtime/abi `memory`). */
export const WORKERS_GENERATION = 256;
export const WORKERS_DONE = 276;
export const WORKERS_FAILED = 280;
export const WORKERS_DONE_FAILED = 2147483648;
export const WORKERS_SHUTDOWN = 284;
export const WORKERS_HELPED = 288;
export const MAX_WORKERS = 8;
export const REQUEST_PENDING = -1;
export const REQUEST_FAILED = -2;
export const EXPORT_FRAME = "frame";
export const EXPORT_INIT = "init";
export const EXPORT_MEMORY = "memory";
export const SCREEN_FORMAT = "rgba8unorm";
/** What a pipeline binds at a binding, as the manifest names it. */
export const BINDING_KINDS = ["read", "read_write", "texture", "depth_texture", "sampler", "comparison_sampler"] as const;
/** `minStorageBufferOffsetAlignment`: where a bound range of a buffer may start. */
export const BINDING_OFFSET_ALIGNMENT = 256;
export const MAX_WORKGROUP_SIZE = [256, 256, 64];
export const MAX_WORKGROUP_INVOCATIONS = 256;
export const MAX_STORAGE_BUFFERS_PER_STAGE = 8;
export const MAX_UNIFORM_BUFFER_BINDING_SIZE = 65536;
/** WebGPU's default limits, in `wrela.limit`'s order (runtime/abi `Limits`). */
export const DEFAULT_LIMITS = [8192, 134217728, 8, 65536, 16384, 256, 256, 256, 64, 65535];

export const FNV_OFFSET = 0xcbf29ce484222325n;
export const FNV_PRIME = 0x00000100000001b3n;

/** Where a panic leaves its message: a u32 byte count, then UTF-8 (at most PANIC_CAP bytes). */
export const PANIC_MESSAGE = 512;
export const PANIC_CAP = 3580;
