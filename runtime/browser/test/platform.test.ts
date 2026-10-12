import { expect, test } from "bun:test";
import { BrowserPlatform, exportName, MESSAGE_LIMIT, PortRpc, programUrl, wordAt, wordBytes } from "../src/platform.ts";

test("program channels correlate replies and close outstanding requests", async () => {
  const channel = new MessageChannel();
  let right!: PortRpc;
  const left = new PortRpc(channel.port1, () => {});
  right = new PortRpc(channel.port2, (id, body) => right.reply(id, body.slice().reverse()));
  try {
    expect(await left.call(new Uint8Array([1, 2, 3]))).toEqual(new Uint8Array([3, 2, 1]));
    right.close();
    const waiting = left.call(new Uint8Array([9]));
    left.close();
    await expect(waiting).rejects.toThrow("closed");
    await expect(left.call(new Uint8Array())).rejects.toThrow("closed");
  } finally { left.close(); right.close(); }
});

test("program channels reject oversized requests before transmission", () => {
  const channel = new MessageChannel();
  const rpc = new PortRpc(channel.port1, () => {});
  try { expect(() => rpc.call(new Uint8Array(MESSAGE_LIMIT + 1))).toThrow("invalid capability message"); }
  finally { rpc.close(); channel.port2.close(); }
});

test("program URLs require a secure origin and reject credential and query injection", () => {
  const base = "https://games.wrela.dev/";
  expect(programUrl("https://last-green.games.wrela.dev/session/", base).origin).toBe("https://last-green.games.wrela.dev");
  expect(programUrl("http://127.0.0.1:8100/session/", base).hostname).toBe("127.0.0.1");
  for (const url of ["javascript:alert(1)", "http://example.com/", "https://user:secret@example.com/", "https://example.com/?redirect=other", "https://example.com/#mode"]) {
    expect(() => programUrl(url, base)).toThrow();
  }
  expect(wordAt(wordBytes(0xffff_fffe))).toBe(0xffff_fffe);
  expect(() => wordAt(new Uint8Array(3))).toThrow("missing");
});


test("parent channels reject a different origin and dispose a waiting receive", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const listeners = new Map<string, (event: unknown) => void>();
  const parent = {};
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent, addEventListener: (type: string, listener: (event: unknown) => void) => listeners.set(type, listener) } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { referrer: "https://console.example/" } });
  const canvas = { style: {}, tabIndex: -1 } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, () => {});
  const channel = new MessageChannel();
  try {
    listeners.get("message")!({ source: parent, origin: "https://attacker.example", data: { type: "wrela-connect" }, ports: [channel.port2] });
    await expect(platform.request("channel/receive", new Uint8Array())).rejects.toThrow("no parent");
    listeners.get("message")!({ source: parent, origin: "https://console.example", data: { type: "wrela-connect" }, ports: [channel.port2] });
    const waiting = platform.request("channel/receive", new Uint8Array());
    platform.dispose();
    await expect(waiting).rejects.toThrow("closed");
  } finally {
    platform.dispose(); channel.port1.close(); channel.port2.close();
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
    if (oldDocument) Object.defineProperty(globalThis, "document", oldDocument); else Reflect.deleteProperty(globalThis, "document");
  }
});

test("reloading an embedded program closes its surface and restores the console", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const ports: MessagePort[] = [];
  const visibility: boolean[] = [];
  let removed = false;
  let focused = 0;
  const frame = {
    style: {},
    onload: null as (() => void) | null,
    onerror: null as (() => void) | null,
    contentWindow: {
      postMessage: (_message: unknown, _origin: string, transferred: MessagePort[]) => {
        ports.push(transferred[0]!);
        transferred[0]!.postMessage({ type: "connected" });
      },
      focus: () => {},
    },
    remove: () => { removed = true; },
  };
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent: {}, addEventListener: () => {} } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: {
    baseURI: "https://console.example/",
    createElement: () => frame,
    body: { append: () => queueMicrotask(() => frame.onload?.()) },
  } });
  const canvas = { style: {}, tabIndex: -1, focus: () => { focused++; } } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, visible => visibility.push(visible));
  try {
    const handle = await platform.request("surface/open", new TextEncoder().encode("https://game.example/session/"));
    await platform.request("surface/show", handle);
    const closed = platform.request("surface/closed", handle);
    expect(platform.active).toBe(false);
    expect(canvas.style.visibility).toBe("hidden");
    frame.onload?.();
    expect(platform.active).toBe(true);
    expect(removed).toBe(true);
    expect(canvas.style.visibility).toBe("visible");
    expect(visibility).toEqual([false, true]);
    expect(focused).toBe(1);
    expect(await closed).toEqual(new Uint8Array());
    await expect(platform.request("surface/call", handle)).rejects.toThrow("closed");
    expect(ports).toHaveLength(1);
  } finally {
    platform.dispose();
    for (const port of ports) port.close();
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
    if (oldDocument) Object.defineProperty(globalThis, "document", oldDocument); else Reflect.deleteProperty(globalThis, "document");
  }
});


test("export filenames fit full session keys and reject traversal or truncation", () => {
  const body = (name: string) => { const text = new TextEncoder().encode(name); const b = new Uint8Array(4 + text.length); b.set(wordBytes(text.length)); b.set(text, 4); return b; };
  const longest = `${"g".repeat(64)}-${"p".repeat(64)}-${"s".repeat(64)}.wrela-save`;
  expect(exportName(body(longest)).name).toBe(longest);
  for (const name of ["../save", "save/path", "", "x".repeat(241)]) expect(() => exportName(body(name))).toThrow("filename");
  expect(() => exportName(wordBytes(10))).toThrow("filename");
});
