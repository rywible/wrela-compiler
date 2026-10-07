// Generated from the wrela-abi crate (runtime/abi): the one definition of the
// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin generate`.

export const STREAM_VERSION = 7;
export const MANIFEST_VERSION = 5;
/** The bytes `WRCS`, read as a little-endian u32. */
export const STREAM_MAGIC = 0x53435257;
export const HEADER_LEN = 12;
export const COMMAND_HEADER_LEN = 8;
/** A pass's attachment that isn't there, and a pass's colour attachment that is the screen. */
export const NONE = 0xffffffff;
export const SCREEN = 0xfffffffe;
/** The upload ring's starting size, and the largest write it takes: a bigger one goes through
 * the queue, between submissions. */
export const UPLOAD_START = 262144;
export const UPLOAD_MAX = 16777216;

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
  Post: 23,
  DrawIndexedIndirect: 24,
  Label: 25,
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
export const COMPARES = [null, "less", "less-equal", "greater", "greater-equal", "equal", "not-equal", "always", "never"] as const;

export const IMPORT_MODULE = "wrela";
export const IMPORT_SUBMIT = "submit";
export const IMPORT_REQUEST_STATUS = "request_status";
export const IMPORT_REQUEST_TAKE = "request_take";
export const IMPORT_LIMIT = "limit";
export const IMPORT_MEMORY = "memory";
export const EXPORT_WORKER = "__worker";
export const IMPORT_AUDIO = "audio";
export const IMPORT_INPUT = "input";
export const IMPORT_TICK = "tick";
export const EXPORT_TICK = "__tick";
/** Input (runtime/abi `input`): an event is EVENT_SIZE bytes, six words: its kind, its
 * modifiers, then four words that depend on the kind. */
export const EVENT_SIZE = 24;
export const EventKind = {
  PointerMove: 1,
  PointerDown: 2,
  PointerUp: 3,
  Wheel: 4,
  KeyDown: 5,
  KeyUp: 6,
  Text: 7,
} as const;
export const MODIFIER_SHIFT = 1;
export const MODIFIER_CONTROL = 2;
export const MODIFIER_ALT = 4;
export const MODIFIER_META = 8;
/** A pointer's buttons, by number, as a script names them. */
export const BUTTONS = ["primary", "middle", "secondary"] as const;
/** The physical keys, by number: DOM's `KeyboardEvent.code` (0: one not listed). */
export const KEYS = ["Unknown", "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ", "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9", "Enter", "Escape", "Backspace", "Tab", "Space", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "CapsLock", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote", "Backquote", "Comma", "Period", "Slash", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "NumpadEnter", "NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide", "NumpadDecimal"] as const;
/** Every function a program may import besides its memory, with its type as both hosts word
 * it. */
export const HOST_FUNCTIONS = [
  ["submit", "(i32, i32) -> ()"],
  ["request_status", "(i32) -> (i32)"],
  ["request_take", "(i32, i32) -> ()"],
  ["limit", "(i32) -> (i32)"],
  ["audio", "(i32, i32) -> ()"],
  ["input", "(i32, i32) -> (i32)"],
  ["tick", "(i32, i32, i32) -> ()"],
] as const;
export const EXPORT_AUDIO = "__audio";
/** The audio thread's rate and render quantum, and where `__audio` leaves a quantum's samples. */
export const AUDIO_SAMPLE_RATE = 48000;
export const AUDIO_QUANTUM = 128;
export const AUDIO_OUT = 1245184;
/** The threads (runtime/abi `memory`): their numbers, their blocks, and the words of a block and
 * of a job slot a host reads and writes. */
export const THREAD_MAIN = 0;
export const THREAD_TICK = 1;
export const THREAD_AUDIO = 2;
export const THREAD_HELPER0 = 3;
export const MAX_WORKERS = 8;
export const THREAD_BLOCKS = 1114112;
export const THREAD_BLOCK_SIZE = 8192;
export const THREAD_BLOCKS_END = 1204224;
export const JOB_DONE = 20;
export const JOB_FAILED = 24;
export const JOB_DONE_FAILED = 2147483648;
export const RUNNING = 32;
export const JOB_SLOTS = 1212416;
export const JOB_SLOTS_END = 1216512;
export const SLOT_STATE = 0;
export const SLOT_THREAD = 12;
export const SLOT_FAILED = 5;
/** How many chunks the helpers have run (one of their shared words). */
export const PAR_HELPED = 264;
/** The ticker's words (runtime/abi `memory`), and its records' region. */
export const TICK_WANT_HASH = 512;
export const TICK_HASH = 520;
export const TICK_ORIGIN = 528;
export const TICK_RECORDS = 1228800;
export const MAX_TICK_RECORDS = 256;
/** The tick log (runtime/abi `ticks`): its magic, the bytes `WRTL` read as a little-endian
 * u32. */
export const TICK_LOG_MAGIC = 0x4c545257;
export const TICK_LOG_VERSION = 1;
export const TICK_LOG_HEADER_LEN = 32;
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

/** Where in a thread's block a panic leaves its message: a u32 byte count, then UTF-8 (at most
 * PANIC_CAP bytes). */
export const PANIC = 4096;
export const PANIC_CAP = 4092;
