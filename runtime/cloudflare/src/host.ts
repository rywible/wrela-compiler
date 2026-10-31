// A server build over HTTP (language.md §19.5): one instance of `server/game.wasm`, the store
// beside it, and the requests it answers. It is the Durable Object's whole job (cloudflare.ts),
// written against small interfaces (`Storage`, `Options`) so the tests run it on bun:sqlite.
//
// It mirrors `wrela_host::server` and `wrela_host::rooms` where a room's contract carries over:
// - **A request is a short connection** (runtime/abi `http`): a `Connection` event opens it, one
//   `Message` carries the request's bytes, the program answers with `http/respond`, and a later
//   call hears that it closed.
// - **A request the program makes is answered after the call returns**, for the next call: the
//   store's from what is committed when it's taken, a signature's once WebCrypto has checked it.
// - **A call's writes commit in one transaction when it returns** (`transactionSync`), then its
//   responses go. A call that fails commits nothing and sends nothing.
// - **A call is made again at once while its requests were answered**, or new events wait,
//   giving the event loop a turn between calls. With no client waiting, the object's alarm goes on.
//
// What it can't mirror: a call has no deadline (JavaScript can't interrupt WASM it runs; the
// Worker's CPU limit is the bound), and the memory cap is checked after each call, not refused
// inside it. A failed call drops the instance, and every request open on it gets 500 (the
// program's) or 503 (the host's limits): their state lived in the instance.
//
// Nothing here logs a request, a path or a header; a failure's reason, and a program's printed
// lines when asked for, are all that reach the log.

import {
  EVENT_SIZE,
  EventKind,
  EXPORT_MEMORY,
  IMPORT_CLOCK,
  IMPORT_INPUT,
  IMPORT_LIMIT,
  IMPORT_MODULE,
  IMPORT_PAYLOAD,
  IMPORT_PHASE,
  IMPORT_PRINT,
  IMPORT_REQUEST_STATUS,
  IMPORT_REQUEST_TAKE,
  IMPORT_SUBMIT,
  REQUEST_FAILED,
  REQUEST_PENDING,
  RESPOND_WITHIN_MS,
  SERVICE_RESPOND,
  SERVICE_TIME,
  THREAD_MAIN,
} from "../../browser/src/abi.gen.ts";
import { decodeResponse, encodeResponse, type Header, type HttpResponse, In, Out, setByServer } from "../../browser/src/http.ts";
import { connectionEvent } from "../../browser/src/input.ts";
import { Inbox } from "../../browser/src/net.ts";
import { takePanicMessage } from "../../browser/src/imports.ts";
import { decode } from "../../browser/src/stream.ts";

/** A server build's exports. */
const EXPORT_SERVE = "serve";
const EXPORT_SERVE_INIT = "serve_init";

/** The store's limits (`wrela_host::services`). */
export const MAX_KEY = 1024;
export const MAX_VALUE = 1020 << 10;
export const MAX_SCAN = 1 << 20;
export const MAX_RANDOM = 1024;

/** What one instance may use, as `wrela_host::server::Limits`' defaults. */
export interface Limits {
  /** Its memory, in bytes: checked after each call. */
  memory: number;
  /** The bytes one call's commands may hold. */
  commands: number;
  /** How long one turn of calls may run before it yields to the alarm, in milliseconds. */
  turn: number;
  /** The requests open at once, and the bytes of the requests that wait for the program. */
  open: number;
  queued: number;
}

export const LIMITS: Limits = { memory: 64 << 20, commands: 16 << 20, turn: 250, open: 256, queued: 32 << 20 };

/** The rows of a query, as `ctx.storage.sql.exec` gives them, one at a time. */
export interface Cursor extends Iterable<Record<string, unknown>> {
  toArray(): Record<string, unknown>[];
}

/** A Durable Object's storage, as much of it as the host uses (the SQLite storage API). */
export interface Storage {
  sql: { exec(query: string, ...bindings: unknown[]): Cursor };
  transactionSync<T>(fn: () => T): T;
}

export interface Options {
  limits?: Limits;
  /** The wall clock, in Unix milliseconds. */
  now?: () => number;
  /** Asks to be called (`alarm`) at that time: work is left and no request waits. */
  setAlarm?: (at: number) => void;
  /** Where the program's printed lines go; none: nowhere. */
  print?: (line: string) => void;
  /** Where a failure's reason goes. */
  log?: (line: string) => void;
}

/** Why a call failed: the program's own trap, or the host's limits or refusals. */
export class Failure extends Error {
  constructor(
    readonly program: boolean,
    why: string,
  ) {
    super(why);
  }

  /** `e` as a failure: itself if it is one, else the host's (or, with `program`, the program's). */
  static of(e: unknown, program = false): Failure {
    return e instanceof Failure ? e : new Failure(program, e instanceof Error ? e.message : String(e));
  }
}

// ---- HTTP's bytes (runtime/abi `http`) ----------------------------------------------------------

export { decodeRequest, decodeResponse, encodeRequest, encodeResponse, type Header, type HttpRequest, type HttpResponse } from "../../browser/src/http.ts";

/** The statuses whose responses have no body. */
export const NO_BODY: readonly number[] = [204, 205, 304];

/** A header a program wrote is a token in lower case, and its value has no line break or NUL. */
const HEADER_NAME = /^[a-z0-9!#$%&'*+.^_`|~-]+$/;
const HEADER_VALUE = /^[^\r\n\0]*$/;

/** A program's response, checked: why not, if it isn't one a server may send. */
export function checkResponse(bytes: Uint8Array): HttpResponse {
  const s = decodeResponse(bytes);
  if (NO_BODY.includes(s.status) && s.body.length > 0) throw new Error(`a ${s.status} has no body`);
  for (const [n, v] of s.headers) {
    if (!HEADER_NAME.test(n)) throw new Error(`\`${n}\` isn't a header's name in lower case`);
    if (!setByServer(n)) throw new Error(`a server can't set header \`${n}\``);
    if (!HEADER_VALUE.test(v)) throw new Error(`header \`${n}\`'s value has a line break`);
  }
  return s;
}

/** The headers the host gives every response it sends, the program's and its own. */
export const HOST_HEADERS: readonly Header[] = [
  ["x-content-type-options", "nosniff"],
  ["referrer-policy", "same-origin"],
  ["content-security-policy", "default-src 'none'"],
];

/** What the host adds to every response: nothing of the API is a page, and nothing is cached
 * unless the program says so. */
export function withHostHeaders(s: HttpResponse): HttpResponse {
  const headers = [...s.headers];
  if (!headers.some(([n]) => n === "cache-control")) headers.push(["cache-control", "no-store"]);
  headers.push(...HOST_HEADERS);
  return { ...s, headers };
}

/** A response with no body: the host's own, for a request the program couldn't answer. */
export const bare = (status: number): Uint8Array => encodeResponse(withHostHeaders({ status, headers: [], body: new Uint8Array(0) }));

// ---- the store (wrela_host::services) ---------------------------------------------------------

const asBytes = (v: unknown): Uint8Array => (v instanceof Uint8Array ? v : new Uint8Array(v as ArrayBuffer));
/** Bytes bound as a BLOB: an ArrayBuffer of exactly them, as the SQLite storage API takes one. */
export const blob = (b: Uint8Array): ArrayBuffer =>
  b.byteOffset === 0 && b.byteLength === b.buffer.byteLength && b.buffer instanceof ArrayBuffer ? b.buffer : (b.slice().buffer as ArrayBuffer);

/** The server's store: one table of keys and values, both bytes, as `store.sqlite` on
 * `wrela-serve`. */
export class Store {
  constructor(readonly storage: Storage) {
    storage.sql.exec("CREATE TABLE IF NOT EXISTS kv (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID");
  }

  /** A read of the store (`store/get`, `store/scan`), answered from what's committed: null if it
   * failed, or isn't one. */
  answer(service: string, body: Uint8Array): Uint8Array | null {
    if (service === "store/get") return answerOf(() => this.get(body));
    if (service === "store/scan") return answerOf(() => this.scan(body));
    return null;
  }

  /** `store/get`: the key → 1 then the value, or 0. */
  get(body: Uint8Array): Uint8Array {
    if (body.length > MAX_KEY) throw new Error(`a key is longer than ${MAX_KEY} bytes`);
    const rows = this.storage.sql.exec("SELECT value FROM kv WHERE key = ?", blob(body)).toArray();
    if (rows.length === 0) return new Uint8Array([0]);
    return new Out().push(new Uint8Array([1])).push(asBytes(rows[0]!["value"])).bytes();
  }

  /** `store/scan`: entries with `from <= key < to` (an empty `to`: no end), at most `limit`, up
   * or down, after their count; it stops early past `MAX_SCAN` bytes. */
  scan(body: Uint8Array): Uint8Array {
    const r = new In(body);
    const from = r.chunk();
    const to = r.chunk();
    const limit = r.word();
    const down = r.byte() === 1;
    r.end("service");
    if (from.length > MAX_KEY || to.length > MAX_KEY) throw new Error(`a key is longer than ${MAX_KEY} bytes`);
    const order = down ? "DESC" : "ASC";
    const rows =
      to.length === 0
        ? this.storage.sql.exec(`SELECT key, value FROM kv WHERE key >= ? ORDER BY key ${order} LIMIT ?`, blob(from), limit)
        : this.storage.sql.exec(`SELECT key, value FROM kv WHERE key >= ? AND key < ? ORDER BY key ${order} LIMIT ?`, blob(from), blob(to), limit);
    const out = new Out();
    let count = 0;
    let bytes = 0;
    // Read row by row: a scan stops at its bytes, whatever its limit.
    for (const row of rows) {
      const k = asBytes(row["key"]);
      const v = asBytes(row["value"]);
      bytes += k.length + v.length;
      if (count > 0 && bytes > MAX_SCAN) break;
      out.chunk(k).chunk(v);
      count++;
    }
    return new Out().word(count).push(out.bytes()).bytes();
  }

  /** Commits writes (`store/write` bodies) in one transaction, all or none. A malformed body is
   * the program's, and commits nothing. */
  commit(batches: Uint8Array[]): void {
    const ops = writes(batches);
    if (ops.length === 0) return;
    this.storage.transactionSync(() => {
      for (const [k, v] of ops) {
        if (v === null) this.storage.sql.exec("DELETE FROM kv WHERE key = ?", blob(k));
        else this.storage.sql.exec("INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)", blob(k), blob(v));
      }
    });
  }
}

/** The writes `store/write` bodies make, each a key and its new value (null: deleted), in order;
 * a malformed body, or a key or a value too long, is the program's failure. */
export function writes(batches: Uint8Array[]): [Uint8Array, Uint8Array | null][] {
  const ops: [Uint8Array, Uint8Array | null][] = [];
  try {
    for (const b of batches) {
      const r = new In(b);
      while (r.left() > 0) {
        const kind = r.byte();
        const k = r.chunk();
        if (k.length > MAX_KEY) throw new Error(`a key is longer than ${MAX_KEY} bytes`);
        if (kind === 1) {
          const v = r.chunk();
          if (v.length > MAX_VALUE) throw new Error(`a value is longer than ${MAX_VALUE} bytes`);
          ops.push([k, v]);
        } else if (kind === 2) {
          ops.push([k, null]);
        } else {
          throw new Error(`a store write of kind ${kind}`);
        }
      }
    }
  } catch (e) {
    throw Failure.of(e, true);
  }
  return ops;
}

// ---- cryptography (WebCrypto) -----------------------------------------------------------------

/** `crypto/random`: how many bytes → that many from the runtime's generator. */
export function random(body: Uint8Array): Uint8Array {
  const r = new In(body);
  const n = r.word();
  r.end("service");
  if (n > MAX_RANDOM) throw new Error(`more than ${MAX_RANDOM} random bytes at once`);
  return crypto.getRandomValues(new Uint8Array(n));
}

/** A DER ECDSA signature (SEQUENCE of two INTEGERs) as the 64 bytes `r‖s` WebCrypto takes, or
 * null if it isn't one: strict DER, as p256's `DerSignature` reads it. */
export function derToRaw(der: Uint8Array): Uint8Array<ArrayBuffer> | null {
  if (der.length < 8 || der[0] !== 0x30 || der[1] !== der.length - 2) return null;
  const out = new Uint8Array(64);
  let at = 2;
  for (const half of [0, 32]) {
    if (der[at] !== 0x02) return null;
    const n = der[at + 1]!;
    at += 2;
    if (n === 0 || n > 33 || at + n > der.length) return null;
    let int = der.subarray(at, at + n);
    at += n;
    // Positive and minimal: a leading 0 only before a byte whose top bit is set.
    if (int[0]! & 0x80) return null;
    if (int[0] === 0) {
      if (n === 1 || !(int[1]! & 0x80)) return null;
      int = int.subarray(1);
    }
    if (int.length > 32) return null;
    out.set(int, half + 32 - int.length);
  }
  return at === der.length ? out : null;
}

const base64url = (b: Uint8Array): string => {
  let s = "";
  for (const x of b) s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

/** Big-endian bytes without their leading zeros (a JWK's `n` and `e`, RFC 7518 §6.3.1). */
const unsigned = (b: Uint8Array): Uint8Array => {
  let i = 0;
  while (i < b.length - 1 && b[i] === 0) i++;
  return b.subarray(i);
};

/** `crypto/verify`: an algorithm (1 ES256, 2 Ed25519, 3 RS256), a key, a message and a signature
 * → 1 if the signature is the key's over the message, else 0. A key the algorithm can't import
 * fails the request; a malformed signature is simply not the key's. */
export async function verify(body: Uint8Array): Promise<Uint8Array> {
  const r = new In(body);
  const alg = r.byte();
  const key = r.chunk().slice() as Uint8Array<ArrayBuffer>;
  const message = r.chunk().slice() as Uint8Array<ArrayBuffer>;
  const signature = r.chunk().slice() as Uint8Array<ArrayBuffer>;
  r.end("service");
  const answer = (ok: boolean) => new Uint8Array([ok ? 1 : 0]);
  const imported = async (format: "raw" | "jwk", data: Uint8Array | JsonWebKey, algorithm: AlgorithmIdentifier | EcKeyImportParams | RsaHashedImportParams) => {
    try {
      return await (format === "jwk"
        ? crypto.subtle.importKey("jwk", data as JsonWebKey, algorithm, false, ["verify"])
        : crypto.subtle.importKey("raw", data as Uint8Array<ArrayBuffer>, algorithm, false, ["verify"]));
    } catch {
      throw new Error("the key isn't one of the algorithm's");
    }
  };
  switch (alg) {
    case 1: {
      const k = await imported("raw", key, { name: "ECDSA", namedCurve: "P-256" });
      const raw = derToRaw(signature);
      if (raw === null) return answer(false);
      return answer(await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, k, raw, message));
    }
    case 2: {
      if (key.length !== 32) throw new Error("an Ed25519 key is 32 bytes");
      const k = await imported("raw", key, { name: "Ed25519" });
      if (signature.length !== 64) return answer(false);
      return answer(await crypto.subtle.verify({ name: "Ed25519" }, k, signature, message));
    }
    case 3: {
      const kr = new In(key);
      const n = unsigned(kr.chunk());
      const e = unsigned(kr.chunk());
      kr.end("service");
      const jwk: JsonWebKey = { kty: "RSA", n: base64url(n), e: base64url(e), alg: "RS256", ext: true };
      const k = await imported("jwk", jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" });
      return answer(await crypto.subtle.verify({ name: "RSASSA-PKCS1-v1_5" }, k, signature, message));
    }
    default:
      throw new Error(`no signature algorithm ${alg}`);
  }
}

// ---- an instance ------------------------------------------------------------------------------

/** What `f` answers, or null (a failed request) if it throws. */
function answerOf(f: () => Uint8Array): Uint8Array | null {
  try {
    return f();
  } catch {
    return null;
  }
}

/** A printed line's text, whatever its bytes. */
const LINES = new TextDecoder();

/** A call's command the host carries out after it returns. */
type Command = { kind: "write"; body: Uint8Array } | { kind: "respond"; conn: number; response: HttpResponse };

/** What runs an instance: the HTTP server here, a room (room.ts). It holds the events the program
 * hasn't read, and takes the requests the program makes (`submit`): a command `C` for after the
 * call, or a request answered for the next. */
export interface Owner<C> {
  readonly queue: Inbox;
  readonly limits: Limits;
  /** Where a printed line goes; none, nowhere. */
  readonly print: ((line: string) => void) | undefined;
  /** Seconds on the host's clock, never going back. */
  clock(): number;
  submit(self: Instance<C>, id: number, service: string, body: Uint8Array): void;
}

/** One instance of the server build, and the host's part of it: its owner's commands are `C`. */
export class Instance<C> {
  readonly memory: WebAssembly.Memory;
  readonly serve: (time: number) => void;
  /** Answers the program may read, and those made during this call (read from the next). */
  answered = new Map<number, Uint8Array | null>();
  arriving: Promise<[number, Uint8Array | null]>[] = [];
  commands: C[] = [];
  commandBytes = 0;
  /** Why the host refused what the program asked, in this call. */
  refused: string | null = null;

  private constructor(
    readonly owner: Owner<C>,
    instance: WebAssembly.Instance,
  ) {
    const exports = instance.exports;
    const memory = exports[EXPORT_MEMORY];
    if (!(memory instanceof WebAssembly.Memory)) throw new Failure(false, "this isn't a server build: its memory isn't its own");
    this.memory = memory;
    const serve = exports[EXPORT_SERVE];
    if (typeof serve !== "function") throw new Failure(false, "this isn't a server build: it doesn't export `serve`");
    this.serve = serve as (time: number) => void;
  }

  static async start<C>(owner: Owner<C>, module: WebAssembly.Module): Promise<Instance<C>> {
    let self: Instance<C> | null = null;
    const me = () => {
      if (self === null) throw new Error("the instance isn't started");
      return self;
    };
    const imports = { [IMPORT_MODULE]: imports_(owner, me) };
    let instance: WebAssembly.Instance;
    try {
      instance = await WebAssembly.instantiate(module, imports);
    } catch (e) {
      throw new Failure(false, `instantiating the server build: ${(e as Error).message}`);
    }
    self = new Instance(owner, instance);
    const init = instance.exports[EXPORT_SERVE_INIT];
    if (init !== undefined) {
      if (typeof init !== "function" || (init as Function).length !== 0) throw new Failure(false, "`serve_init` isn't `serve_init()`");
      self.call(() => (init as () => void)());
    }
    return self;
  }

  /** Runs `f` as a call: what it asked is answered after it, and its failure classified. */
  call(f: () => void): void {
    this.refused = null;
    this.commands = [];
    this.commandBytes = 0;
    try {
      f();
    } catch (e) {
      throw this.classify(e);
    } finally {
      this.owner.queue.endCall();
    }
    if (this.memory.buffer.byteLength > this.owner.limits.memory) {
      throw new Failure(false, "the program asked for more memory than its instance has");
    }
  }

  private classify(e: unknown): Failure {
    if (e instanceof Failure) return e;
    if (this.refused !== null) return new Failure(true, this.refused);
    if (e instanceof RangeError && /call stack/i.test(e.message)) return new Failure(false, "the call went deeper than the stack allows");
    if (e instanceof WebAssembly.RuntimeError) {
      const panic = takePanicMessage(this.memory, THREAD_MAIN);
      return new Failure(true, panic ?? e.message);
    }
    return new Failure(false, (e as Error)?.message ?? String(e));
  }

  /** The host refuses what the program asked: `why` fails the call, as the program's (it
   * misused the host), or with `limit`, as the host's limits. */
  refuse(why: string, limit = false): never {
    this.refused = why;
    throw new Failure(!limit, why);
  }

  read(at: number, len: number): Uint8Array<ArrayBuffer> {
    if (at + len > this.memory.buffer.byteLength) this.refuse(`(${at}, ${len}) reaches past the end of the program's memory`);
    return new Uint8Array(this.memory.buffer, at, len).slice();
  }

  write(at: number, bytes: Uint8Array, what: string): void {
    if (at + bytes.length > this.memory.buffer.byteLength) this.refuse(`${what} reaches past the end of the program's memory`);
    new Uint8Array(this.memory.buffer, at, bytes.length).set(bytes);
  }

  /** A command for after the call, of `bytes` toward the call's limit. */
  command(c: C, bytes: number): void {
    this.commandBytes += bytes;
    if (this.commandBytes > this.owner.limits.commands) this.refuse(`a call's commands past ${this.owner.limits.commands} bytes`, true);
    this.commands.push(c);
  }

  /** Request `id`'s answer (null: it failed), which the next call reads. */
  answer(id: number, a: Uint8Array | null | Promise<Uint8Array | null>): void {
    this.arriving.push(Promise.resolve(a).then((x) => [id, x] as [number, Uint8Array | null]));
  }

  /** The answers its last call's requests got, readable from its next call. */
  async settle(): Promise<void> {
    for (const [id, a] of await Promise.all(this.arriving.splice(0))) this.answered.set(id, a);
  }
}

/** The answer to a request a host gives the same way wherever it runs a server build: random
 * bytes, a signature's check and the time of day (`now`, Unix milliseconds); null (a failed
 * request) for a service it doesn't give. */
export function commonAnswer(service: string, body: Uint8Array, now: number): Uint8Array | null | Promise<Uint8Array | null> {
  switch (service) {
    case "crypto/random":
      return answerOf(() => random(body));
    case "crypto/verify":
      return verify(body).catch(() => null);
    case SERVICE_TIME:
      return body.length === 0 ? new Out().u64(BigInt(Math.floor(now))).bytes() : null;
    default:
      return null;
  }
}

/** The `wrela` imports, for the instance `me` gives, which `owner` runs. */
function imports_<C>(owner: Owner<C>, me: () => Instance<C>): WebAssembly.ModuleImports {
  return {
    [IMPORT_SUBMIT](at: number, len: number) {
      const self: Instance<C> = me();
      let commands: ReturnType<typeof decode>;
      try {
        commands = decode(self.read(at, len));
      } catch (e) {
        self.refuse(`a malformed command batch: ${(e as Error).message}`);
      }
      for (const c of commands) {
        if (c.op !== "HostRequest") self.refuse(`a server host can't carry out ${c.op}`);
        owner.submit(self, c.request, c.service, c.body);
      }
    },
    [IMPORT_REQUEST_STATUS](id: number): number {
      const self = me();
      if (!self.answered.has(id)) return REQUEST_PENDING;
      const a = self.answered.get(id);
      return a === null || a === undefined ? REQUEST_FAILED : a.length;
    },
    [IMPORT_REQUEST_TAKE](id: number, at: number) {
      const self = me();
      if (!self.answered.has(id)) self.refuse(`request_take(${id}): the request isn't answered`);
      const a = self.answered.get(id) ?? new Uint8Array(0);
      self.write(at, a, `request_take(${id}, ${at})`);
      self.answered.delete(id);
    },
    [IMPORT_LIMIT]: () => 0,
    [IMPORT_PRINT](at: number, len: number) {
      const print = owner.print;
      if (print !== undefined) print(LINES.decode(me().read(at, len)));
    },
    [IMPORT_INPUT](at: number, cap: number): number {
      const self = me();
      // A copy that doesn't fit fails the call, and its events go with its instance.
      const events = owner.queue.take(cap);
      events.forEach((e, i) => self.write(at + i * EVENT_SIZE, e, `input(${at}, ${cap})`));
      return events.length;
    },
    [IMPORT_PAYLOAD](lo: number, hi: number, at: number, cap: number): number {
      const self = me();
      const bytes = owner.queue.payload((lo >>> 0) + (hi >>> 0) * 2 ** 32);
      if (bytes === undefined) return -1;
      self.write(at, bytes.subarray(0, Math.min(cap, bytes.length)), `payload(.., ${at}, ${cap})`);
      return bytes.length;
    },
    [IMPORT_CLOCK](at: number) {
      const b = new Uint8Array(8);
      new DataView(b.buffer).setFloat64(0, owner.clock(), true);
      me().write(at, b, `clock(${at})`);
    },
    [IMPORT_PHASE]: () => {},
  };
}

/** A request waiting for its response. */
interface Open {
  resolve: (response: Uint8Array) => void;
  timer: ReturnType<typeof setTimeout>;
  /** Its bytes, for a fresh instance if the one it was queued for failed before reading it, and
   * whether it was given to one so already (once: a request every instance fails on is failed). */
  bytes: Uint8Array;
  retried: boolean;
}

/** How soon the alarm comes back for work left after a call, in case the object is reset first:
 * a call that finishes it sets the alarm again, so it comes only to an object that lost it. */
const RECOVER_MS = 1000;

/** The highest connection number, past which `std::net::POISONED` marks a message. */
export const POISONED = 2 ** 31;

/** A server build's HTTP instance, its store and its requests. */
export class Server implements Owner<Command> {
  readonly limits: Limits;
  readonly store: Store;
  /** The events the program hasn't read, and whether any came since its last call. */
  queue = new Inbox();
  private fresh = false;
  private instance: Instance<Command> | null = null;
  private starting: Promise<Instance<Command>> | null = null;
  /** Connections responded to, in the current call. */
  private readonly responding = new Set<number>();
  private readonly open = new Map<number, Open>();
  private nextConn = 1;
  private readonly started: number;
  private lastClock = 0;
  private pumping = false;
  /** Whether the alarm asked for a call though nothing came. */
  private woken = false;
  /** Whether the last call left events it was given a call's worth short of (its payload bytes),
   * and read some: the next call takes the rest. */
  private leftover = false;
  /** When the alarm this object set is due (0: none it knows of). */
  private alarmAt = 0;
  private readonly now: () => number;

  constructor(
    readonly module: WebAssembly.Module,
    storage: Storage,
    readonly options: Options = {},
  ) {
    this.limits = options.limits ?? LIMITS;
    this.now = options.now ?? Date.now;
    this.store = new Store(storage);
    this.started = this.now();
  }

  get print(): ((line: string) => void) | undefined {
    return this.options.print;
  }

  /** Seconds since the host started, never going back. */
  clock(): number {
    this.lastClock = Math.max(this.lastClock, (this.now() - this.started) / 1000);
    return this.lastClock;
  }

  /** Answers a request's bytes (`encodeRequest`) with a response's (`encodeResponse`). */
  request(bytes: Uint8Array): Promise<Uint8Array> {
    // What the object holds is bounded: past it, a request is turned away before it's queued.
    if (this.open.size >= this.limits.open || this.queue.held + bytes.length > this.limits.queued) return Promise.resolve(bare(503));
    const conn = this.nextConn++;
    if (this.nextConn >= POISONED) this.nextConn = 1;
    const answer = new Promise<Uint8Array>((resolve) => {
      const timer = setTimeout(() => this.finish(conn, bare(504)), RESPOND_WITHIN_MS);
      this.open.set(conn, { resolve, timer, bytes, retried: false });
    });
    this.queue.push(connectionEvent(conn, true));
    this.queue.pushMessage(conn, bytes);
    this.fresh = true;
    void this.pump();
    return answer;
  }

  /** The object's alarm: the work that no request waits on. An object woken from eviction starts
   * its instance and calls it once, so a program that keeps its queue in the store goes on. */
  async alarm(): Promise<void> {
    this.alarmAt = 0;
    this.woken = true;
    await this.pump();
  }

  /** Whether a call is due. */
  private due(): boolean {
    return this.fresh || this.leftover || this.woken || (this.instance?.arriving.length ?? 0) > 0;
  }

  /** Calls while a call is due, yielding between calls; past a turn's time, it leaves the rest to
   * the alarm, so no one invocation runs past its CPU limit. */
  async pump(): Promise<void> {
    // A pump under way sees what came since: it asks `due` after each call.
    if (this.pumping) return;
    this.pumping = true;
    try {
      const began = this.now();
      while (this.due()) {
        await this.once();
        // Past a turn, the rest goes on in the alarm: its own invocation, and its own CPU time.
        if (this.due() && this.now() - began >= this.limits.turn) {
          this.alarmAt = this.now();
          this.options.setAlarm?.(this.alarmAt);
          return;
        }
        await new Promise((r) => setTimeout(r, 0));
      }
    } finally {
      this.pumping = false;
    }
  }

  /** One call: the answers its last call's requests got, then `serve`, then its commands. */
  private async once(): Promise<void> {
    let instance: Instance<Command>;
    try {
      instance = await this.ready();
    } catch (e) {
      this.fail(e, false);
      return;
    }
    // Answers arrive before the call that may read them.
    await instance.settle();
    if (instance !== this.instance) return;
    this.fresh = false;
    this.woken = false;
    this.responding.clear();
    try {
      const time = Math.fround((this.now() - this.started) / 1000);
      const before = this.queue.length;
      instance.call(() => instance.serve(time));
      this.leftover = this.queue.length > 0 && this.queue.length < before;
      this.carryOut(instance.commands);
      // Work left (an answer due, events left) survives an eviction: the alarm brings it back.
      if (this.due() && this.alarmAt < this.now()) {
        this.alarmAt = this.now() + RECOVER_MS;
        this.options.setAlarm?.(this.alarmAt);
      }
    } catch (e) {
      this.fail(e);
    }
  }

  /** The instance, started if it isn't (with `serve_init`). */
  private ready(): Promise<Instance<Command>> {
    if (this.instance !== null) return Promise.resolve(this.instance);
    this.starting ??= (async () => {
      try {
        const i = await Instance.start<Command>(this, this.module);
        // What `serve_init` wrote commits before its first `serve`.
        this.carryOut(i.commands);
        this.instance = i;
        return i;
      } finally {
        this.starting = null;
      }
    })();
    return this.starting;
  }

  /** A call's writes committed, then its responses sent. */
  private carryOut(commands: Command[]): void {
    this.store.commit(commands.flatMap((c) => (c.kind === "write" ? [c.body] : [])));
    for (const c of commands) {
      if (c.kind === "respond") this.finish(c.conn, encodeResponse(withHostHeaders(c.response)));
    }
  }

  /** Request `conn` is answered with `response`, and closes for the program (unless `tell` is
   * false: its instance went, and the next never heard of it). */
  private finish(conn: number, response: Uint8Array, tell = true): void {
    const o = this.open.get(conn);
    if (o === undefined) return;
    clearTimeout(o.timer);
    this.open.delete(conn);
    o.resolve(response);
    if (tell) {
      this.queue.push(connectionEvent(conn, false));
      this.fresh = true;
      void this.pump();
    }
  }

  /** A call failed (or, with `started` false, an instance didn't start): the instance goes, and
   * every request open on it that a call was given with it. One no call has read yet waits for
   * the next instance, unless none starts. */
  private fail(e: unknown, started = true): void {
    const f = Failure.of(e);
    this.options.log?.(`wrela: the server's call failed: ${f.program ? "the program's: " : "the host's limits: "}${f.message}`);
    const unread = new Set<number>();
    if (started) {
      for (const ev of this.queue.events) {
        const v = new DataView(ev.buffer, ev.byteOffset, EVENT_SIZE);
        if (v.getUint32(0, true) === EventKind.Message) unread.add(v.getUint32(8, true));
      }
    }
    this.instance = null;
    this.woken = false;
    this.fresh = false;
    this.leftover = false;
    this.queue = new Inbox();
    for (const [conn, o] of [...this.open]) {
      if (unread.has(conn) && !o.retried) {
        o.retried = true;
        this.queue.push(connectionEvent(conn, true));
        this.queue.pushMessage(conn, o.bytes);
        this.fresh = true;
      } else {
        this.finish(conn, bare(f.program ? 500 : 503), false);
      }
    }
  }

  /** A host request the program made: a command for after the call, or a request answered for
   * the next. */
  submit(self: Instance<Command>, id: number, service: string, body: Uint8Array): void {
    switch (service) {
      case "store/write":
        return self.command({ kind: "write", body }, body.length);
      case SERVICE_RESPOND: {
        if (body.length < 4) self.refuse("`http/respond` needs a connection's number");
        const conn = new DataView(body.buffer, body.byteOffset).getUint32(0, true);
        if (!this.open.has(conn)) return; // the client went, or it timed out: nowhere to go
        if (this.responding.has(conn)) self.refuse(`request ${conn} takes one response`);
        let response: HttpResponse;
        try {
          response = checkResponse(body.subarray(4));
        } catch (e) {
          self.refuse(`\`http/respond\`: ${(e as Error).message}`);
        }
        this.responding.add(conn);
        return self.command({ kind: "respond", conn, response }, body.length);
      }
      case "net/send":
      case "net/close":
      case "net/tag":
      case "net/keep":
      case "net/compact":
        return self.refuse(`\`${service}\`: an HTTP server's connections take one response (\`std::http::respond\`)`);
      case "store/get":
      case "store/scan":
        return self.answer(id, this.store.answer(service, body));
      default:
        return self.answer(id, commonAnswer(service, body, this.now()));
    }
  }
}
