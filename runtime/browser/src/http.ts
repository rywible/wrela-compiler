// HTTP's bytes (runtime/abi `http`, language.md §19.5), which the browser host and the Cloudflare
// host (runtime/cloudflare) share, and the browser's `http/fetch` service: a request to the
// page's own origin, cookies sent, with only the headers a client may set, answered with the
// response and only the headers a client may see.

import { CLIENT_SEES, CLIENT_SETS, HTTP_METHODS, HTTP_REQUEST_MAGIC, HTTP_RESPONSE_MAGIC, OWN_HEADER_PREFIX, SERVER_SEES, SERVER_SETS } from "./abi.gen.ts";

export type Header = [name: string, value: string];

export interface HttpRequest {
  /** When the host took it, in Unix milliseconds (0 from a client). */
  time: bigint;
  method: string;
  path: string;
  query: string;
  headers: Header[];
  body: Uint8Array;
}

export interface HttpResponse {
  status: number;
  headers: Header[];
  body: Uint8Array;
}

const UTF8 = new TextDecoder("utf-8", { fatal: true });
const ENCODER = new TextEncoder();

/** HTTP's bytes, and a host service's, read from the front (little-endian). */
export class In {
  at = 0;
  readonly view: DataView;
  constructor(readonly bytes: Uint8Array) {
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  left(): number {
    return this.bytes.length - this.at;
  }
  take(n: number): Uint8Array {
    if (this.left() < n) throw new Error("the bytes end early");
    this.at += n;
    return this.bytes.subarray(this.at - n, this.at);
  }
  byte(): number {
    return this.take(1)[0]!;
  }
  word(): number {
    this.take(4);
    return this.view.getUint32(this.at - 4, true);
  }
  u64(): bigint {
    this.take(8);
    return this.view.getBigUint64(this.at - 8, true);
  }
  chunk(): Uint8Array {
    return this.take(this.word());
  }
  text(): string {
    return UTF8.decode(this.chunk());
  }
  headers(): Header[] {
    const n = this.word();
    // Each header is at least its two lengths.
    if (n > this.left() / 8) throw new Error("more headers than bytes");
    const out: Header[] = [];
    for (let i = 0; i < n; i++) out.push([this.text(), this.text()]);
    return out;
  }
  end(what: string): void {
    if (this.left() !== 0) throw new Error(`bytes past the ${what}'s end`);
  }
}

/** HTTP's bytes, and a host service's, written (little-endian). */
export class Out {
  parts: Uint8Array[] = [];
  length = 0;
  push(b: Uint8Array): this {
    this.parts.push(b);
    this.length += b.length;
    return this;
  }
  word(x: number): this {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, x, true);
    return this.push(b);
  }
  u64(x: bigint): this {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigUint64(0, x, true);
    return this.push(b);
  }
  chunk(b: Uint8Array): this {
    return this.word(b.length).push(b);
  }
  text(s: string): this {
    return this.chunk(ENCODER.encode(s));
  }
  headers(headers: Header[]): this {
    this.word(headers.length);
    for (const [n, v] of headers) this.text(n).text(v);
    return this;
  }
  bytes(): Uint8Array<ArrayBuffer> {
    const out = new Uint8Array(this.length);
    let at = 0;
    for (const p of this.parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }
}

export function encodeRequest(q: HttpRequest): Uint8Array<ArrayBuffer> {
  return new Out().word(HTTP_REQUEST_MAGIC).u64(q.time).text(q.method).text(q.path).text(q.query).headers(q.headers).chunk(q.body).bytes();
}

export function decodeRequest(bytes: Uint8Array): HttpRequest {
  const r = new In(bytes);
  if (r.word() !== HTTP_REQUEST_MAGIC) throw new Error("not a request");
  const q = { time: r.u64(), method: r.text(), path: r.text(), query: r.text(), headers: r.headers(), body: r.chunk() };
  r.end("request");
  return q;
}

export function encodeResponse(s: HttpResponse): Uint8Array<ArrayBuffer> {
  return new Out().word(HTTP_RESPONSE_MAGIC).word(s.status).headers(s.headers).chunk(s.body).bytes();
}

export function decodeResponse(bytes: Uint8Array): HttpResponse {
  const r = new In(bytes);
  if (r.word() !== HTTP_RESPONSE_MAGIC) throw new Error("not a response");
  const status = r.word();
  if (status < 100 || status > 599) throw new Error(`status ${status} isn't 100 to 599`);
  const s = { status, headers: r.headers(), body: r.chunk() };
  r.end("response");
  return s;
}

const listed = (list: readonly string[], name: string) => name.startsWith(OWN_HEADER_PREFIX) || list.includes(name);
/** Whether a server sees request header `name` (lower case), sets response header `name`, and a
 * client sets or sees it (runtime/abi `http`'s lists). */
export const seenByServer = (name: string) => listed(SERVER_SEES, name);
export const setByServer = (name: string) => listed(SERVER_SETS, name);
export const setByClient = (name: string) => listed(CLIENT_SETS, name);
export const seenByClient = (name: string) => listed(CLIENT_SEES, name);

/** `http/fetch`: a request's bytes (`std::http::fetch`) → the response's. The path is absolute on
 * the page's origin, so nothing leaves it; a redirect isn't followed, and nothing is cached. */
export async function httpFetch(body: Uint8Array, origin: string, send: typeof fetch = fetch): Promise<Uint8Array<ArrayBuffer>> {
  const q = decodeRequest(body);
  if (!HTTP_METHODS.includes(q.method)) throw new Error(`\`${q.method}\` isn't a method a host sends`);
  if (!q.path.startsWith("/") || q.path.startsWith("//")) throw new Error("a fetch's path is absolute on the page's origin");
  const headers = new Headers();
  for (const [n, v] of q.headers) {
    if (!setByClient(n)) throw new Error(`a program can't set header \`${n}\``);
    headers.append(n, v);
  }
  const url = new URL(q.path + (q.query === "" ? "" : `?${q.query}`), origin);
  if (url.origin !== new URL(origin).origin) throw new Error("a fetch stays on the page's origin");
  const empty = q.method === "GET" || q.method === "HEAD";
  const r = await send(url, {
    method: q.method,
    headers,
    body: empty ? null : (q.body.slice() as Uint8Array<ArrayBuffer>),
    credentials: "same-origin",
    redirect: "error",
    cache: "no-store",
  });
  const seen: Header[] = [];
  for (const [n, v] of r.headers) if (seenByClient(n)) seen.push([n, v]);
  return encodeResponse({ status: r.status, headers: seen, body: new Uint8Array(await r.arrayBuffer()) });
}
