import { expect, test } from "bun:test";
import type { Overlay } from "../src/assist.ts";
import { ACCESSIBILITY, BrowserPlatform, decodeImage, environment, exportName, iconLink, MESSAGE_LIMIT, PASTE_EVENTS, PortRpc, prefetch, programUrl, TEXT_INPUT, wordAt, wordBytes } from "../src/platform.ts";
import { Reader, Writer } from "../src/wire.ts";

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

test("the environment says whether this page load reloaded the tab, then what the host has", () => {
  const load = (type: string) => [{ type } as unknown as PerformanceEntry];
  const b = environment(true, 1_700_000_000_999, 2, load("navigate"), PASTE_EVENTS | ACCESSIBILITY);
  expect([b[0], wordAt(b.subarray(1)), wordAt(b.subarray(5)), b[9], wordAt(b.subarray(10))]).toEqual([1, 1_700_000_000, 2048, 0, PASTE_EVENTS | ACCESSIBILITY]);
  expect(b.length).toBe(14);
  expect(environment(false, 0, 1, load("reload"), 0)[9]).toBe(1);
  // Back and forward are visits, and so is a page whose navigation the browser didn't record.
  expect(environment(false, 0, 1, load("back_forward"), 0)[9]).toBe(0);
  expect(environment(false, 0, 1, [], 0)[9]).toBe(0);
});

test("an image decodes only at the size the program expects", async () => {
  const pixels = new Uint8ClampedArray(3 * 2 * 4).fill(7);
  const decoder = async (image: Uint8Array) => {
    expect([...image]).toEqual([9, 9]);
    return { width: 3, height: 2, pixels };
  };
  const body = (w: number, h: number) => { const b = new Uint8Array(10); b.set(wordBytes(w)); b.set(wordBytes(h), 4); b.set([9, 9], 8); return b; };
  expect([...await decodeImage(body(3, 2), decoder)]).toEqual([...pixels]);
  await expect(decodeImage(body(2, 3), decoder)).rejects.toThrow("not 2x3");
  await expect(decodeImage(body(0, 2), decoder)).rejects.toThrow("invalid image size");
  await expect(decodeImage(body(5000, 5000), decoder)).rejects.toThrow("invalid image size");
  await expect(decodeImage(new Uint8Array(3), decoder)).rejects.toThrow();
});

test("prefetching fetches each URL at low priority until it is stopped", async () => {
  const seen: { url: string; init: RequestInit | undefined }[] = [];
  const controller = new AbortController();
  const fetchFn = (async (url: URL, init?: RequestInit) => {
    seen.push({ url: url.href, init });
    if (url.pathname.endsWith("/missing")) throw new Error("offline");
    if (url.pathname.endsWith("/stop")) controller.abort();
    return new Response("x");
  }) as unknown as typeof fetch;
  const list = "https://a.example/one\n\nhttps://a.example/missing\nhttps://a.example/two\n";
  await prefetch(list, "https://console.example/", new AbortController().signal, fetchFn);
  expect(seen.map(s => s.url).sort()).toEqual(["https://a.example/missing", "https://a.example/one", "https://a.example/two"]);
  expect(seen[0]!.init).toMatchObject({ mode: "cors", credentials: "include", priority: "low" });
  // Stopped (a program opening), nothing more starts.
  seen.length = 0;
  const many = ["stop", ...Array.from({ length: 20 }, (_, i) => `f${i}`)].map(n => `https://a.example/${n}`).join("\n");
  await prefetch(many, "https://console.example/", controller.signal, fetchFn);
  expect(seen.length).toBeLessThanOrEqual(3);
  // A URL that isn't a secure, plain address is refused before anything is fetched.
  await expect(prefetch("javascript:alert(1)", "https://console.example/", new AbortController().signal, fetchFn)).rejects.toThrow();
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

test("icons are square PNGs the build shipped, linked from the page", () => {
  const body = (s: string) => new TextEncoder().encode(s);
  const base = "https://play.example/console/";
  expect(iconLink(body("icon 32 files/icons/icon-32.png"), base))
    .toEqual({ rel: "icon", sizes: "32x32", href: "https://play.example/console/files/icons/icon-32.png" });
  expect(iconLink(body("apple-touch-icon 180 files/icons/icon-180.png"), base).rel).toBe("apple-touch-icon");
  for (const bad of [
    "icon 32 https://x.example/i.png", "icon 32 files/../secret.png", "icon 32 files/icons/../x.png",
    "icon 32 files/icons/i.svg", "icon 32 icons/i.png", "manifest 32 files/icons/i.png", "icon 0 files/icons/i.png",
    "icon 4096 files/icons/i.png", "icon 32 files/icons/i.png extra", "icon 32 files/.icons/i.png",
  ]) expect(() => iconLink(body(bad), base)).toThrow("invalid icon");
});

test("a program sets the canvas cursor only to a listed shape", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent: {}, addEventListener: () => {} } });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, () => {});
  const encode = (s: string) => new TextEncoder().encode(s);
  try {
    expect(await platform.request("cursor", encode("pointer"))).toEqual(new Uint8Array());
    expect(canvas.style.cursor).toBe("pointer");
    await expect(platform.request("cursor", encode("url(https://x.example/c.png), auto"))).rejects.toThrow("unsupported cursor");
    await expect(platform.request("cursor", new Uint8Array([255]))).rejects.toThrow();
    expect(canvas.style.cursor).toBe("pointer");
    await platform.request("cursor", encode("default"));
    expect(canvas.style.cursor).toBe("default");
    for (const name of ["grab", "grabbing", "crosshair", "move", "nwse-resize", "nesw-resize"]) {
      await platform.request("cursor", encode(name));
      expect(canvas.style.cursor).toBe(name);
    }
  } finally {
    platform.dispose();
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
  }
});

test("the environment says which input services the host has, and they reach them", async () => {
  const names = ["window", "matchMedia"] as const;
  const saved = names.map((n) => Object.getOwnPropertyDescriptor(globalThis, n));
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent: {}, addEventListener: () => {}, devicePixelRatio: 2 } });
  Object.defineProperty(globalThis, "matchMedia", { configurable: true, value: () => ({ matches: false }) });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, () => {});
  const capabilities = async () => wordAt((await platform.request("environment", new Uint8Array())).subarray(10));
  const calls: unknown[] = [];
  try {
    const before = await capabilities();
    expect(before & (PASTE_EVENTS | TEXT_INPUT | ACCESSIBILITY)).toBe(0);
    await expect(platform.request("text/input", new Writer().u8(1).f32(1).f32(2).f32(3).f32(4).bytes())).rejects.toThrow("no text input");
    const input = {
      release() {},
      textInput: (on: boolean, caret: unknown) => calls.push(["text", on, caret]),
      pasted: (n: number) => (n === 3 ? { text: "hi", html: "" } : null),
      offer: (text: string, html: string) => calls.push(["offer", text, html]),
    };
    const overlay = { tree: (b: Uint8Array) => calls.push(["tree", b.length]), announce: (t: string) => calls.push(["say", t]) };
    platform.attach(input, overlay as unknown as Overlay);
    expect((await capabilities()) & (PASTE_EVENTS | TEXT_INPUT | ACCESSIBILITY)).toBe(PASTE_EVENTS | TEXT_INPUT | ACCESSIBILITY);
    await platform.request("text/input", new Writer().u8(1).f32(1).f32(2).f32(3).f32(4).bytes());
    await platform.request("clipboard/offer", new Writer().str("a").str("<b>a</b>").bytes());
    const pasted = new Reader(await platform.request("clipboard/pasted", wordBytes(3)));
    expect([pasted.str(), pasted.str()]).toEqual(["hi", ""]);
    await expect(platform.request("clipboard/pasted", wordBytes(4))).rejects.toThrow("isn't kept");
    await platform.request("a11y/announce", new TextEncoder().encode("3 items selected"));
    expect(calls).toEqual([["text", true, { x: 1, y: 2, w: 3, h: 4 }], ["offer", "a", "<b>a</b>"], ["say", "3 items selected"]]);
  } finally {
    platform.dispose();
    names.forEach((n, i) => { const d = saved[i]; if (d) Object.defineProperty(globalThis, n, d); else Reflect.deleteProperty(globalThis, n); });
  }
});

test("a shown program is named in the address, and Back asks it to return", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const oldHistory = Object.getOwnPropertyDescriptor(globalThis, "history");
  const oldLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const ports: MessagePort[] = [];
  const heard: unknown[] = [];
  const frame = {
    style: {},
    onload: null as (() => void) | null,
    onerror: null as (() => void) | null,
    contentWindow: {
      postMessage: (_message: unknown, _origin: string, transferred: MessagePort[]) => {
        const port = transferred[0]!;
        ports.push(port);
        port.onmessage = ({ data }) => heard.push(data);
        port.postMessage({ type: "connected" });
      },
      focus: () => {},
    },
    remove: () => {},
  };
  const listeners = new Map<string, () => void>();
  const page: { parent?: unknown; addEventListener: (type: string, listener: () => void) => void } = {
    addEventListener: (type, listener) => listeners.set(type, listener),
  };
  page.parent = page;
  // The session history: entries of state and address, and the one shown.
  const entries: { state: unknown; url: string }[] = [{ state: null, url: "/" }];
  let at = 0;
  const pages = {
    get state() { return entries[at]!.state; },
    pushState: (state: unknown, _title: string, url: string) => { entries.splice(at + 1); entries.push({ state, url }); at++; },
    replaceState: (state: unknown, _title: string, url: string) => { entries[at] = { state, url }; },
    back: () => { at--; queueMicrotask(() => listeners.get("popstate")!()); },
  };
  Object.defineProperty(globalThis, "window", { configurable: true, value: page });
  Object.defineProperty(globalThis, "history", { configurable: true, value: pages });
  Object.defineProperty(globalThis, "location", { configurable: true, value: { hash: "", pathname: "/", search: "?from=link" } });
  // The page's <base> is its release's directory; the address keeps the page's own path.
  Object.defineProperty(globalThis, "document", { configurable: true, value: {
    baseURI: "https://console.example/releases/0123456789abcdef0123/",
    createElement: () => frame,
    body: { append: () => queueMicrotask(() => frame.onload?.()) },
  } });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, () => {});
  const show = (handle: Uint8Array, name: string) => {
    const body = new Uint8Array(handle.length + name.length);
    body.set(handle); body.set(new TextEncoder().encode(name), handle.length);
    return platform.request("surface/show", body);
  };
  try {
    const first = await platform.request("surface/open", new TextEncoder().encode("https://game.example/session/"));
    await expect(show(first, "../evil")).rejects.toThrow("invalid program name");
    await show(first, "leapling");
    expect(entries.map(e => e.url)).toEqual(["/", "/?from=link#leapling"]);
    // Back: the program is asked to return, and the page is the console's again.
    pages.back();
    await new Promise(r => setTimeout(r, 0));
    expect(heard).toContainEqual({ type: "leave" });
    expect(platform.active).toBe(false);
    frame.onload?.();
    expect(platform.active).toBe(true);
    expect(at).toBe(0);
    // A program that returns by itself takes its entry with it.
    const second = await platform.request("surface/open", new TextEncoder().encode("https://game.example/session/"));
    await show(second, "last-green");
    expect(at).toBe(1);
    frame.onload?.();
    expect(at).toBe(0);
  } finally {
    platform.dispose();
    for (const port of ports) port.close();
    for (const [name, old] of [["window", oldWindow], ["document", oldDocument], ["history", oldHistory], ["location", oldLocation]] as const) {
      if (old) Object.defineProperty(globalThis, name, old); else Reflect.deleteProperty(globalThis, name);
    }
  }
});
