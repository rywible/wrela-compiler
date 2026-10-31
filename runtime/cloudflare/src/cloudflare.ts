// The Cloudflare host's glue (language.md §19.5): the Durable Object that runs a server build's
// HTTP instance beside its store (host.ts), and `api`, which the site's Worker calls first, so
// the site's own origin serves its API.
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
import { HTTP_METHODS, MAX_HTTP_REQUEST, PAIR_SECTION, SAFE_METHODS } from "../../browser/src/abi.gen.ts";
import { Out, seenByServer } from "../../browser/src/http.ts";
import { bare, decodeResponse, encodeRequest, type Header, HOST_HEADERS, NO_BODY, Server } from "./host.ts";
import { MAX_UP, Room, type Socket, type StoreClient } from "./room.ts";

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

/** What the Worker's configuration gives the host (server/cloudflare.json). */
export interface Env {
  WRELA_SERVER: { getByName(name: string): { request(bytes: Uint8Array): Promise<Uint8Array> } & StoreClient };
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

/** The one object that holds the store and runs the program's HTTP instance. */
export const SERVER_OBJECT = "server";

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

/** The API's response to `request`, or null if it isn't under the API's prefix. */
export async function api(request: Request, env: Env, options: ApiOptions = {}): Promise<Response | null> {
  const url = new URL(request.url);
  const prefix = options.prefix ?? "/api/";
  if (!url.pathname.startsWith(prefix)) return null;
  const method = request.method.toUpperCase();
  if (!HTTP_METHODS.includes(method)) return refusal(405, { allow: HTTP_METHODS.join(", ") });
  let limited: Limit | null;
  try {
    limited = await pastLimit(env, method, url.pathname, request.headers.get("cf-connecting-ip") ?? "unknown");
  } catch (e) {
    // The Worker's configuration is wrong (never the client's address, which no error holds).
    console.error(`wrela: ${(e as Error).message}`);
    return refusal(503);
  }
  if (limited !== null) return refusal(429, { "retry-after": String(limited.period ?? 60) });
  // A GET's public answer, kept by its URL alone: it's the same whoever asks.
  const cache = method === "GET" ? (options.cache ?? (globalThis as { caches?: { default?: ResponseCache } }).caches?.default) : undefined;
  const key = cache === undefined ? null : new Request(url.href, { method: "GET" });
  if (cache !== undefined && key !== null) {
    const hit = await cache.match(key);
    if (hit !== undefined) return hit;
  }
  if (!SAFE_METHODS.includes(method)) {
    const origins = (env.WRELA_ORIGINS ?? "").split(",").map((o) => o.trim()).filter((o) => o !== "");
    const origin = request.headers.get("origin");
    const site = request.headers.get("sec-fetch-site");
    if (origin === null || !origins.includes(origin) || (site !== null && site !== "same-origin")) return refusal(403);
  }
  const body = await bodyWithin(request, MAX_HTTP_REQUEST);
  if (body === null) return refusal(413);
  // Headers iterate in name order, lower case, with a name's values joined.
  const headers: Header[] = [];
  for (const [name, value] of request.headers) if (seenByServer(name)) headers.push([name, value]);
  const bytes = encodeRequest({ time: BigInt(Date.now()), method, path: url.pathname, query: url.search.slice(1), headers, body });
  if (bytes.length > MAX_HTTP_REQUEST) return refusal(413);
  let answer: Uint8Array;
  try {
    answer = await env.WRELA_SERVER.getByName(SERVER_OBJECT).request(bytes);
  } catch {
    // The object was reset (a call past the CPU limit, a deploy): the client may try again.
    answer = bare(503);
  }
  const s = decodeResponse(answer);
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

/** Makes the Durable Object class for a server build's module. */
export function serverClass(module: WebAssembly.Module) {
  return class WrelaServer extends DurableObject<Env> {
    readonly server: Server;

    constructor(ctx: DurableObjectState, env: Env) {
      super(ctx, env);
      this.server = new Server(module, ctx.storage, {
        setAlarm: (at) => void ctx.storage.setAlarm(at),
        log: (line) => console.error(line),
        ...(env.WRELA_PRINT === "1" ? { print: (line: string) => console.log(line) } : {}),
      });
    }

    /** A request's bytes, answered with a response's (RPC from `api`). */
    async request(bytes: Uint8Array): Promise<Uint8Array> {
      return this.server.request(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
    }

    /** A room's read of the store (RPC from a room's object): answered from what's committed. */
    async answer(service: string, body: Uint8Array): Promise<Uint8Array | null> {
      return this.server.storeAnswer(service, new Uint8Array(body));
    }

    /** A room's call's writes, committed in one transaction (RPC from a room's object). */
    async commit(batches: Uint8Array[]): Promise<void> {
      this.server.store.commit(batches.map((b) => new Uint8Array(b)));
    }

    async alarm(): Promise<void> {
      await this.server.alarm();
    }
  };
}

/** Whether `name` names a room: 1 to 64 of a-z, 0-9, `-` and `_` (`wrela_serve::room_name`). */
export const roomName = (name: string) => /^[a-z0-9_-]{1,64}$/.test(name);

/** A room's WebSocket: the response to `request` for `/room/<name>?pair=<16 hex digits>`, or null
 * for a path that isn't a room's. Before the room's object is billed it refuses what isn't an
 * upgrade (426), a room's name that isn't one (404), another program's pair (403), a page of
 * another origin (403: a browser's Origin must be one of `WRELA_ORIGINS`; a client with none
 * isn't a browser, and the room's protocol decides), and a client past its rate limits (429). Each
 * build's rooms are its own (`<pair>:<name>`), as `wrela-serve` keeps each build's apart. */
export async function rooms(request: Request, env: Env, pair: bigint | null): Promise<Response | null> {
  const url = new URL(request.url);
  if (!url.pathname.startsWith("/room/")) return null;
  if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") return refusal(426, { upgrade: "websocket" });
  const name = url.pathname.slice("/room/".length);
  if (!roomName(name)) return refusal(404);
  const asked = url.searchParams.get("pair");
  const hex = pair === null ? null : pair.toString(16).padStart(16, "0");
  if (hex === null || asked !== hex) return refusal(403);
  const origin = request.headers.get("origin");
  const origins = (env.WRELA_ORIGINS ?? "").split(",").map((o) => o.trim()).filter((o) => o !== "");
  if (origin !== null && !origins.includes(origin)) return refusal(403);
  let limited: Limit | null;
  try {
    limited = await pastLimit(env, "GET", url.pathname, request.headers.get("cf-connecting-ip") ?? "unknown");
  } catch (e) {
    console.error(`wrela: ${(e as Error).message}`);
    return refusal(503);
  }
  if (limited !== null) return refusal(429, { "retry-after": String(limited.period ?? 60) });
  if (env.WRELA_ROOM === undefined) return refusal(404);
  // Only the upgrade crosses: no address, and no header the room doesn't read.
  return env.WRELA_ROOM.getByName(`${hex}:${name}`).fetch(new Request(url.href, { headers: { upgrade: "websocket" } }));
}

/** The pair id in a server build's module (`wrela.pair`), which its clients carry. */
export function pairOf(module: WebAssembly.Module): bigint | null {
  const [section] = WebAssembly.Module.customSections(module, PAIR_SECTION);
  return section !== undefined && section.byteLength === 8 ? new DataView(section).getBigUint64(0, true) : null;
}

/** Makes the Durable Object class of a server build's rooms. */
export function roomClass(module: WebAssembly.Module) {
  return class WrelaRoom extends DurableObject<Env> {
    readonly room: Room;

    constructor(ctx: DurableObjectState, env: Env) {
      super(ctx, env);
      const server = env.WRELA_SERVER.getByName(SERVER_OBJECT);
      this.room = new Room(module, ctx.storage, server, ctx.getWebSockets() as unknown as Socket[], {
        log: (line) => console.error(line),
        ...(env.WRELA_PRINT === "1" ? { print: (line: string) => console.log(line) } : {}),
      });
    }

    /** A peer's upgrade, which `rooms` checked. */
    async fetch(_request: Request): Promise<Response> {
      const pair = new WebSocketPair();
      const [client, server] = [pair[0], pair[1]];
      this.ctx.acceptWebSocket(server);
      this.room.connect(server as unknown as Socket);
      return new Response(null, { status: 101, webSocket: client });
    }

    async webSocketMessage(ws: WebSocket, message: ArrayBuffer | string): Promise<void> {
      this.room.message(ws as unknown as Socket, message);
    }

    async webSocketClose(ws: WebSocket): Promise<void> {
      this.room.closed(ws as unknown as Socket);
    }

    async webSocketError(ws: WebSocket): Promise<void> {
      this.room.closed(ws as unknown as Socket);
    }

    async alarm(): Promise<void> {
      await this.room.alarm();
    }
  };
}

/** A peer's message is at most this many bytes (`wrela_serve::MAX_UP`). */
export { MAX_UP };
