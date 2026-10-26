// Generated from the wrela-abi crate (runtime/abi): the one definition of the
// command stream and manifest. Don't edit; run `cargo run -p wrela-abi --bin generate`.

export const STREAM_VERSION = 13;
export const MANIFEST_VERSION = 11;
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
  Post: 23,
  DrawIndexedIndirect: 24,
  Label: 25,
  Prepare: 26,
  Unprepare: 27,
  HandOver: 28,
  HostRequest: 29,
  Timed: 30,
  CopyTexture: 31,
  CopyTextureToBuffer: 32,
  CopyBufferToTexture: 33,
  SetScissor: 34,
  SetViewport: 35,
  DrawIndexed: 36,
  MultiDrawIndirect: 37,
  MultiDrawIndexedIndirect: 38,
} as const;

/** A texture's format, by its number in the stream: the format table (runtime/abi
 * `TextureFormat::info`). WebGPU's name; the bytes of a block (a texel, or a compressed format's
 * 4 × 4 texels) and its side; the channels GPU code reads; what it can do on every device
 * (`filterable`) or on some (`storable`, `renderable`, `blendable`); its render target cost and
 * alignment; and the device feature each capability needs, if one. */
export const TEXTURE_FORMATS = [
  { name: "rgba8unorm", bytes: 4, block: 1, channels: 4, depth: false, storable: true, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 1, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rgba16float", bytes: 8, block: 1, channels: 4, depth: false, storable: true, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 2, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "depth32float", bytes: 4, block: 1, channels: 1, depth: true, storable: false, filterable: false, renderable: true, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "r16float", bytes: 2, block: 1, channels: 1, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 2, target_align: 2, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg16float", bytes: 4, block: 1, channels: 2, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 4, target_align: 2, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "r32float", bytes: 4, block: 1, channels: 1, depth: false, storable: true, filterable: false, renderable: true, blendable: true, uint: false, target_cost: 4, target_align: 4, needs: { filter: "float32-filterable", store: null, render: null, blend: "float32-blendable" } },
  { name: "r32uint", bytes: 4, block: 1, channels: 1, depth: false, storable: true, filterable: false, renderable: true, blendable: false, uint: true, target_cost: 4, target_align: 4, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "r8unorm", bytes: 1, block: 1, channels: 1, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 1, target_align: 1, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg8unorm", bytes: 2, block: 1, channels: 2, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 2, target_align: 1, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rgb10a2unorm", bytes: 4, block: 1, channels: 4, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 4, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg32float", bytes: 8, block: 1, channels: 2, depth: false, storable: true, filterable: false, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 4, needs: { filter: "float32-filterable", store: null, render: null, blend: "float32-blendable" } },
  { name: "rgba32float", bytes: 16, block: 1, channels: 4, depth: false, storable: true, filterable: false, renderable: true, blendable: true, uint: false, target_cost: 16, target_align: 4, needs: { filter: "float32-filterable", store: null, render: null, blend: "float32-blendable" } },
  { name: "rg32uint", bytes: 8, block: 1, channels: 2, depth: false, storable: true, filterable: false, renderable: true, blendable: false, uint: true, target_cost: 8, target_align: 4, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg11b10ufloat", bytes: 4, block: 1, channels: 3, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 4, needs: { filter: null, store: null, render: "rg11b10ufloat-renderable", blend: "rg11b10ufloat-renderable" } },
  { name: "bc1-rgba-unorm", bytes: 8, block: 4, channels: 4, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "bc4-r-unorm", bytes: 8, block: 4, channels: 1, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "bc5-rg-unorm", bytes: 16, block: 4, channels: 2, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "bc7-rgba-unorm", bytes: 16, block: 4, channels: 4, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "etc2-rgb8unorm", bytes: 8, block: 4, channels: 3, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-etc2", store: null, render: null, blend: null } },
  { name: "astc-4x4-unorm", bytes: 16, block: 4, channels: 4, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-astc", store: null, render: null, blend: null } },
] as const;
/** A texture's kind, by its number in the stream (runtime/abi `TextureKind`): WebGPU's dimension
 * name. */
export const TEXTURE_KINDS = ["2d", "3d", "2d-array", "cube"] as const;
/** WebGPU's default `maxColorAttachmentBytesPerSample` and `maxColorAttachments`. */
export const MAX_COLOR_ATTACHMENT_BYTES = 32;
export const MAX_COLOR_ATTACHMENTS = 4;
/** The most mip levels a texture has, layers an array has, and anisotropy a sampler takes;
 * WebGPU's alignment of a copy's buffer offset and rows. */
export const MAX_LEVELS = 14;
export const MAX_LAYERS = 256;
export const MAX_ANISOTROPY = 16;
export const ROW_ALIGNMENT = 256;
/** The optional features the hosts request and report, in `wrela.limit(10)`'s bit order
 * (runtime/abi `FEATURES`). */
export const FEATURES = ["shader-f16", "subgroups", "texture-compression-bc", "texture-compression-etc2", "texture-compression-astc", "float32-filterable", "float32-blendable", "rg11b10ufloat-renderable", "bgra8unorm-storage", "timestamp-query", "chromium-experimental-multi-draw-indirect", "indirect-first-instance"] as const;
/** The blends (runtime/abi `Blend`): each one's colour and alpha components, WebGPU's names
 * (`null` for `replace`). */
export const BLENDS = {
  replace: null,
  over: { color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" }, alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" } },
  premultiplied: { color: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" }, alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" } },
  add: { color: { srcFactor: "one", dstFactor: "one", operation: "add" }, alpha: { srcFactor: "one", dstFactor: "one", operation: "add" } },
  multiply: { color: { srcFactor: "dst", dstFactor: "zero", operation: "add" }, alpha: { srcFactor: "dst-alpha", dstFactor: "zero", operation: "add" } },
  min: { color: { srcFactor: "one", dstFactor: "one", operation: "min" }, alpha: { srcFactor: "one", dstFactor: "one", operation: "min" } },
  max: { color: { srcFactor: "one", dstFactor: "one", operation: "max" }, alpha: { srcFactor: "one", dstFactor: "one", operation: "max" } },
} as const;
/** `CreateTexture`'s format word with this bit set: kernels write the texture. */
export const TEXTURE_WRITABLE = 65536;
/** The most texels a side of a 3D texture. */
export const MAX_TEXTURE_3D = 2048;
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
export const IMPORT_PAYLOAD = "payload";
/** The payload bytes one call is given at most, past its first message. */
export const PAYLOAD_PER_CALL = 1048576;
/** The custom section that holds a program's pair id, which a client and its server share. */
export const PAIR_SECTION = "wrela.pair";
export const IMPORT_TICK = "tick";
export const EXPORT_TICK = "__tick";
/** Hot reload: what a host calls to change a lifted literal, and keeps for the next build. */
export const EXPORT_LIFT_SET = "__lift_set";
export const IMPORT_KEEP = "keep";
export const IMPORT_KEPT = "kept";
/** The clock (`std::time`): seconds on the host's clock, an `f64` written at an address. */
export const IMPORT_CLOCK = "clock";
/** A line printed (`std::io::print`) and a phase timed (`std::time::phase`): each a UTF-8 run,
 * shown or noted as it's made. */
export const IMPORT_PRINT = "print";
export const IMPORT_PHASE = "phase";
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
  PadButton: 8,
  PadAxis: 9,
  Compose: 10,
  Paste: 11,
  Assist: 12,
  Message: 13,
  Connection: 14,
  Field: 15,
} as const;
export const MODIFIER_SHIFT = 1;
export const MODIFIER_CONTROL = 2;
export const MODIFIER_ALT = 4;
export const MODIFIER_META = 8;
/** A pointer's buttons, by number, as a script names them. */
export const BUTTONS = ["primary", "middle", "secondary"] as const;
/** The physical keys, by number: DOM's `KeyboardEvent.code` (0: one not listed). */
export const KEYS = ["Unknown", "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ", "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9", "Enter", "Escape", "Backspace", "Tab", "Space", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "CapsLock", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote", "Backquote", "Comma", "Period", "Slash", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "NumpadEnter", "NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide", "NumpadDecimal"] as const;
/** How many gamepads a host reports, and a pad's buttons and axes by number: the W3C Gamepad
 * API's standard mapping, as a script names them. */
export const PADS = 4;
export const PAD_BUTTONS = ["south", "east", "west", "north", "leftshoulder", "rightshoulder", "lefttrigger", "righttrigger", "select", "start", "leftstick", "rightstick", "up", "down", "left", "right", "home"] as const;
export const PAD_AXES = ["leftx", "lefty", "rightx", "righty"] as const;
/** What an `Assist` does to its node, by number (runtime/abi `input`). */
export const ASSIST_ACTIONS = ["activate", "select", "focus", "increment", "decrement", "dismiss"] as const;
/** The most characters a `Compose` change holds. */
export const COMPOSE_MAX = 64;
/** Every function a program may import besides its memory, with its type as both hosts word
 * it. */
export const HOST_FUNCTIONS = [
  ["submit", "(i32, i32) -> ()"],
  ["request_status", "(i32) -> (i32)"],
  ["request_take", "(i32, i32) -> ()"],
  ["limit", "(i32) -> (i32)"],
  ["audio", "(i32, i32) -> ()"],
  ["input", "(i32, i32) -> (i32)"],
  ["payload", "(i32, i32, i32, i32) -> (i32)"],
  ["tick", "(i32, i32, i32) -> ()"],
  ["keep", "(i32, i32) -> ()"],
  ["kept", "(i32, i32) -> (i32)"],
  ["clock", "(i32) -> ()"],
  ["print", "(i32, i32) -> ()"],
  ["phase", "(i32, i32) -> ()"],
] as const;
export const EXPORT_AUDIO = "__audio";
/** The audio thread's rate, render quantum and channels, and where `__audio` leaves a quantum's
 * samples: the left channel's, then the right's. */
export const AUDIO_SAMPLE_RATE = 48000;
export const AUDIO_QUANTUM = 128;
export const AUDIO_CHANNELS = 2;
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
export const TASK_SLOTS = 1212416;
export const TASK_SLOT_SIZE = 16;
export const TASK_SLOTS_END = 1216512;
export const SLOT_STATE = 0;
export const SLOT_THREAD = 12;
export const SLOT_FAILED = 5;
/** How many chunks the helpers have run (one of their shared words). */
export const PAR_HELPED = 264;
/** The helpers' wake count and shutdown flag: a host stops a program's helpers with them. */
export const PAR_WAKE = 256;
export const PAR_SHUTDOWN = 260;
/** How many helpers the host started for the program: it writes it before `init`. */
export const PAR_HELPERS = 272;
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
export const REQUEST_MISSING = -3;
export const EXPORT_FRAME = "frame";
export const EXPORT_INIT = "init";
export const EXPORT_MEMORY = "memory";
export const SCREEN_FORMAT = "rgba8unorm";
/** What a pipeline binds at a binding, as the manifest names it. */
export const BINDING_KINDS = ["read", "read_write", "texture", "depth_texture", "sampler", "comparison_sampler", "storage_texture", "texture_3d", "storage_texture_3d", "texture_2d_array", "texture_cube", "depth_texture_2d_array", "storage_texture_2d_array"] as const;
/** What each binding kind is (runtime/abi `BindingKind`): a colour texture, which names its
 * format; a texture kernels write (a storage texture); a texture of depths; its view dimension;
 * and the per-stage limit it counts against. */
export const BINDING_KIND_TRAITS = [
  { name: "read", has_format: false, is_storage: false, is_depth: false, dimension: null, limit: "storage buffers" },
  { name: "read_write", has_format: false, is_storage: false, is_depth: false, dimension: null, limit: "storage buffers" },
  { name: "texture", has_format: true, is_storage: false, is_depth: false, dimension: "2d", limit: "sampled textures" },
  { name: "depth_texture", has_format: false, is_storage: false, is_depth: true, dimension: "2d", limit: "sampled textures" },
  { name: "sampler", has_format: false, is_storage: false, is_depth: false, dimension: null, limit: "samplers" },
  { name: "comparison_sampler", has_format: false, is_storage: false, is_depth: false, dimension: null, limit: "samplers" },
  { name: "storage_texture", has_format: true, is_storage: true, is_depth: false, dimension: "2d", limit: "storage textures" },
  { name: "texture_3d", has_format: true, is_storage: false, is_depth: false, dimension: "3d", limit: "sampled textures" },
  { name: "storage_texture_3d", has_format: true, is_storage: true, is_depth: false, dimension: "3d", limit: "storage textures" },
  { name: "texture_2d_array", has_format: true, is_storage: false, is_depth: false, dimension: "2d-array", limit: "sampled textures" },
  { name: "texture_cube", has_format: true, is_storage: false, is_depth: false, dimension: "cube", limit: "sampled textures" },
  { name: "depth_texture_2d_array", has_format: false, is_storage: false, is_depth: true, dimension: "2d-array", limit: "sampled textures" },
  { name: "storage_texture_2d_array", has_format: true, is_storage: true, is_depth: false, dimension: "2d-array", limit: "storage textures" },
] as const;
/** WebGPU's default per-stage limits (runtime/abi `StageLimit`): what each counts, and the most a
 * shader stage may have. */
export const STAGE_LIMITS = [
  { name: "storage buffers", max: 8 },
  { name: "sampled textures", max: 16 },
  { name: "samplers", max: 16 },
  { name: "storage textures", max: 4 },
] as const;
/** `minStorageBufferOffsetAlignment`: where a bound range of a buffer may start. */
export const BINDING_OFFSET_ALIGNMENT = 256;
export const MAX_WORKGROUP_SIZE = [256, 256, 64];
export const MAX_WORKGROUP_INVOCATIONS = 256;
export const MAX_STORAGE_BUFFERS_PER_STAGE = 8;
export const MAX_UNIFORM_BUFFER_BINDING_SIZE = 65536;
export const MAX_BINDINGS_PER_GROUP = 1000;
/** WebGPU's default limits, in `wrela.limit`'s order (runtime/abi `Limits`). */
export const DEFAULT_LIMITS = [8192, 134217728, 8, 65536, 16384, 256, 256, 256, 64, 65535, 2048, 0, 0];

export const FNV_OFFSET = 0xcbf29ce484222325n;
export const FNV_PRIME = 0x00000100000001b3n;

/** Where in a thread's block a panic leaves its message: a u32 byte count, then UTF-8 (at most
 * PANIC_CAP bytes). */
export const PANIC = 4096;
export const PANIC_CAP = 4092;
