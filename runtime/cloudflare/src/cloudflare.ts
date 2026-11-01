// The Cloudflare host's glue (language.md §19.5): the Durable Object that runs each of a server
// build's named instances beside its store (host.ts), `api`, which the site's Worker calls first,
// so the site's own origin serves its API, and the edge instance `api` gives each request to,
// which sends it to an instance or answers it: a fresh one each request. One class, `WrelaServer`, holds every instance:
// instance `<kind>/<name>` is its object `getByName("<kind>/<name>")`, with a store of its own.
// An instance calls another, and sends it messages, over RPC, and calls an archived build's
// verifier through the Worker Loader (`WRELA_LOADER`), from the builds in `WRELA_BUILDS`.
//
// `api` refuses what it can before the object is billed: a path outside the prefix (null: not
// the API's), a method the host doesn't take (405), a client past one of its rate limits (429:
// `WRELA_LIMITS`, each a Workers Rate Limiting binding keyed on the client's address), a request
// that changes something from another origin (403: its Origin must be one of `WRELA_ORIGINS`, and
// `Sec-Fetch-Site`, when sent, `same-origin`), and a body past 1 MiB (413). A GET whose earlier
// answer was public it serves from the Cache API. It passes the object only the headers a server
// sees (runtime/abi `http`), so no address reaches the object or the program, and logs nothing:
// the address is a rate limit's key and nothing else.

import { DurableObject } from "cloudflare:workers";
import { HTTP_METHODS, MAX_HTTP_REQUEST, SAFE_METHODS } from "../../browser/src/abi.gen.ts";
import { Out, seenByServer } from "../../browser/src/http.ts";
import { pairOf, pairParam, roomName } from "../../browser/src/net.ts";
import { SERVER_INSTANCE, VERIFY_SCHEMA } from "../../browser/src/abi.gen.ts";
import { bare, decodeResponse, encodeRequest, type Header, HOST_HEADERS, NO_BODY, type Peers, Server } from "./host.ts";
import { Room, type StoreClient } from "./room.ts";

/** A Workers Rate Limiting binding (`ratelimits` in the Wrangler configuration). */
export interface RateLimit {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

/** A rate limit on the API's requests: the binding that counts them, the requests it counts (a
 * path prefix, and methods; none: every method), and its period in seconds (10 or 60, for
 * `retry-after`). */
export interface Limit {
  binding: string;
  prefix: string;
  methods?: string[];
  period?: number;
}

/** An instance's object, as another instance, a room or the Worker reaches it (RPC). Each call
 * names the instance, which the object keeps, since it can't read its own name. */
export interface ServerStub extends StoreClient {
  request(name: string, bytes: Uint8Array): Promise<Uint8Array>;
  call(name: string, from: string, body: Uint8Array): Promise<Uint8Array | null>;
  deliver(name: string, from: string, id: number, body: Uint8Array): Promise<boolean>;
}

/** An R2 bucket, as much of it as `call_build` reads. */
export interface Bucket {
  get(key: string): Promise<{ text(): Promise<string>; arrayBuffer(): Promise<ArrayBuffer> } | null>;
}

/** What a Worker Loader gives for an archived build (`env.WRELA_LOADER.get`). */
export interface WorkerCode {
  compatibilityDate: string;
  mainModule: string;
  modules: Record<string, string | { wasm: ArrayBuffer }>;
  globalOutbound: null;
  env: Record<string, never>;
}

/** The Worker Loader binding (`worker_loaders`), as much as `call_build` uses. */
export interface Loader {
  get(id: string, code: () => Promise<WorkerCode>): { getEntrypoint(): { verify(bytes: Uint8Array): Promise<Uint8Array | null> } };
}

/** What the Worker's configuration gives the host (server/cloudflare.json). */
export interface Env {
  WRELA_SERVER: { getByName(name: string): ServerStub };
  /** The Worker Loader, and the bucket of archived builds, for `call_build`. */
  WRELA_LOADER?: Loader;
  WRELA_BUILDS?: Bucket;
  /** The rooms' objects, one a room (`rooms`). */
  WRELA_ROOM?: { getByName(name: string): { fetch(request: Request): Promise<Response> } };
  /** The page origins whose requests may change something, comma-separated. */
  WRELA_ORIGINS?: string;
  /** The rate limits, as JSON: a list of `Limit`s. */
  WRELA_LIMITS?: string;
  /** "1": the program's printed lines go to the Worker's log. */
  WRELA_PRINT?: string;
  /** The rate limits' bindings, by name. */
  [binding: string]: unknown;
}

/** As much of the Cache API (`caches.default`) as the host uses. */
export interface ResponseCache {
  match(request: Request): Promise<Response | undefined>;
  put(request: Request, response: Response): Promise<void>;
}

export interface ApiOptions {
  /** The path the API is under (default `/api/`). */
  prefix?: string;
  /** The cache public GETs are served from (default: `caches.default`, where there is one). */
  cache?: ResponseCache;
  /** Where a response being cached waits once the Worker has answered (the `fetch` handler's
   * context); none: it's cached before the answer. */
  ctx?: { waitUntil(promise: Promise<unknown>): void };
}

/** The instance whose store the rooms share. */
export const SERVER_OBJECT = SERVER_INSTANCE;

/** A response of the host's own, with no body: its headers as any response of the API's. */
function refusal(status: number, headers: Record<string, string> = {}): Response {
  return new Response(null, { status, headers: [["cache-control", "no-store"], ...HOST_HEADERS, ...Object.entries(headers)] });
}

/** The body, at most `max` bytes: null if it's longer. */
async function bodyWithin(request: Request, max: number): Promise<Uint8Array | null> {
  const declared = request.headers.get("content-length");
  if (declared !== null && Number(declared) > max) return null;
  if (request.body === null) return new Uint8Array(0);
  const out = new Out();
  const reader = request.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (out.length + value.length > max) {
      await reader.cancel();
      return null;
    }
    out.push(value);
  }
  return out.bytes();
}

/** Whether a response may be kept for every client: a 200 that says `public` with a lifetime,
 * and sets no cookie and varies on nothing. */
export function shareable(r: Response): boolean {
  const control = r.headers.get("cache-control") ?? "";
  const life = /(?:^|[\s,])(?:s-maxage|max-age)=(\d+)/.exec(control);
  return (
    r.status === 200 &&
    /(?:^|[\s,])public(?:$|[\s,])/.test(control) &&
    life !== null &&
    Number(life[1]) > 0 &&
    !/(?:^|[\s,])(?:private|no-store|no-cache)/.test(control) &&
    !r.headers.has("set-cookie") &&
    !r.headers.has("vary")
  );
}

/** The rate limits from `WRELA_LIMITS`, read once for each value it has; a malformed list is a
 * configuration error. */
let limitsRead: { text: string; list: Limit[] } | null = null;
function limits(env: Env): Limit[] {
  const text = env.WRELA_LIMITS ?? "[]";
  if (limitsRead?.text === text) return limitsRead.list;
  const list: unknown = JSON.parse(text);
  if (!Array.isArray(list) || !list.every((l) => typeof l?.binding === "string" && typeof l?.prefix === "string")) {
    throw new Error("WRELA_LIMITS isn't a list of { binding, prefix, methods?, period? }");
  }
  limitsRead = { text, list: list as Limit[] };
  return limitsRead.list;
}

/** The page origins from `WRELA_ORIGINS`, read once for each value it has. */
let originsRead: { text: string; list: string[] } | null = null;
function origins(env: Env): string[] {
  const text = env.WRELA_ORIGINS ?? "";
  if (originsRead?.text !== text) originsRead = { text, list: text.split(",").map((o) => o.trim()).filter((o) => o !== "") };
  return originsRead.list;
}

/** The refusal of a client past one of its rate limits on `method` to `path` (429), or of a
 * Worker whose `WRELA_LIMITS` is wrong (503); null if neither. */
async function gate(env: Env, request: Request, method: string, path: string): Promise<Response | null> {
  let limited: Limit | null;
  try {
    limited = await pastLimit(env, method, path, request.headers.get("cf-connecting-ip") ?? "unknown");
  } catch (e) {
    // The Worker's configuration is wrong (never the client's address, which no error holds).
    console.error(`wrela: ${(e as Error).message}`);
    return refusal(503);
  }
  return limited === null ? null : refusal(429, { "retry-after": String(limited.period ?? 60) });
}

/** The first limit `method` on `path` is past, from this client's address: none if it's past
 * none. The address is each binding's key, and goes nowhere else. */
async function pastLimit(env: Env, method: string, path: string, address: string): Promise<Limit | null> {
  for (const l of limits(env)) {
    if (!path.startsWith(l.prefix) || (l.methods !== undefined && !l.methods.includes(method))) continue;
    const binding = env[l.binding] as RateLimit | undefined;
    if (typeof binding?.limit !== "function") throw new Error(`WRELA_LIMITS names \`${l.binding}\`, which isn't a rate limit binding`);
    if (!(await binding.limit({ key: address })).success) return l;
  }
  return null;
}

/** An RPC's bytes as a Uint8Array (structured clone gives one; anything else is copied). */
const bytesOf = (b: Uint8Array | ArrayBuffer) => (b instanceof Uint8Array ? b : new Uint8Array(b));

/** The other instances and the archived builds, as `env` reaches them: a failure is an answer
 * (503, null, false), never a throw. */
export function peersOf(env: Env, log: (line: string) => void = (l) => console.error(l)): Peers {
  const stub = (name: string) => env.WRELA_SERVER.getByName(name);
  return {
    // The object was reset (a call past the CPU limit, a deploy): the client may try again.
    request: (name, bytes) => stub(name).request(name, bytes).then(bytesOf, () => bare(503)),
    call: (name, from, body) => stub(name).call(name, from, body).then((b) => (b === null ? null : bytesOf(b)), () => null),
    deliver: (name, from, id, body) => stub(name).deliver(name, from, id, body).catch(() => false),
    callBuild: (artifact, body) => callBuild(env, artifact, body, log),
  };
}

/** What `verifier.json` says: its contract, the date its host was written against, its main
 * module and every module it has (`wrela build`'s `server/verifier/`). */
interface VerifierJson {
  schema: string;
  compatibility_date: string;
  main: string;
  modules: string[];
}

/** The reply of the verifier of the archived build at `artifact` (`std::server::call_build`):
 * its files are `<artifact>/verifier.json` and the modules it names in `WRELA_BUILDS`, loaded by
 * the Worker Loader under the artifact's path (a path names one build's bytes, so a warm one is
 * reused) with no bindings and no network. Null if there's no such build, or it failed. */
export async function callBuild(env: Env, artifact: string, body: Uint8Array, log: (line: string) => void): Promise<Uint8Array | null> {
  const loader = env.WRELA_LOADER;
  const builds = env.WRELA_BUILDS;
  if (loader === undefined || builds === undefined) {
    log("wrela: `call_build` needs the Worker's WRELA_LOADER and WRELA_BUILDS bindings");
    return null;
  }
  try {
    const worker = loader.get(artifact, async () => {
      const file = async (name: string) => {
        const o = await builds.get(`${artifact}/${name}`);
        if (o === null) throw new Error(`the archived build has no ${name}`);
        return o;
      };
      const meta = JSON.parse(await (await file("verifier.json")).text()) as VerifierJson;
      if (meta.schema !== VERIFY_SCHEMA) throw new Error(`the archived build's verifier is \`${meta.schema}\`, not \`${VERIFY_SCHEMA}\``);
      const modules: WorkerCode["modules"] = {};
      for (const name of meta.modules) {
        const o = await file(name);
        modules[name] = name.endsWith(".wasm") ? { wasm: await o.arrayBuffer() } : await o.text();
      }
      return { compatibilityDate: meta.compatibility_date, mainModule: meta.main, modules, globalOutbound: null, env: {} };
    });
    const reply = await worker.getEntrypoint().verify(body);
    return reply === null || reply === undefined ? null : bytesOf(reply);
  } catch (e) {
    log(`wrela: an archived build's verifier failed: ${(e as Error).message}`);
    return null;
  }
}

/** A new edge instance of `module` for a request to the Worker of `env`, with no store. It lasts
 * the request: the Workers runtime cancels what one request left waiting once it's answered, so
 * an instance kept for the next would wait forever. */
export function edgeOf(module: WebAssembly.Module, env: Env): Server {
  const log = (line: string) => console.error(line);
  const edge: Server = new Server(module, null, {
    peers: peersOf(env, log),
    // The Worker has no alarm: work past a turn goes on in a moment, while its request waits.
    setAlarm: (at) => void setTimeout(() => void edge.alarm(), Math.max(0, at - Date.now())),
    log,
    ...(env.WRELA_PRINT === "1" ? { print: (line: string) => console.log(line) } : {}),
  });
  return edge;
}

/** The API's response to `request`, or null if it isn't under the API's prefix: the edge
 * instance of `module` answers it, or sends it to an instance. */
export async function api(module: WebAssembly.Module, request: Request, env: Env, options: ApiOptions = {}): Promise<Response | null> {
  const url = new URL(request.url);
  const prefix = options.prefix ?? "/api/";
  if (!url.pathname.startsWith(prefix)) return null;
  const method = request.method.toUpperCase();
  if (!HTTP_METHODS.includes(method)) return refusal(405, { allow: HTTP_METHODS.join(", ") });
  const limited = await gate(env, request, method, url.pathname);
  if (limited !== null) return limited;
  // A GET's public answer, kept by its URL alone: it's the same whoever asks.
  const cache = method === "GET" ? (options.cache ?? (globalThis as { caches?: { default?: ResponseCache } }).caches?.default) : undefined;
  const key = cache === undefined ? null : new Request(url.href, { method: "GET" });
  if (cache !== undefined && key !== null) {
    const hit = await cache.match(key);
    if (hit !== undefined) return hit;
  }
  if (!SAFE_METHODS.includes(method)) {
    const origin = request.headers.get("origin");
    const site = request.headers.get("sec-fetch-site");
    if (origin === null || !origins(env).includes(origin) || (site !== null && site !== "same-origin")) return refusal(403);
  }
  const body = await bodyWithin(request, MAX_HTTP_REQUEST);
  if (body === null) return refusal(413);
  // Headers iterate in name order, lower case, with a name's values joined.
  const headers: Header[] = [];
  for (const [name, value] of request.headers) if (seenByServer(name)) headers.push([name, value]);
  const bytes = encodeRequest({ time: BigInt(Date.now()), method, path: url.pathname, query: url.search.slice(1), headers, body });
  if (bytes.length > MAX_HTTP_REQUEST) return refusal(413);
  const s = decodeResponse(await edgeOf(module, env).request(bytes));
  const out = new Headers();
  for (const [n, v] of s.headers) out.append(n, v);
  const empty = method === "HEAD" || NO_BODY.includes(s.status);
  const response = new Response(empty ? null : (s.body.slice() as Uint8Array<ArrayBuffer>), { status: s.status, headers: out });
  if (cache !== undefined && key !== null && shareable(response)) {
    const kept = cache.put(key, response.clone());
    if (options.ctx !== undefined) options.ctx.waitUntil(kept);
    else await kept;
  }
  return response;
}

/** Makes the Durable Object class for a server build's module: each object one named instance. */
export function serverClass(module: WebAssembly.Module) {
  return class WrelaServer extends DurableObject<Env> {
    readonly server: Server;

    constructor(ctx: DurableObjectState, env: Env) {
      super(ctx, env);
      const log = (line: string) => console.error(line);
      this.server = new Server(module, ctx.storage, {
        peers: peersOf(env, log),
        setAlarm: (at) => void ctx.storage.setAlarm(at),
        log,
        ...(env.WRELA_PRINT === "1" ? { print: (line: string) => console.log(line) } : {}),
      });
    }

    /** A request's bytes, which the edge sent instance `name`, answered with a response's. */
    async request(name: string, bytes: Uint8Array): Promise<Uint8Array> {
      this.server.name(name);
      return this.server.request(bytesOf(bytes));
    }

    /** A call from instance `from` to instance `name`, answered with its reply (null: none). */
    async call(name: string, from: string, body: Uint8Array): Promise<Uint8Array | null> {
      this.server.name(name);
      return this.server.call(from, bytesOf(body));
    }

    /** Message `id` from instance `from` to instance `name`: whether it was acknowledged. */
    async deliver(name: string, from: string, id: number, body: Uint8Array): Promise<boolean> {
      this.server.name(name);
      return this.server.message(from, id, bytesOf(body));
    }

    /** A room's read of the store (RPC from a room's object): answered from what's committed. */
    async answer(service: string, body: Uint8Array): Promise<Uint8Array | null> {
      return this.server.store!.answer(service, bytesOf(body));
    }

    /** A room's call's writes, committed in one transaction (RPC from a room's object). */
    async commit(batches: Uint8Array[]): Promise<void> {
      this.server.store!.commit(batches.map(bytesOf));
    }

    async alarm(): Promise<void> {
      await this.server.alarm();
    }
  };
}

/** A room's WebSocket: the response to `request` for `/room/<name>?pair=<16 hex digits>`, or null
 * for a path that isn't a room's. Before the room's object is billed it refuses what isn't an
 * upgrade (426), a room's name that isn't one (404), another program's pair (403), a page of
 * another origin (403: a browser's Origin must be one of `WRELA_ORIGINS`; a client with none
 * isn't a browser, and the room's protocol decides), and a client past its rate limits (429). Each
 * build's rooms are its own (`<pair>:<name>`), as `wrela-serve` keeps each build's apart. */
export async function rooms(request: Request, env: Env, pair: bigint | null): Promise<Response | null> {
  const url = new URL(request.url);
  if (!url.pathname.startsWith("/room/")) return null;
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") return refusal(426, { upgrade: "websocket" });
  const name = url.pathname.slice("/room/".length);
  if (!roomName(name)) return refusal(404);
  const hex = pair === null ? null : pairParam(pair);
  if (hex === null || url.searchParams.get("pair") !== hex) return refusal(403);
  const origin = request.headers.get("origin");
  if (origin !== null && !origins(env).includes(origin)) return refusal(403);
  const limited = await gate(env, request, "GET", url.pathname);
  if (limited !== null) return limited;
  if (env.WRELA_ROOM === undefined) return refusal(404);
  // Only the handshake crosses: no address, and no header the room doesn't read.
  const handshake = new Headers();
  for (const [n, v] of request.headers) if (n === "upgrade" || n === "connection" || n.startsWith("sec-websocket-")) handshake.set(n, v);
  return env.WRELA_ROOM.getByName(`${hex}:${name}`).fetch(new Request(url.href, { headers: handshake }));
}

/** Makes the Durable Object class of a server build's rooms. */
export function roomClass(module: WebAssembly.Module) {
  const pair = pairOf(module);
  return class WrelaRoom extends DurableObject<Env> {
    readonly room: Room;

    constructor(ctx: DurableObjectState, env: Env) {
      super(ctx, env);
      const server = env.WRELA_SERVER.getByName(SERVER_OBJECT);
      this.room = new Room(module, ctx.storage, server, ctx.getWebSockets(), {
        pair,
        log: (line) => console.error(line),
        ...(env.WRELA_PRINT === "1" ? { print: (line: string) => console.log(line) } : {}),
      });
    }

    /** A peer's upgrade, which `rooms` checked. */
    async fetch(_request: Request): Promise<Response> {
      const pair = new WebSocketPair();
      const [client, server] = [pair[0], pair[1]];
      this.ctx.acceptWebSocket(server);
      this.room.connect(server);
      return new Response(null, { status: 101, webSocket: client });
    }

    // Each event lasts until the room has taken it, so the runtime neither cancels nor hibernates
    // the call it started.
    async webSocketMessage(ws: WebSocket, message: ArrayBuffer | string): Promise<void> {
      this.room.message(ws, message);
      await this.room.settled;
    }

    async webSocketClose(ws: WebSocket): Promise<void> {
      this.room.closed(ws);
      // Before compatibility date 2026-04-07 the object finishes the close handshake itself.
      try {
        ws.close();
      } catch {
        // It's closed already.
      }
      await this.room.settled;
    }

    async webSocketError(ws: WebSocket): Promise<void> {
      this.room.closed(ws);
      await this.room.settled;
    }

    async alarm(): Promise<void> {
      await this.room.alarm();
    }
  };
}
