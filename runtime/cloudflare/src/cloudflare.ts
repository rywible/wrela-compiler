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
import { HTTP_METHODS, MAX_HTTP_REQUEST, OWN_HEADER_PREFIX, SAFE_METHODS, SERVER_SEES } from "../../browser/src/abi.gen.ts";
import { bare, decodeResponse, encodeRequest, type Header, Server } from "./host.ts";

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
  WRELA_SERVER: { getByName(name: string): { request(bytes: Uint8Array): Promise<Uint8Array> } };
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

const seen = (name: string) => name.startsWith(OWN_HEADER_PREFIX) || SERVER_SEES.includes(name);

/** A response of the host's own, with no body. */
function refusal(status: number, headers: Record<string, string> = {}): Response {
  return new Response(null, { status, headers: { "cache-control": "no-store", ...headers } });
}

/** The body, at most `max` bytes: null if it's longer. */
async function bodyWithin(request: Request, max: number): Promise<Uint8Array | null> {
  const declared = request.headers.get("content-length");
  if (declared !== null && Number(declared) > max) return null;
  if (request.body === null) return new Uint8Array(0);
  const parts: Uint8Array[] = [];
  let length = 0;
  const reader = request.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.length;
    if (length > max) {
      await reader.cancel();
      return null;
    }
    parts.push(value);
  }
  const out = new Uint8Array(length);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
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

/** The rate limits from `WRELA_LIMITS`; a malformed list is a configuration error. */
function limits(env: Env): Limit[] {
  const list: unknown = JSON.parse(env.WRELA_LIMITS ?? "[]");
  if (!Array.isArray(list) || !list.every((l) => typeof l?.binding === "string" && typeof l?.prefix === "string")) {
    throw new Error("WRELA_LIMITS isn't a list of { binding, prefix, methods?, period? }");
  }
  return list as Limit[];
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
  const limited = await pastLimit(env, method, url.pathname, request.headers.get("cf-connecting-ip") ?? "unknown");
  if (limited !== null) return refusal(429, { "retry-after": String(limited.period ?? 60) });
  const cache = options.cache ?? (globalThis as { caches?: { default?: ResponseCache } }).caches?.default;
  // Keyed by the URL alone: a public answer is the same whoever asks.
  const key = new Request(url.href, { method: "GET" });
  if (method === "GET" && cache !== undefined) {
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
  for (const [name, value] of request.headers) if (seen(name)) headers.push([name, value]);
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
  const empty = method === "HEAD" || s.status === 204 || s.status === 304;
  const response = new Response(empty ? null : (s.body.slice() as Uint8Array<ArrayBuffer>), { status: s.status, headers: out });
  if (method === "GET" && cache !== undefined && shareable(response)) {
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
      return this.server.request(new Uint8Array(bytes));
    }

    async alarm(): Promise<void> {
      await this.server.alarm();
    }
  };
}
