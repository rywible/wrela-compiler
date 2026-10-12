// Application-host mechanisms. Services transport opaque bytes; the Wrela application
// owns its catalog, UI, session protocol and save policy.
import type { Bytes } from "./stream.ts";

export const MESSAGE_LIMIT = 1024 * 1024 - 512;
const DECODER = new TextDecoder("utf-8", { fatal: true });
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
  const name = DECODER.decode(body.subarray(4, 4 + n));
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(name)) throw new Error("invalid export filename");
  return { name, offset: 4 + n };
}
const checked = (body: unknown): Bytes => {
  if (!(body instanceof Uint8Array) || body.length > MESSAGE_LIMIT) throw new Error("invalid capability message");
  return new Uint8Array(body);
};

/** A bounded request/reply channel. Closing it rejects every outstanding request. */
export class PortRpc {
  #next = 1;
  #closed = false;
  #calls = new Map<number, { resolve: (b: Bytes) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  constructor(readonly port: MessagePort, readonly receive: (id: number, body: Bytes) => void, readonly event: (kind: string) => void = () => {}) {
    port.onmessage = ({ data }: MessageEvent<unknown>) => {
      if (!data || typeof data !== "object" || !("type" in data)) return;
      const m = data as { type: unknown; id?: unknown; body?: unknown; ok?: unknown };
      if (m.type === "frame" || m.type === "ready" || m.type === "return" || m.type === "failed") { this.event(m.type); return; }
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
  signal(type: "frame" | "ready" | "return" | "failed"): void { if (!this.#closed) this.port.postMessage({ type }); }
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.port.close();
    for (const call of this.#calls.values()) { clearTimeout(call.timer); call.reject(new Error("the program channel closed")); }
    this.#calls.clear();
  }
}

export function programUrl(text: string, base: string): URL {
  const url = new URL(text, base);
  if (url.username || url.password || url.hash || url.search) throw new Error("program URLs cannot contain credentials, queries or fragments");
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
  constructor(readonly canvas: HTMLCanvasElement, readonly visible: (v: boolean) => void) {
    canvas.style.position = "relative"; canvas.style.zIndex = "1"; canvas.style.outline = "none"; canvas.tabIndex = 0;
    window.addEventListener("message", (e: MessageEvent<unknown>) => {
      if (window.parent === window || e.source !== window.parent || this.#parent || !document.referrer) return;
      if (e.origin !== new URL(document.referrer).origin) return;
      if (!e.data || typeof e.data !== "object" || !("type" in e.data) || e.data.type !== "wrela-connect" || e.ports.length !== 1) return;
      this.#parent = new PortRpc(e.ports[0]!, (id, body) => {
        const b = new Uint8Array(body.length + 4); b.set(wordBytes(id)); b.set(body, 4);
        if (this.#receiving) { const give = this.#receiving; this.#receiving = null; give.resolve(b); }
        else if (this.#inbox.length < 32) this.#inbox.push(b);
        else this.#parent?.reply(id, new Uint8Array(), false);
      });
      e.ports[0]!.postMessage({ type: "connected" });
    });
    window.addEventListener("pagehide", () => this.dispose());
  }
  get active(): boolean { return this.#shown === null; }
  presented(): void { this.#parent?.signal("frame"); }
  failed(): void { this.#parent?.signal("failed"); }
  dispose(): void {
    for (const s of [...this.#surfaces.values()]) s.close();
    this.#parent?.close(); this.#parent = null;
    this.#receiving?.reject(new Error("the parent channel closed")); this.#receiving = null; this.#inbox = [];
  }
  async #open(text: string): Promise<Bytes> {
    if (this.#surfaces.size >= 4) throw new Error("too many embedded programs");
    const url = programUrl(text, document.baseURI);
    const frame = document.createElement("iframe");
    frame.src = url.href;
    frame.title = "Wrela program";
    frame.allow = "cross-origin-isolated; autoplay; gamepad; fullscreen";
    frame.referrerPolicy = "origin";
    frame.style.cssText = "position:fixed;inset:0;width:100%;height:100%;border:0;z-index:0;pointer-events:none";
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
      if (this.#shown === id) { this.#shown = null; this.canvas.style.visibility = "visible"; this.visible(true); this.canvas.focus(); }
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
      const b = new Uint8Array(9); b[0] = Number(matchMedia("(prefers-reduced-motion: reduce)").matches); b.set(wordBytes(Math.floor(Date.now() / 1000)), 1); b.set(wordBytes(Math.round((window.devicePixelRatio || 1) * 1024)), 5); return b;
    }
    if (service === "surface/open") return this.#open(DECODER.decode(body));
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
      if (service === "surface/show") { if (this.#shown !== null) throw new Error("a program surface is already shown"); this.#shown = wordAt(body); s.frame.style.zIndex = "2"; s.frame.style.pointerEvents = "auto"; this.canvas.style.visibility = "hidden"; this.visible(false); s.frame.contentWindow?.focus(); return new Uint8Array(); }
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
    throw new Error(`this host has no capability ${service}`);
  }
}
