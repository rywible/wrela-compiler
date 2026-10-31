// The Cloudflare host's glue (language.md §19.5): the Durable Object that runs a server build's
// HTTP instance beside its store (host.ts), and `api`, which the site's Worker calls first, so
// the site's own origin serves its API.
//
// `api` refuses what it can before the object is billed: a path outside the prefix (null: not
// the API's), a method the host doesn't take (405), a request that changes something from another
// origin (403: its Origin must be one of `WRELA_ORIGINS`, and `Sec-Fetch-Site`, when sent,
// `same-origin`), and a body past 1 MiB (413). It passes the object only the headers a server
// sees (runtime/abi `http`), so no address reaches the object or the program, and logs nothing.

import { DurableObject } from "cloudflare:workers";
import { HTTP_METHODS, MAX_HTTP_REQUEST, OWN_HEADER_PREFIX, SAFE_METHODS, SERVER_SEES } from "../../browser/src/abi.gen.ts";
import { bare, decodeResponse, encodeRequest, type Header, Server } from "./host.ts";

/** What the Worker's configuration gives the host (server/cloudflare.json). */
export interface Env {
  WRELA_SERVER: { getByName(name: string): { request(bytes: Uint8Array): Promise<Uint8Array> } };
  /** The page origins whose requests may change something, comma-separated. */
  WRELA_ORIGINS?: string;
  /** "1": the program's printed lines go to the Worker's log. */
  WRELA_PRINT?: string;
}

export interface ApiOptions {
  /** The path the API is under (default `/api/`). */
  prefix?: string;
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

/** The API's response to `request`, or null if it isn't under the API's prefix. */
export async function api(request: Request, env: Env, options: ApiOptions = {}): Promise<Response | null> {
  const url = new URL(request.url);
  const prefix = options.prefix ?? "/api/";
  if (!url.pathname.startsWith(prefix)) return null;
  const method = request.method.toUpperCase();
  if (!HTTP_METHODS.includes(method)) return refusal(405, { allow: HTTP_METHODS.join(", ") });
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
  return new Response(empty ? null : (s.body.slice() as Uint8Array<ArrayBuffer>), { status: s.status, headers: out });
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
