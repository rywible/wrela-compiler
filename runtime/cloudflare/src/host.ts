// A server build over HTTP (language.md §19.5): one instance of `server/game.wasm`, the store
// beside it, and the requests, calls and messages it answers. A named instance is a Durable
// Object's whole job (cloudflare.ts); the edge instance, with no store, runs in the Worker in
// front of them. It is written against small interfaces (`Storage`, `Options`, `Peers`) so the
// tests run it on bun:sqlite.
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
// - **A call to another instance, and a message, is a short connection too**: a `Message` whose
//   bytes start `WSC1` or `WSM1`, answered by `server/reply` or acknowledged by `server/ack`.
//   Calls, sends, replies, acknowledgements and dispatches are commands, carried out once the
//   call's writes are committed. A send is written to the outbox in the same transaction, and
//   tried until its receiver acknowledges it, with backoff up to an hour.
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
  CALL_MAGIC,
  MAX_INSTANCE_NAME,
  MESSAGE_MAGIC,
  REQUEST_FAILED,
  REQUEST_PENDING,
  RESPOND_WITHIN_MS,
  RETRY_CAP_MS,
  SERVER_INSTANCE,
  SERVICE_ACK,
  SERVICE_CALL,
  SERVICE_CALL_BUILD,
  SERVICE_DISPATCH,
  SERVICE_HMAC,
  SERVICE_ME,
  SERVICE_REPLY,
  SERVICE_RESPOND,
  SERVICE_SEND,
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

/** The other instances, and the archived builds, as an instance reaches them. Each answers
 * failure (null, false, a 503) rather than throwing. */
export interface Peers {
  /** Instance `name`'s response to a request's bytes. */
  request(name: string, bytes: Uint8Array): Promise<Uint8Array>;
  /** Instance `name`'s reply to a call from `from`; null if it failed or didn't reply. */
  call(name: string, from: string, body: Uint8Array): Promise<Uint8Array | null>;
  /** Whether instance `name` acknowledged message `id` from `from` once its writes committed. */
  deliver(name: string, from: string, id: number, body: Uint8Array): Promise<boolean>;
  /** The archived build at `artifact`'s verifier's reply; null if it failed or isn't there. */
  callBuild(artifact: string, body: Uint8Array): Promise<Uint8Array | null>;
}

export interface Options {
  limits?: Limits;
  /** Where calls, messages and dispatches go; none: each fails. */
  peers?: Peers;
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

/** A message waiting in the outbox: where to, its bytes, when it was sent (Unix ms), how many
 * tries it has had, when it's next due, and whether its age has been logged. */
export interface Waiting {
  id: number;
  dest: string;
  body: Uint8Array;
  made: number;
  tries: number;
}

/** A message a call sends: where to, and its bytes. */
export interface Send {
  to: string;
  body: Uint8Array;
}

/** The server's store: one table of keys and values, both bytes, as `store.sqlite` on
 * `wrela-serve`; beside it, the instance's outbox and its name. */
export class Store {
  constructor(readonly storage: Storage) {
    storage.sql.exec("CREATE TABLE IF NOT EXISTS kv (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID");
    // AUTOINCREMENT: an id is never used twice, so a receiver can tell a message come again.
    storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS wrela_outbox (id INTEGER PRIMARY KEY AUTOINCREMENT, dest TEXT NOT NULL, body BLOB NOT NULL, made INTEGER NOT NULL, tries INTEGER NOT NULL, due INTEGER NOT NULL, told INTEGER NOT NULL)",
    );
    storage.sql.exec("CREATE TABLE IF NOT EXISTS wrela_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)");
  }

  /** The instance's name, as its first call, request or message named it; null before. */
  get name(): string | null {
    const rows = this.storage.sql.exec("SELECT value FROM wrela_meta WHERE key = 'name'").toArray();
    return rows.length === 0 ? null : String(rows[0]!["value"]);
  }

  set name(name: string) {
    this.storage.sql.exec("INSERT OR REPLACE INTO wrela_meta (key, value) VALUES ('name', ?)", name);
  }

  /** The messages due at `now`, oldest first, at most `limit`. */
  due(now: number, limit: number): Waiting[] {
    return this.storage.sql
      .exec("SELECT id, dest, body, made, tries FROM wrela_outbox WHERE due <= ? ORDER BY id LIMIT ?", now, limit)
      .toArray()
      .map((r) => ({ id: Number(r["id"]), dest: String(r["dest"]), body: asBytes(r["body"]), made: Number(r["made"]), tries: Number(r["tries"]) }));
  }

  /** When the next message is due; null if none waits. */
  nextDue(): number | null {
    const rows = this.storage.sql.exec("SELECT MIN(due) AS at FROM wrela_outbox").toArray();
    const at = rows[0]?.["at"];
    return at === null || at === undefined ? null : Number(at);
  }

  /** Message `id` was acknowledged: it goes. */
  acknowledged(id: number): void {
    this.storage.sql.exec("DELETE FROM wrela_outbox WHERE id = ?", id);
  }

  /** Message `id` wasn't acknowledged: its tries, and when it's next due. Whether it's the first
   * time it's older than `RETRY_CAP_MS` (`now` is the time). */
  missed(id: number, tries: number, due: number, now: number): boolean {
    const old = this.storage.sql.exec("SELECT made, told FROM wrela_outbox WHERE id = ?", id).toArray()[0];
    if (old === undefined) return false;
    const tell = Number(old["told"]) === 0 && now - Number(old["made"]) >= RETRY_CAP_MS;
    this.storage.sql.exec("UPDATE wrela_outbox SET tries = ?, due = ?, told = ? WHERE id = ?", tries, due, tell || Number(old["told"]) === 1 ? 1 : 0, id);
    return tell;
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

  /** Commits writes (`store/write` bodies) and a call's sends, into the outbox, due at `now`,
   * in one transaction, all or none. A malformed body is the program's, and commits nothing. */
  commit(batches: Uint8Array[], sends: Send[] = [], now = 0): void {
    const ops = writes(batches);
    if (ops.length === 0 && sends.length === 0) return;
    this.storage.transactionSync(() => {
      for (const [k, v] of ops) {
        if (v === null) this.storage.sql.exec("DELETE FROM kv WHERE key = ?", blob(k));
        else this.storage.sql.exec("INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)", blob(k), blob(v));
      }
      for (const m of sends) {
        this.storage.sql.exec("INSERT INTO wrela_outbox (dest, body, made, tries, due, told) VALUES (?, ?, ?, 0, ?, 0)", m.to, blob(m.body), now, now);
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

/** `crypto/hmac`: a key and a message → HMAC-SHA256 of the message under the key, 32 bytes
 * (RFC 2104). WebCrypto refuses an empty key, which is the same key as 64 zero bytes. */
export async function hmac(body: Uint8Array): Promise<Uint8Array> {
  const r = new In(body);
  const key = r.chunk().slice() as Uint8Array<ArrayBuffer>;
  const message = r.chunk().slice() as Uint8Array<ArrayBuffer>;
  r.end("service");
  const k = await crypto.subtle.importKey("raw", key.length === 0 ? new Uint8Array(64) : key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", k, message));
}

// ---- instances' names, calls and messages (runtime/abi `http`) ---------------------------------

/** Whether `name` names an instance: `server`, or a kind and a name (runtime/abi
 * `instance_name`). */
export function instanceName(name: string): boolean {
  return name === SERVER_INSTANCE || (name.includes("/") && artifactPath(name));
}

/** Whether `path` names an archived build (runtime/abi `artifact_path`). */
export function artifactPath(path: string): boolean {
  return path.length > 0 && path.length <= MAX_INSTANCE_NAME && path.split("/").every((p) => p !== "" && p !== "." && p !== ".." && /^[A-Za-z0-9._-]+$/.test(p));
}

/** A call's bytes, as its callee reads them (`std::server::called`). */
export const encodeCall = (from: string, body: Uint8Array): Uint8Array => new Out().word(CALL_MAGIC).text(from).chunk(body).bytes();

/** A message's bytes, as its receiver reads them (`std::server::delivered`). */
export const encodeMessage = (from: string, id: number, body: Uint8Array): Uint8Array =>
  new Out().word(MESSAGE_MAGIC).text(from).u64(BigInt(id)).chunk(body).bytes();

/** How long a message waits after its `tries`-th miss: a second, doubling, up to an hour. */
export const backoff = (tries: number): number => Math.min(1000 * 2 ** Math.min(tries - 1, 22), RETRY_CAP_MS);

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

/** A call's command the host carries out after it returns: once its writes and sends commit. */
type Command =
  | { kind: "write"; body: Uint8Array }
  | { kind: "respond"; conn: number; response: HttpResponse }
  | { kind: "dispatch"; conn: number; to: string | number }
  | { kind: "call"; id: number; to: string; body: Uint8Array }
  | { kind: "build"; id: number; artifact: string; body: Uint8Array }
  | { kind: "reply"; conn: number; body: Uint8Array }
  | { kind: "send"; to: string; body: Uint8Array }
  | { kind: "ack"; conn: number };

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
  /** An answer `self` waited for arrived between calls (`Instance.later`): a call is due. */
  arrived?(self: Instance<C>): void;
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

  /** Request `id`'s answer, readable from the call after it arrives, however many calls run
   * meanwhile: a call to another instance, which may take seconds, and may call back. */
  later(id: number, a: Promise<Uint8Array | null>): void {
    void a
      .catch(() => null)
      .then((x) => {
        this.answered.set(id, x);
        this.owner.arrived?.(this);
      });
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
    case SERVICE_HMAC:
      return hmac(body).catch(() => null);
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

/** What a connection is: a request, answered by `http/respond` (or, at the edge, `server/dispatch`),
 * a call, answered by `server/reply`, or a message, acknowledged by `server/ack`. */
type Kind = "request" | "call" | "message";

/** A connection waiting for its answer. */
interface Open {
  kind: Kind;
  /** Its answer: a response's bytes for a request; a reply's for a call, or an empty one for a
   * message acknowledged; null for a call or a message that wasn't. */
  resolve: (answer: Uint8Array | null) => void;
  timer: ReturnType<typeof setTimeout>;
  /** Its bytes, for a fresh instance if the one it was queued for failed before reading it, and
   * whether it was given to one so already (once: a request every instance fails on is failed). */
  bytes: Uint8Array;
  retried: boolean;
}

/** How soon the alarm comes back for work left after a call, in case the object is reset first:
 * a call that finishes it sets the alarm again, so it comes only to an object that lost it. */
const RECOVER_MS = 1000;

/** The most messages one round of the outbox tries at once. */
const ROUND = 64;

/** The highest connection number, past which `std::net::POISONED` marks a message. */
export const POISONED = 2 ** 31;

/** A connection's answer when it gets none: 504 for a request, nothing for a call or a message. */
const unanswered = (kind: Kind): Uint8Array | null => (kind === "request" ? bare(504) : null);

/** A named instance of a server build, with its store and outbox; or, with no storage, the edge
 * instance, with neither. It answers requests, calls and messages. */
export class Server implements Owner<Command> {
  readonly limits: Limits;
  /** The store; null at the edge. */
  readonly store: Store | null;
  /** The events the program hasn't read, and whether any came since its last call. */
  queue = new Inbox();
  private fresh = false;
  private instance: Instance<Command> | null = null;
  private starting: Promise<Instance<Command>> | null = null;
  /** Connections answered, in the current call. */
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
  /** The outbox's messages being tried now. */
  private readonly trying = new Set<number>();
  /** The instance's name: empty at the edge; null until a request, call or message names it. */
  private named: string | null;

  constructor(
    readonly module: WebAssembly.Module,
    storage: Storage | null,
    readonly options: Options = {},
  ) {
    this.limits = options.limits ?? LIMITS;
    this.now = options.now ?? Date.now;
    this.store = storage === null ? null : new Store(storage);
    this.named = this.store === null ? "" : this.store.name;
    this.started = this.now();
    // Messages a reset object hadn't delivered go on from its alarm.
    const due = this.store?.nextDue() ?? null;
    if (due !== null) this.schedule(due);
  }

  get print(): ((line: string) => void) | undefined {
    return this.options.print;
  }

  /** The instance's name, which a request, a call or a message to it says (kept in its store, for
   * an object its alarm wakes). */
  name(name: string): void {
    if (this.store === null || this.named === name) return;
    this.named = name;
    this.store.name = name;
  }

  /** Seconds since the host started, never going back. */
  clock(): number {
    this.lastClock = Math.max(this.lastClock, (this.now() - this.started) / 1000);
    return this.lastClock;
  }

  /** Answers a request's bytes (`encodeRequest`) with a response's (`encodeResponse`). */
  request(bytes: Uint8Array): Promise<Uint8Array> {
    return this.connect("request", bytes, bare(503)) as Promise<Uint8Array>;
  }

  /** Answers a call from instance `from` (empty: the edge) with the program's reply; null if it
   * didn't reply, or failed. */
  call(from: string, body: Uint8Array): Promise<Uint8Array | null> {
    return this.connect("call", encodeCall(from, body), null);
  }

  /** Gives the program message `id` from instance `from`: whether it acknowledged it, once its
   * writes committed. */
  async message(from: string, id: number, body: Uint8Array): Promise<boolean> {
    return (await this.connect("message", encodeMessage(from, id, body), null)) !== null;
  }

  /** A connection of `kind` whose one message is `bytes`, answered as `Open.resolve` says; `full`
   * is its answer if the object holds too much to take it. */
  private connect(kind: Kind, bytes: Uint8Array, full: Uint8Array | null): Promise<Uint8Array | null> {
    // What the object holds is bounded: past it, a connection is turned away before it's queued.
    if (this.open.size >= this.limits.open || this.queue.held + bytes.length > this.limits.queued) return Promise.resolve(full);
    const conn = this.nextConn++;
    if (this.nextConn >= POISONED) this.nextConn = 1;
    const answer = new Promise<Uint8Array | null>((resolve) => {
      const timer = setTimeout(() => this.finish(conn, unanswered(kind)), RESPOND_WITHIN_MS);
      this.open.set(conn, { kind, resolve, timer, bytes, retried: false });
    });
    this.queue.push(connectionEvent(conn, true));
    this.queue.pushMessage(conn, bytes);
    this.fresh = true;
    void this.pump();
    return answer;
  }

  /** The object's alarm: the work that no request waits on, and the outbox. An object woken from
   * eviction starts its instance and calls it once, so a program that keeps its queue in the
   * store goes on. */
  async alarm(): Promise<void> {
    this.alarmAt = 0;
    this.woken = true;
    await Promise.all([this.pump(), this.deliver()]);
  }

  /** An answer an instance waited for arrived. */
  arrived(self: Instance<Command>): void {
    if (self !== this.instance) return;
    this.fresh = true;
    void this.pump();
  }

  /** Asks for the alarm at `at`, unless it's due sooner already. */
  private schedule(at: number): void {
    if (this.alarmAt !== 0 && this.alarmAt <= at) return;
    this.alarmAt = at;
    this.options.setAlarm?.(at);
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
          this.schedule(this.now());
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
      this.carryOut(instance, instance.commands);
      // Work left (an answer due, events left) survives an eviction: the alarm brings it back.
      if (this.due() && (this.alarmAt === 0 || this.alarmAt < this.now())) {
        this.alarmAt = 0;
        this.schedule(this.now() + RECOVER_MS);
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
        this.carryOut(i, i.commands);
        this.instance = i;
        return i;
      } finally {
        this.starting = null;
      }
    })();
    return this.starting;
  }

  /** A call's writes and sends committed, then its answers, calls and acknowledgements sent. */
  private carryOut(instance: Instance<Command>, commands: Command[]): void {
    const sends = commands.flatMap((c) => (c.kind === "send" ? [{ to: c.to, body: c.body }] : []));
    this.store?.commit(
      commands.flatMap((c) => (c.kind === "write" ? [c.body] : [])),
      sends,
      this.now(),
    );
    const peers = this.options.peers;
    for (const c of commands) {
      switch (c.kind) {
        case "respond":
          this.finish(c.conn, encodeResponse(withHostHeaders(c.response)));
          break;
        case "dispatch": {
          const o = this.open.get(c.conn);
          if (o === undefined) break;
          if (typeof c.to === "number") this.finish(c.conn, bare(c.to));
          else if (peers === undefined) this.finish(c.conn, bare(503));
          else void peers.request(c.to, o.bytes).catch(() => bare(503)).then((r) => this.finish(c.conn, r));
          break;
        }
        case "reply":
          this.finish(c.conn, c.body);
          break;
        case "ack":
          this.finish(c.conn, new Uint8Array(0));
          break;
        case "call":
          instance.later(c.id, peers?.call(c.to, this.named ?? "", c.body) ?? Promise.resolve(null));
          break;
        case "build":
          instance.later(c.id, peers?.callBuild(c.artifact, c.body) ?? Promise.resolve(null));
          break;
      }
    }
    if (sends.length > 0) {
      // The alarm comes back for them if this object goes before they're delivered.
      this.schedule(this.now() + RECOVER_MS);
      void this.deliver();
    }
  }

  /** Tries the outbox's messages that are due, each once, at most `ROUND` at a time: one
   * acknowledged goes, one not waits twice as long as before (up to an hour), and a message
   * older than an hour writes a line to the log once, naming its receiver's kind alone. Then the
   * alarm is set for the next one due. */
  async deliver(): Promise<void> {
    const store = this.store;
    const peers = this.options.peers;
    if (store === null || peers === undefined) return;
    const from = this.named ?? "";
    const round = store.due(this.now(), ROUND + this.trying.size).filter((m) => !this.trying.has(m.id));
    await Promise.all(
      round.map(async (m) => {
        this.trying.add(m.id);
        const ok = await peers.deliver(m.dest, from, m.id, m.body).catch(() => false);
        this.trying.delete(m.id);
        if (ok) {
          store.acknowledged(m.id);
          return;
        }
        const now = this.now();
        if (store.missed(m.id, m.tries + 1, now + backoff(m.tries + 1), now)) {
          this.options.log?.(`wrela: a message to a \`${m.dest.split("/")[0]}\` instance has waited more than an hour (${m.tries + 1} tries)`);
        }
      }),
    );
    const next = store.nextDue();
    if (next === null) return;
    if (next <= this.now() && round.length > 0) return this.deliver();
    this.alarmAt = 0;
    this.schedule(Math.max(next, this.now() + (round.length > 0 ? 0 : RECOVER_MS)));
  }

  /** Connection `conn` is answered with `answer`, and closes for the program (unless `tell` is
   * false: its instance went, and the next never heard of it). */
  private finish(conn: number, answer: Uint8Array | null, tell = true): void {
    const o = this.open.get(conn);
    if (o === undefined) return;
    clearTimeout(o.timer);
    this.open.delete(conn);
    o.resolve(answer);
    if (tell) {
      this.queue.push(connectionEvent(conn, false));
      this.fresh = true;
      void this.pump();
    }
  }

  /** A call failed (or, with `started` false, an instance didn't start): the instance goes, and
   * every connection open on it that a call was given with it. One no call has read yet waits for
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
        this.finish(conn, o.kind === "request" ? bare(f.program ? 500 : 503) : null, false);
      }
    }
  }

  /** Connection `conn`, from a command's body (its first word), if it's open and of `kind`:
   * else null if it closed (the client went, or it timed out: nowhere to go), and a refusal if
   * it's another kind's, or answered already in this call. */
  private answering(self: Instance<Command>, service: string, body: Uint8Array, kind: Kind): number | null {
    if (body.length < 4) self.refuse(`\`${service}\` needs a connection's number`);
    const conn = new DataView(body.buffer, body.byteOffset).getUint32(0, true);
    const o = this.open.get(conn);
    if (o === undefined) return null;
    if (o.kind !== kind) {
      self.refuse(`\`${service}\` answers a ${kind}, and connection ${conn} is a ${o.kind}: a request takes \`http/respond\`, a call \`server/reply\`, a message \`server/ack\``);
    }
    if (this.responding.has(conn)) self.refuse(`${kind} ${conn} takes one answer`);
    this.responding.add(conn);
    return conn;
  }

  /** A host request the program made: a command for after the call, or a request answered for
   * the next (at once, for its name). */
  submit(self: Instance<Command>, id: number, service: string, body: Uint8Array): void {
    const edge = this.store === null;
    const named = (r: In, what: string, valid: (s: string) => boolean): string => {
      const name = r.text();
      if (!valid(name)) self.refuse(`\`${service}\`: \`${name}\` isn't ${what}`);
      return name;
    };
    switch (service) {
      case "store/write":
        if (edge) self.refuse("`store/write`: the edge has no store: `call` an instance that has one");
        return self.command({ kind: "write", body }, body.length);
      case SERVICE_RESPOND: {
        const conn = this.answering(self, service, body, "request");
        if (conn === null) return;
        let response: HttpResponse;
        try {
          response = checkResponse(body.subarray(4));
        } catch (e) {
          self.refuse(`\`http/respond\`: ${(e as Error).message}`);
        }
        return self.command({ kind: "respond", conn, response }, body.length);
      }
      case SERVICE_DISPATCH: {
        if (!edge) self.refuse("`server/dispatch` is the edge's: an instance answers its requests itself");
        const conn = this.answering(self, service, body, "request");
        const r = new In(body.subarray(4));
        let to: string | number;
        try {
          const how = r.byte();
          if (how === 1) to = named(r, "an instance's name", instanceName);
          else if (how === 2) to = r.word();
          else throw new Error(`a dispatch of kind ${how}`);
          r.end("dispatch");
        } catch (e) {
          if (e instanceof Failure) throw e;
          self.refuse(`\`server/dispatch\`: ${(e as Error).message}`);
        }
        if (typeof to === "number" && (to < 400 || to > 599)) self.refuse(`\`server/dispatch\`: a refusal's status is 400 to 599, not ${to}`);
        if (conn === null) return;
        return self.command({ kind: "dispatch", conn, to }, body.length);
      }
      case SERVICE_REPLY: {
        const conn = this.answering(self, service, body, "call");
        const r = new In(body.subarray(4));
        let reply: Uint8Array;
        try {
          reply = r.chunk();
          r.end("reply");
        } catch (e) {
          self.refuse(`\`server/reply\`: ${(e as Error).message}`);
        }
        if (conn === null) return;
        return self.command({ kind: "reply", conn, body: reply }, body.length);
      }
      case SERVICE_ACK: {
        if (body.length !== 4) self.refuse("`server/ack` takes a connection's number");
        const conn = this.answering(self, service, body, "message");
        if (conn === null) return;
        return self.command({ kind: "ack", conn }, body.length);
      }
      case SERVICE_CALL:
      case SERVICE_SEND:
      case SERVICE_CALL_BUILD: {
        if (service === SERVICE_SEND && edge) self.refuse("`server/send`: the edge has no outbox: `call` an instance that sends");
        const r = new In(body);
        let to: string;
        let rest: Uint8Array;
        try {
          to = service === SERVICE_CALL_BUILD ? named(r, "an artifact's path", artifactPath) : named(r, "an instance's name", instanceName);
          rest = r.chunk();
          r.end("service");
        } catch (e) {
          if (e instanceof Failure) throw e;
          self.refuse(`\`${service}\`: ${(e as Error).message}`);
        }
        if (service === SERVICE_SEND) return self.command({ kind: "send", to, body: rest }, body.length);
        if (service === SERVICE_CALL) return self.command({ kind: "call", id, to, body: rest }, body.length);
        return self.command({ kind: "build", id, artifact: to, body: rest }, body.length);
      }
      case SERVICE_ME:
        // Answered at once: the program reads it in the call that asks.
        if (this.named === null) return self.answer(id, null);
        self.answered.set(id, new TextEncoder().encode(this.named));
        return;
      case "net/send":
      case "net/close":
      case "net/tag":
      case "net/keep":
      case "net/compact":
        return self.refuse(`\`${service}\`: an HTTP server's connections take one answer (\`std::http::respond\`)`);
      case "store/get":
      case "store/scan":
        if (this.store === null) return self.refuse(`\`${service}\`: the edge has no store: \`call\` an instance that has one`);
        return self.answer(id, this.store.answer(service, body));
      default:
        return self.answer(id, commonAnswer(service, body, this.now()));
    }
  }
}
