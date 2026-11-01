// Application-host mechanisms. Services transport opaque bytes; the Wrela application
// owns its catalog, UI, session protocol and save policy.
import { httpFetch } from "./http.ts";
import { asksForTest } from "./testmode.ts";
import { glyph, type Paper } from "./glyph.ts";
import type { Listener } from "./listen.ts";
import type { Bytes } from "./stream.ts";
import type { Semantics } from "./semantics.ts";
import { Reader, UTF8, Writer } from "./wire.ts";

export const MESSAGE_LIMIT = 1024 * 1024 - 512;
const ENCODER = new TextEncoder();

/** The longest address or link a program may give (`nav/*`). */
const ADDRESS_LIMIT = 2048;

/** A path that still resolves on this origin when History or Location reads it again. */
function addressPath(url: Pick<URL, "pathname" | "search" | "hash">): string {
  const path = (url.pathname.startsWith("//") ? "/." : "") + url.pathname;
  return path + url.search + url.hash;
}

/** `text` as an address on the page's own origin (`#std/io`, `/v0.1/?q=x`), resolved against
 * the page's address `here`: its path, query and fragment. Another origin, another scheme, or
 * text with control characters fails. */
export function sameOriginAddress(text: string, here: string): string {
  if (text.length === 0 || text.length > ADDRESS_LIMIT || /[\u0000-\u001f\u007f\\]/.test(text)) throw new Error("invalid address");
  const base = new URL(here);
  const url = new URL(text, base);
  if (url.origin !== base.origin || url.protocol !== base.protocol) throw new Error("an address stays on the page's own origin");
  return addressPath(url);
}

/** `text` as a link to follow: an absolute http or https URL without credentials. */
export function linkUrl(text: string): string {
  if (text.length === 0 || text.length > ADDRESS_LIMIT || /[\u0000-\u001f\u007f]/.test(text)) throw new Error("invalid link");
  const url = new URL(text);
  if ((url.protocol !== "https:" && url.protocol !== "http:") || url.username || url.password) throw new Error("a link is an http or https URL");
  return url.href;
}
/** The passkeys' algorithms, by their COSE numbers, in the order a site prefers them: ES256,
 * Ed25519 and RS256 (std::crypto checks each). */
export const PASSKEY_ALGORITHMS = [-7, -8, -257];

/** What a page's `navigator.credentials` gives for passkeys. */
export type Credentials = Pick<CredentialsContainer, "create" | "get">;

/** Credential IDs, as a count then each. */
function credentialIds(r: Reader): Uint8Array<ArrayBuffer>[] {
  const n = r.u32();
  if (n > 64) throw new Error("too many credentials");
  return Array.from({ length: n }, () => r.chunk());
}

/** `passkey/create` (compiler/std/host.wrela's `create_passkey`): the relying party's ID and name,
 * the user's handle, name and shown name, the challenge, the credentials to exclude, and a
 * timeout in milliseconds → the new credential's ID, the client's data and the attestation
 * object. Every passkey is discoverable and verifies its person, with no attestation. */
export async function createPasskey(body: Bytes, credentials: Credentials): Promise<Bytes> {
  const r = new Reader(body);
  const rp = { id: r.str(), name: r.str() };
  const user = { id: r.chunk(), name: r.str(), displayName: r.str() };
  const challenge = r.chunk();
  const exclude = credentialIds(r);
  const timeout = r.u32();
  r.end();
  const credential = await credentials.create({
    publicKey: {
      rp, user, challenge, timeout,
      pubKeyCredParams: PASSKEY_ALGORITHMS.map(alg => ({ type: "public-key" as const, alg })),
      excludeCredentials: exclude.map(id => ({ type: "public-key" as const, id })),
      authenticatorSelection: { residentKey: "required", requireResidentKey: true, userVerification: "required" },
      attestation: "none",
    },
  }) as PublicKeyCredential | null;
  if (!credential || credential.type !== "public-key") throw new Error("no passkey was made");
  const response = credential.response as AuthenticatorAttestationResponse;
  return new Writer()
    .chunk(new Uint8Array(credential.rawId))
    .chunk(new Uint8Array(response.clientDataJSON))
    .chunk(new Uint8Array(response.attestationObject))
    .bytes();
}

/** `passkey/get` (compiler/std/host.wrela's `use_passkey`): the relying party's ID, the challenge,
 * the credentials allowed (none: any the person chooses), and a timeout in milliseconds → the
 * credential's ID, the client's data, the authenticator's data, the signature and the user's
 * handle (empty if the authenticator gave none). The person's verification is required. */
export async function usePasskey(body: Bytes, credentials: Credentials): Promise<Bytes> {
  const r = new Reader(body);
  const rpId = r.str();
  const challenge = r.chunk();
  const allow = credentialIds(r);
  const timeout = r.u32();
  r.end();
  const credential = await credentials.get({
    publicKey: {
      rpId, challenge, timeout,
      allowCredentials: allow.map(id => ({ type: "public-key" as const, id })),
      userVerification: "required",
    },
  }) as PublicKeyCredential | null;
  if (!credential || credential.type !== "public-key") throw new Error("no passkey was used");
  const response = credential.response as AuthenticatorAssertionResponse;
  return new Writer()
    .chunk(new Uint8Array(credential.rawId))
    .chunk(new Uint8Array(response.clientDataJSON))
    .chunk(new Uint8Array(response.authenticatorData))
    .chunk(new Uint8Array(response.signature))
    .chunk(response.userHandle ? new Uint8Array(response.userHandle) : new Uint8Array())
    .bytes();
}

const CURSORS = new Set(["default", "pointer", "text", "grab", "grabbing", "crosshair", "move", "ew-resize", "nwse-resize", "nesw-resize"]);
/** What `environment`'s capabilities word says this host has (runtime/browser): its bits. */
export const PASTE_EVENTS = 1;
export const TEXT_INPUT = 2;
export const SEMANTICS = 4;
export const SYSTEM_GLYPHS = 8;
export const wordBytes = (n: number): Bytes => {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, n, true);
  return b;
};
export const wordAt = (b: Bytes): number => {
  if (b.length < 4) throw new Error("missing capability handle");
  return new DataView(b.buffer, b.byteOffset, b.byteLength).getUint32(0, true);
};
export function exportName(body: Bytes): { name: string; offset: number } {
  const n = wordAt(body);
  if (n > 240 || body.length < 4 + n) throw new Error("invalid export filename");
  const name = UTF8.decode(body.subarray(4, 4 + n));
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(name)) throw new Error("invalid export filename");
  return { name, offset: 4 + n };
}
// One of the page's icons, `rel size path`: a square PNG the build shipped, under `files/`.
const ICON = /^(icon|apple-touch-icon) ([1-9][0-9]{0,3}) (files\/[A-Za-z0-9_-][A-Za-z0-9_.-]*\/[A-Za-z0-9_-][A-Za-z0-9_.-]*\.png)$/;
export function iconLink(body: Bytes, base: string): { rel: string; sizes: string; href: string } {
  const match = ICON.exec(UTF8.decode(body));
  if (!match || Number(match[2]) > 1024) throw new Error("invalid icon");
  const [, rel, size, path] = match;
  return { rel: rel!, sizes: `${size}x${size}`, href: new URL(path!, base).href };
}
const checked = (body: unknown): Bytes => {
  if (!(body instanceof Uint8Array) || body.length > MESSAGE_LIMIT) throw new Error("invalid capability message");
  return new Uint8Array(body);
};

/** The environment a program asks for (compiler/std/host.wrela): whether the system asks for reduced
 * motion, the seconds since 1970, the device pixel ratio × 1024, whether this page load is a
 * reload of the tab (its navigation's type), rather than a visit, what this host has, and whether
 * the system asks for a light colour scheme. */
export function environment(reducedMotion: boolean, nowMs: number, pixelRatio: number, navigations: PerformanceEntryList, capabilities: number, light = false): Bytes {
  const b = new Uint8Array(15);
  b[0] = Number(reducedMotion);
  b.set(wordBytes(Math.floor(nowMs / 1000)), 1);
  b.set(wordBytes(Math.round(pixelRatio * 1024)), 5);
  b[9] = Number(navigations.some((n) => (n as PerformanceNavigationTiming).type === "reload"));
  b.set(wordBytes(capabilities), 10);
  b[14] = Number(light);
  return b;
}

/** How many prefetches run at once. */
const PREFETCHES = 3;

/** `prefetch` (compiler/std/host.wrela): fetches each URL of `list`, one a line, into the browser's
 * cache at low priority, a few at a time, until `signal` aborts. A URL that fails is skipped:
 * the program fetches it again when it needs it. Answers once all are done or it is aborted.
 * Each is a readable request with credentials, as the page that later loads it makes: an opaque
 * response (`no-cors`) of a shader or a manifest is blocked, and the cache may keep requests
 * with and without credentials apart. */
export async function prefetch(list: string, base: string, signal: AbortSignal, fetchFn: typeof fetch): Promise<Bytes> {
  const urls = list.split("\n").map(line => line.trim()).filter(line => line !== "").map(line => {
    const url = programUrl(line, base); url.hash = ""; return url;
  });
  let next = 0;
  const run = async () => {
    while (next < urls.length && !signal.aborted) {
      const url = urls[next++]!;
      try { await (await fetchFn(url, { mode: "cors", credentials: "include", priority: "low", signal })).arrayBuffer(); } catch { /* skipped */ }
    }
  };
  await Promise.all(Array.from({ length: PREFETCHES }, run));
  return new Uint8Array();
}

/** Decoded pixels: RGBA8, rows top to bottom. */
export interface Decoded { width: number; height: number; pixels: Uint8ClampedArray }

/** The most pixels a decoded image may have. */
const IMAGE_PIXELS = 4096 * 4096;

/** `image/decode` (compiler/std/host.wrela): the width and height the program expects (words), then
 * a compressed image, which `decode` turns into its pixels. Another size fails. */
export async function decodeImage(body: Bytes, decode: (image: Bytes) => Promise<Decoded>): Promise<Bytes> {
  const width = wordAt(body), height = wordAt(body.subarray(4));
  if (width === 0 || height === 0 || width * height > IMAGE_PIXELS) throw new Error("invalid image size");
  const image = await decode(body.slice(8));
  if (image.width !== width || image.height !== height) throw new Error(`the image is ${image.width}x${image.height}, not ${width}x${height}`);
  return new Uint8Array(image.pixels.buffer, image.pixels.byteOffset, image.pixels.byteLength) as Bytes;
}

/** The browser's own decoder, without colour conversion, so the pixels are the file's. */
async function browserDecode(image: Bytes): Promise<Decoded> {
  const bitmap = await createImageBitmap(new Blob([image]), { colorSpaceConversion: "none", premultiplyAlpha: "none" });
  try {
    const { width, height } = bitmap;
    if (width * height > IMAGE_PIXELS) throw new Error("the image is too large");
    const canvas = new OffscreenCanvas(width, height);
    const g = canvas.getContext("2d", { willReadFrequently: true });
    if (!g) throw new Error("no canvas to decode on");
    g.drawImage(bitmap, 0, 0);
    return { width, height, pixels: g.getImageData(0, 0, width, height).data };
  } finally { bitmap.close(); }
}

/** A bounded request/reply channel. Closing it rejects every outstanding request. */
export class PortRpc {
  #next = 1;
  #closed = false;
  #calls = new Map<number, { resolve: (b: Bytes) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  constructor(readonly port: MessagePort, readonly receive: (id: number, body: Bytes) => void, readonly event: (kind: string) => void = () => {}) {
    port.onmessage = ({ data }: MessageEvent<unknown>) => {
      if (!data || typeof data !== "object" || !("type" in data)) return;
      const m = data as { type: unknown; id?: unknown; body?: unknown; ok?: unknown };
      if (m.type === "frame" || m.type === "ready" || m.type === "return" || m.type === "failed" || m.type === "leave" || m.type === "underlay" || m.type === "underlay-end") { this.event(m.type); return; }
      if (!Number.isSafeInteger(m.id) || Number(m.id) <= 0) return;
      if (m.type === "call") {
        try { this.receive(Number(m.id), checked(m.body).slice()); } catch { this.reply(Number(m.id), new Uint8Array(), false); }
      } else if (m.type === "reply") {
        const call = this.#calls.get(Number(m.id));
        if (!call) return;
        this.#calls.delete(Number(m.id));
        clearTimeout(call.timer);
        try { const b = checked(m.body); if (m.ok === true) call.resolve(b); else call.reject(new Error("the embedded program rejected the request")); }
        catch (e) { call.reject(e instanceof Error ? e : new Error(String(e))); }
      }
    };
    port.onmessageerror = () => this.close();
    port.start();
  }
  call(body: Bytes): Promise<Bytes> {
    if (this.#closed) return Promise.reject(new Error("the program channel is closed"));
    checked(body);
    if (this.#calls.size >= 32) return Promise.reject(new Error("too many program requests"));
    const id = this.#next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.#calls.delete(id); reject(new Error("the program request timed out")); }, 120_000);
      this.#calls.set(id, { resolve, reject, timer });
      this.port.postMessage({ type: "call", id, body: body.slice() });
    });
  }
  reply(id: number, body: Bytes, ok = true): void {
    if (this.#closed) throw new Error("the program channel is closed");
    checked(body);
    this.port.postMessage({ type: "reply", id, body: body.slice(), ok });
  }
  signal(type: "frame" | "ready" | "return" | "failed" | "leave" | "underlay" | "underlay-end"): void { if (!this.#closed) this.port.postMessage({ type }); }
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.port.close();
    for (const call of this.#calls.values()) { clearTimeout(call.timer); call.reject(new Error("the program channel closed")); }
    this.#calls.clear();
  }
}

// A shown program's name in the page's address, `#name`: a lowercase key.
const SHOWN_NAME = /^[a-z0-9-]{1,64}$/;
export function shownName(body: Bytes): string | null {
  if (body.length <= 4) return null;
  const name = UTF8.decode(body.subarray(4));
  if (!SHOWN_NAME.test(name)) throw new Error("invalid program name");
  return name;
}

/** The address of a program to open (`surface/open`) or fetch ahead (`prefetch`), resolved against
 * the page's `base`. A fragment is the opened program's to read (`host::address`): the browser
 * fetches and caches its page without it, and its build's files resolve without it. A query
 * would reach the server and key the cache, so it's refused, as credentials are, and so is a
 * fragment that asks for test mode, which only a page's own address may. */
export function programUrl(text: string, base: string): URL {
  const url = new URL(text, base);
  // An empty query (`/?`) is still one: `search` is empty, but the request isn't the same.
  if (url.username || url.password || url.href.split("#")[0]!.includes("?")) throw new Error("program URLs cannot contain credentials or queries");
  if (asksForTest(url.hash)) throw new Error("a program URL cannot ask for test mode");
  if (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))) throw new Error("programs require HTTPS or loopback HTTP");
  return url;
}

interface Surface {
  frame: HTMLIFrameElement;
  rpc: PortRpc;
  firstFrame: Promise<void>;
  closed: Promise<void>;
  close: () => void;
}

/** Main-thread capabilities, exposed through the same async HostRequest as native hosts. */
export class BrowserPlatform {
  #next = 1;
  #surfaces = new Map<number, Surface>();
  #parent: PortRpc | null = null;
  #inbox: Bytes[] = [];
  #receiving: { resolve: (b: Bytes) => void; reject: (e: Error) => void } | null = null;
  #shown: number | null = null;
  // The surface shown behind this program's canvas (`surface/underlay`), and whether this program
  // is itself one, underlaid by the program that opened it.
  #underlaid: number | null = null;
  #under = false;
  /** Told when the program that opened this one underlays it (true) or no longer does: the page
   * mutes its voice while it's underlaid. */
  onUnderlaid: (on: boolean) => void = () => {};
  #input: Listener | null = null;
  #overlay: Semantics | null = null;
  /** Where glyphs are drawn: the page's OffscreenCanvas, unless a test gives its own. */
  paper: (w: number, h: number) => Paper = (w, h) => new OffscreenCanvas(w, h);
  // The parent asked this program to return (its page's Back): requests waiting to hear it,
  // or that it was asked before any waited.
  #leaving = new Set<() => void>();
  #asked = false;
  // Prefetches in flight, which an opening program cancels.
  #prefetching = new AbortController();
  // Requests waiting to hear that Back or Forward changed the page's address (`nav/changed`), or
  // that it changed before any waited.
  #navigating = new Set<() => void>();
  #navigated = false;
  constructor(readonly canvas: HTMLCanvasElement, readonly visible: (v: boolean) => void, readonly seeThrough: (on: boolean) => void = () => {}) {
    canvas.style.position = "relative"; canvas.style.zIndex = "1"; canvas.style.outline = "none"; canvas.tabIndex = 0;
    // A touch's moves are the program's: the browser doesn't pan or zoom the page under it.
    canvas.style.touchAction = "none";
    window.addEventListener("message", (e: MessageEvent<unknown>) => {
      if (window.parent === window || e.source !== window.parent || this.#parent || !document.referrer) return;
      if (e.origin !== new URL(document.referrer).origin) return;
      if (!e.data || typeof e.data !== "object" || !("type" in e.data) || e.data.type !== "wrela-connect" || e.ports.length !== 1) return;
      this.#parent = new PortRpc(e.ports[0]!, (id, body) => {
        const b = new Uint8Array(body.length + 4); b.set(wordBytes(id)); b.set(body, 4);
        if (this.#receiving) { const give = this.#receiving; this.#receiving = null; give.resolve(b); }
        else if (this.#inbox.length < 32) this.#inbox.push(b);
        else this.#parent?.reply(id, new Uint8Array(), false);
      }, kind => {
        if (kind === "underlay" || kind === "underlay-end") {
          this.#under = kind === "underlay"; this.onUnderlaid(this.#under); return;
        }
        if (kind !== "leave") return;
        if (this.#leaving.size === 0) this.#asked = true;
        for (const resolve of this.#leaving) resolve();
        this.#leaving.clear();
      });
      e.ports[0]!.postMessage({ type: "connected" });
    });
    window.addEventListener("pagehide", () => this.dispose());
    // A top-level page names the program it shows in its address, so the browser's Back asks
    // that program to return, as its own return shortcut does. Otherwise Back and Forward move
    // through the addresses the program gave (`nav/push`), and it hears of each (`nav/changed`).
    // A shown program's name goes after the page's own path: a relative address would resolve
    // against the page's <base>, a release's directory, which a later deploy removes.
    if (window.parent === window && typeof history !== "undefined") {
      window.addEventListener("popstate", () => {
        if (this.#shown !== null) { this.#back(); return; }
        if (this.#navigating.size === 0) this.#navigated = true;
        for (const resolve of this.#navigating) resolve();
        this.#navigating.clear();
      });
    }
  }
  #back(): void {
    const id = this.#shown;
    if (id === null || history.state?.wrela === id) return;
    const s = this.#surfaces.get(id); if (!s) return;
    s.rpc.signal("leave");
    // A program that does not return in time is closed.
    const timer = setTimeout(() => s.close(), 10_000);
    void s.closed.then(() => clearTimeout(timer));
  }
  get active(): boolean { return this.#shown === null; }
  /** This program is underlaid by the one that opened it: it hears no input. */
  get underlaid(): boolean { return this.#under; }
  /** Shows surface `id` behind this program's canvas, or none: the canvas is see-through where
   * the program draws nothing, the other surfaces stay hidden, and the one behind is inert (no
   * pointer, keys or focus) and told it's underlaid, so it plays no sound and reads no pads. */
  #underlay(id: number | null): void {
    const before = this.#underlaid;
    if (before === id) return;
    const was = before === null ? undefined : this.#surfaces.get(before);
    if (was) { was.frame.inert = false; was.rpc.signal("underlay-end"); }
    this.#underlaid = id;
    for (const [k, s] of this.#surfaces) if (k !== this.#shown) s.frame.style.visibility = id === null || k === id ? "" : "hidden";
    const s = id === null ? undefined : this.#surfaces.get(id);
    if (s) { s.frame.inert = true; s.rpc.signal("underlay"); }
    if ((before === null) !== (id === null)) this.seeThrough(id !== null);
  }
  /** The input services (listen.ts) and the semantic overlay (semantics.ts), once there. */
  attach(input: Listener, overlay: Semantics | null): void { this.#input = input; this.#overlay = overlay; }
  #capabilities(): number {
    return (this.#input ? PASTE_EVENTS | TEXT_INPUT : 0) | (this.#overlay ? SEMANTICS : 0) | (typeof OffscreenCanvas === "function" ? SYSTEM_GLYPHS : 0);
  }
  presented(): void { this.#parent?.signal("frame"); }
  failed(): void { this.#parent?.signal("failed"); }
  dispose(): void {
    for (const s of [...this.#surfaces.values()]) s.close();
    this.#parent?.close(); this.#parent = null;
    this.#receiving?.reject(new Error("the parent channel closed")); this.#receiving = null; this.#inbox = [];
  }
  async #open(text: string): Promise<Bytes> {
    if (this.#surfaces.size >= 4) throw new Error("too many embedded programs");
    // The program opening takes the network: what was fetched ahead of time stops.
    this.#prefetching.abort(); this.#prefetching = new AbortController();
    const url = programUrl(text, document.baseURI);
    const frame = document.createElement("iframe");
    frame.src = url.href;
    frame.title = "Wrela program";
    frame.allow = "cross-origin-isolated; autoplay; gamepad; fullscreen";
    frame.referrerPolicy = "origin";
    frame.style.cssText = "position:fixed;inset:0;width:100%;height:100%;border:0;z-index:0;pointer-events:none";
    // Behind the canvas, under the surface that's underlaid, if one is: hidden from it.
    if (this.#underlaid !== null) frame.style.visibility = "hidden";
    const connection = new MessageChannel();
    let resolveFrame!: () => void, resolveClosed!: () => void;
    const firstFrame = new Promise<void>(r => { resolveFrame = r; });
    const closed = new Promise<void>(r => { resolveClosed = r; });
    const id = this.#next++;
    const rpc = new PortRpc(connection.port1, (request) => rpc.reply(request, new Uint8Array(), false), kind => {
      if (kind === "ready") resolveFrame();
      if (kind === "return" || kind === "failed") surface.close();
    });
    let removed = false;
    const surface: Surface = { frame, rpc, firstFrame, closed, close: () => {
      if (removed) return; removed = true;
      frame.onload = null; frame.onerror = null;
      rpc.close(); frame.remove(); this.#surfaces.delete(id); resolveClosed();
      if (this.#underlaid === id) this.#underlay(null);
      if (this.#shown === id) {
        this.#shown = null; this.canvas.style.visibility = "visible"; this.visible(true); this.canvas.focus();
        if (window.parent === window && typeof history !== "undefined" && history.state?.wrela === id) history.back();
      }
    } };
    this.#surfaces.set(id, surface);
    let timer: ReturnType<typeof setTimeout> | undefined;
    let connected: ((e: MessageEvent<unknown>) => void) | undefined;
    try {
      await Promise.race([closed.then(() => { throw new Error("the program closed before connecting"); }), new Promise<void>((resolve, reject) => {
        timer = setTimeout(() => reject(new Error("the embedded program did not connect")), 30_000);
        connected = (e: MessageEvent<unknown>) => {
          if (e.data && typeof e.data === "object" && "type" in e.data && e.data.type === "connected") { clearTimeout(timer); resolve(); }
        };
        connection.port1.addEventListener("message", connected);
        frame.onerror = () => { clearTimeout(timer); reject(new Error("the embedded program could not load")); };
        frame.onload = () => { try { frame.contentWindow!.postMessage({ type: "wrela-connect" }, url.origin, [connection.port2]); } catch (e) { reject(e); } };
        document.body.append(frame);
      })]);
      return wordBytes(id);
    } catch (e) { connection.port2.close(); surface.close(); throw e; }
    finally { clearTimeout(timer); if (connected) connection.port1.removeEventListener("message", connected); frame.onload = removed ? null : () => surface.close(); frame.onerror = null; }
  }
  async request(service: string, body: Bytes): Promise<Bytes> {
    checked(body);
    if (service === "environment") {
      return environment(matchMedia("(prefers-reduced-motion: reduce)").matches, Date.now(), window.devicePixelRatio || 1, performance.getEntriesByType("navigation"), this.#capabilities(), matchMedia("(prefers-color-scheme: light)").matches);
    }
    if (service === "image/decode") return decodeImage(body, browserDecode);
    if (service === "http/fetch") return httpFetch(body, location.origin);
    if (service === "http/fetch-public") return httpFetch(body, location.origin, fetch, true);
    if (service === "cursor") {
      // The pointer's shape over the program's canvas: a named CSS cursor, from a short list.
      const name = UTF8.decode(body);
      if (!CURSORS.has(name)) throw new Error(`unsupported cursor: ${name.slice(0, 32)}`);
      this.canvas.style.cursor = name; return new Uint8Array();
    }
    if (service === "text/input") {
      // On (with the caret's rectangle, device pixels of the canvas) or off.
      if (!this.#input) throw new Error("this host has no text input yet");
      const r = new Reader(body); const on = r.u8() !== 0;
      this.#input.textInput(on, { x: r.f32(), y: r.f32(), w: r.f32(), h: r.f32() }); return new Uint8Array();
    }
    if (service === "clipboard/offer") {
      if (!this.#input) throw new Error("this host has no clipboard yet");
      const r = new Reader(body); this.#input.offer(r.str(), r.str()); return new Uint8Array();
    }
    if (service === "clipboard/write") {
      // Text on the system clipboard now, as a person's press asked (a copy button): the browser
      // refuses it without that press, and the program hears why.
      await navigator.clipboard.writeText(UTF8.decode(body)); return new Uint8Array();
    }
    if (service === "clipboard/pasted") {
      const pasted = this.#input?.pasted(new Reader(body).u32());
      if (!pasted) throw new Error("that paste isn't kept");
      return new Writer().str(pasted.text).str(pasted.html).bytes();
    }
    if (service.startsWith("semantics/") || service === "field/state" || service === "form/submit" || service === "a11y/announce") {
      if (!this.#overlay) throw new Error("this host has no semantic overlay");
      const o = this.#overlay;
      if (service === "semantics/tree") { o.tree(body); return new Uint8Array(); }
      if (service === "semantics/geometry") { o.geometry(body); return new Uint8Array(); }
      if (service === "field/state") return o.fieldState(new Reader(body).u32());
      if (service === "form/submit") { o.submit(new Reader(body).u32()); return new Uint8Array(); }
      if (service === "a11y/announce") { o.announce(UTF8.decode(body)); return new Uint8Array(); }
    }
    if (service === "font/glyph") return glyph(body, this.paper);
    if (service === "icon") {
      // The program's icon in the page's tab, bookmarks and home screen: one link per kind and size.
      const { rel, sizes, href } = iconLink(body, document.baseURI);
      let link = document.head.querySelector<HTMLLinkElement>(`link[rel="${rel}"][sizes="${sizes}"]`);
      if (!link) {
        link = document.createElement("link");
        link.rel = rel; link.type = "image/png"; link.setAttribute("sizes", sizes);
        document.head.append(link);
      }
      link.href = href; return new Uint8Array();
    }
    if (service === "surface/open") return this.#open(UTF8.decode(body));
    if (service === "prefetch") return prefetch(UTF8.decode(body), document.baseURI, this.#prefetching.signal, fetch);
    if (service === "channel/receive") {
      if (!this.#parent) throw new Error("this program has no parent channel");
      if (this.#receiving) throw new Error("a channel receive is already pending");
      const next = this.#inbox.shift(); if (next) return next;
      return new Promise<Bytes>((resolve, reject) => { this.#receiving = { resolve, reject }; });
    }
    if (service === "channel/reply") {
      if (!this.#parent || body.length < 5) throw new Error("invalid channel reply");
      this.#parent.reply(wordAt(body), body.subarray(5), body[4] === 1); return new Uint8Array();
    }
    if (service === "surface/failed") { this.#parent?.signal("failed"); return new Uint8Array(); }
    if (service === "surface/ready") { this.#parent?.signal("ready"); return new Uint8Array(); }
    if (service === "surface/return") { this.#parent?.signal("return"); return new Uint8Array(); }
    if (service === "nav/location") return ENCODER.encode(addressPath(location));
    if (service === "nav/push" || service === "nav/replace" || service === "nav/changed") {
      if (window.parent !== window) throw new Error("an embedded program has no address of its own");
      if (service === "nav/changed") {
        const here = () => ENCODER.encode(addressPath(location));
        if (this.#navigated) { this.#navigated = false; return here(); }
        return new Promise<Bytes>(resolve => { this.#navigating.add(() => resolve(here())); });
      }
      const address = sameOriginAddress(UTF8.decode(body), location.href);
      if (service === "nav/push") history.pushState(null, "", address);
      else history.replaceState(null, "", address);
      return new Uint8Array();
    }
    if (service === "nav/open") {
      // A new tab with no way back to this page.
      window.open(linkUrl(UTF8.decode(body)), "_blank", "noopener,noreferrer"); return new Uint8Array();
    }
    if (service === "nav/go") {
      location.assign(sameOriginAddress(UTF8.decode(body), location.href)); return new Uint8Array();
    }
    if (service === "nav/reload") {
      // The page again from its address: only the page's own program, not one in a surface.
      if (window.parent !== window) throw new Error("only the page's own program reloads the page");
      location.reload(); return new Uint8Array();
    }
    if (service === "keys/find") {
      if (!this.#input) throw new Error("this host has no keys to claim yet");
      this.#input.claimFind(body[0] === 1); return new Uint8Array();
    }
    if (service === "surface/leave") {
      // Answers once the parent asks this program to return.
      if (!this.#parent) throw new Error("this program has no parent channel");
      if (this.#asked) { this.#asked = false; return new Uint8Array(); }
      return new Promise<Bytes>(resolve => { this.#leaving.add(() => resolve(new Uint8Array())); });
    }
    if (service.startsWith("surface/")) {
      const s = this.#surfaces.get(wordAt(body)); if (!s) throw new Error("the program surface is closed");
      if (service === "surface/call") return s.rpc.call(body.subarray(4));
      if (service === "surface/frame") {
        let timer: ReturnType<typeof setTimeout> | undefined;
        try { await Promise.race([s.firstFrame, s.closed.then(() => { throw new Error("the surface closed before drawing"); }), new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("the program did not become ready")), 120_000); })]); }
        catch (e) { s.close(); throw e; }
        finally { clearTimeout(timer); }
        return new Uint8Array();
      }
      if (service === "surface/closed") { await s.closed; return new Uint8Array(); }
      if (service === "surface/show") {
        if (this.#shown !== null) throw new Error("a program surface is already shown");
        const name = shownName(body);
        // Shown in this program's place, it's no longer behind it.
        if (this.#underlaid === wordAt(body)) this.#underlay(null);
        this.#shown = wordAt(body); s.frame.style.visibility = ""; s.frame.style.zIndex = "2"; s.frame.style.pointerEvents = "auto"; this.canvas.style.visibility = "hidden"; this.visible(false); s.frame.contentWindow?.focus();
        if (name && window.parent === window && typeof history !== "undefined") history.pushState({ wrela: this.#shown }, "", addressPath({ pathname: location.pathname, search: location.search, hash: `#${name}` }));
        return new Uint8Array();
      }
      if (service === "surface/underlay") {
        // On (1) or off (0): off hides only the surface that's underlaid.
        if (body.length !== 5) throw new Error("invalid underlay request");
        const id = wordAt(body);
        if (body[4] === 1) {
          if (this.#shown === id) throw new Error("the program surface is shown in this one's place");
          this.#underlay(id);
        } else if (this.#underlaid === id) this.#underlay(null);
        return new Uint8Array();
      }
      if (service === "surface/close") { s.close(); return new Uint8Array(); }
    }
    if (service === "file/export") {
      const { name, offset } = exportName(body);
      const url = URL.createObjectURL(new Blob([body.slice(offset)]));
      const a = document.createElement("a"); a.href = url; a.download = name; a.click(); setTimeout(() => URL.revokeObjectURL(url), 60_000); return new Uint8Array();
    }
    if (service === "file/import") {
      return new Promise<Bytes>((resolve, reject) => {
        const input = document.createElement("input"); input.type = "file";
        input.oncancel = () => reject(new Error("file selection cancelled"));
        input.onchange = () => { const file = input.files?.[0]; if (!file || file.size > MESSAGE_LIMIT) { reject(new Error("the file is missing or too large")); return; } file.arrayBuffer().then(b => resolve(new Uint8Array(b)), reject); };
        input.click();
      });
    }
    if (service === "passkey/create") return createPasskey(body, navigator.credentials);
    if (service === "passkey/get") return usePasskey(body, navigator.credentials);
    throw new Error(`this host has no capability ${service}`);
  }
}
