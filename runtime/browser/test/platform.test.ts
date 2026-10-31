import { expect, test } from "bun:test";
import type { Semantics } from "../src/semantics.ts";
import { BrowserPlatform, createPasskey, decodeImage, PASSKEY_ALGORITHMS, usePasskey, environment, exportName, iconLink, linkUrl, MESSAGE_LIMIT, PASTE_EVENTS, PortRpc, prefetch, programUrl, sameOriginAddress, SEMANTICS, shownName, TEXT_INPUT, wordAt, wordBytes } from "../src/platform.ts";
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

test("clipboard writes preserve an initial byte-order mark as text", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
  let written = "";
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent: {}, addEventListener() {} } });
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (text: string) => { written = text; } } });
  const platform = new BrowserPlatform({ style: {} } as HTMLCanvasElement, () => {});
  try {
    await platform.request("clipboard/write", new TextEncoder().encode("\uFEFFtext"));
    expect(written).toBe("\uFEFFtext");
  } finally {
    platform.dispose();
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
    if (oldClipboard) Object.defineProperty(navigator, "clipboard", oldClipboard); else Reflect.deleteProperty(navigator, "clipboard");
  }
});

test("the environment says whether this page load reloaded the tab, what the host has, then the colour scheme", () => {
  const load = (type: string) => [{ type } as unknown as PerformanceEntry];
  const b = environment(true, 1_700_000_000_999, 2, load("navigate"), PASTE_EVENTS | SEMANTICS);
  expect([b[0], wordAt(b.subarray(1)), wordAt(b.subarray(5)), b[9], wordAt(b.subarray(10))]).toEqual([1, 1_700_000_000, 2048, 0, PASTE_EVENTS | SEMANTICS]);
  expect(b.length).toBe(15);
  expect(b[14]).toBe(0);
  expect(environment(false, 0, 1, [], 0, true)[14]).toBe(1);
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
  // A fragment is the opened program's, not the network's: the page is fetched without it.
  seen.length = 0;
  await prefetch("capybara/#attract", "https://console.example/", new AbortController().signal, fetchFn);
  expect(seen.map(s => s.url)).toEqual(["https://console.example/capybara/"]);
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
  for (const url of ["javascript:alert(1)", "http://example.com/", "https://user:secret@example.com/", "https://example.com/?redirect=other", "https://example.com/?#mode"]) {
    expect(() => programUrl(url, base)).toThrow();
  }
  // A fragment is the program's to read, but never test mode, which only a page's own address asks for.
  expect(programUrl("capybara/#attract", base).href).toBe("https://games.wrela.dev/capybara/#attract");
  expect(programUrl("https://example.com/#mode", base).hash).toBe("#mode");
  for (const url of ["capybara/#test", "http://127.0.0.1:8100/#test&frames=3"]) expect(() => programUrl(url, base)).toThrow("test mode");
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


test("a program opened at an address with a fragment reads it as its own address", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const oldLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const ports: MessagePort[] = [];
  const frame = {
    src: "",
    style: {},
    onload: null as (() => void) | null,
    onerror: null as (() => void) | null,
    contentWindow: {
      postMessage: (_message: unknown, origin: string, transferred: MessagePort[]) => {
        expect(origin).toBe("https://console.example");
        ports.push(transferred[0]!);
        transferred[0]!.postMessage({ type: "connected" });
      },
    },
    remove: () => {},
  };
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent: {}, addEventListener: () => {} } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: {
    baseURI: "https://console.example/",
    createElement: () => frame,
    body: { append: () => queueMicrotask(() => frame.onload?.()) },
  } });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const opener = new BrowserPlatform(canvas, () => {});
  let opened: BrowserPlatform | null = null;
  try {
    await opener.request("surface/open", new TextEncoder().encode("capybara/#attract"));
    expect(frame.src).toBe("https://console.example/capybara/#attract");
    // In the frame, at that address: its own address keeps the fragment.
    Object.defineProperty(globalThis, "location", { configurable: true, value: new URL(frame.src) });
    opened = new BrowserPlatform(canvas, () => {});
    expect(new TextDecoder().decode(await opened.request("nav/location", new Uint8Array()))).toBe("/capybara/#attract");
  } finally {
    opener.dispose(); opened?.dispose();
    for (const port of ports) port.close();
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
    if (oldDocument) Object.defineProperty(globalThis, "document", oldDocument); else Reflect.deleteProperty(globalThis, "document");
    if (oldLocation) Object.defineProperty(globalThis, "location", oldLocation); else Reflect.deleteProperty(globalThis, "location");
  }
});

test("export filenames fit full session keys and reject traversal or truncation", () => {
  const body = (name: string) => { const text = new TextEncoder().encode(name); const b = new Uint8Array(4 + text.length); b.set(wordBytes(text.length)); b.set(text, 4); return b; };
  const longest = `${"g".repeat(64)}-${"p".repeat(64)}-${"s".repeat(64)}.wrela-save`;
  expect(exportName(body(longest)).name).toBe(longest);
  for (const name of ["../save", "save/path", "", "x".repeat(241)]) expect(() => exportName(body(name))).toThrow("filename");
  expect(() => exportName(wordBytes(10))).toThrow("filename");
});

test("shown program names accept every catalog key including a leading hyphen", () => {
  const body = (name: string) => new Writer().u32(1).raw(new TextEncoder().encode(name)).bytes();
  for (const name of ["-", "-game", "a0-", "-".repeat(64)]) expect(shownName(body(name))).toBe(name);
  for (const name of ["_", "A", "../game", "-".repeat(65)]) expect(() => shownName(body(name))).toThrow("invalid program name");
  expect(shownName(wordBytes(1))).toBeNull();
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
    expect(before & (PASTE_EVENTS | TEXT_INPUT | SEMANTICS)).toBe(0);
    await expect(platform.request("text/input", new Writer().u8(1).f32(1).f32(2).f32(3).f32(4).bytes())).rejects.toThrow("no text input");
    const input = {
      release() {},
      textInput: (on: boolean, caret: unknown) => calls.push(["text", on, caret]),
      pasted: (n: number) => (n === 3 ? { text: "hi", html: "" } : null),
      offer: (text: string, html: string) => calls.push(["offer", text, html]),
      claimFind: (on: boolean) => calls.push(["find", on]),
    };
    const overlay = {
      tree: (b: Uint8Array) => calls.push(["tree", b.length]),
      geometry: (b: Uint8Array) => calls.push(["geometry", b.length]),
      fieldState: (n: number) => new Uint8Array([n]),
      submit: (form: number) => calls.push(["submit", form]),
      announce: (t: string) => calls.push(["say", t]),
    };
    platform.attach(input, overlay as unknown as Semantics);
    expect((await capabilities()) & (PASTE_EVENTS | TEXT_INPUT | SEMANTICS)).toBe(PASTE_EVENTS | TEXT_INPUT | SEMANTICS);
    await platform.request("text/input", new Writer().u8(1).f32(1).f32(2).f32(3).f32(4).bytes());
    await platform.request("clipboard/offer", new Writer().str("a").str("<b>a</b>").bytes());
    // A copy button's text goes to the clipboard at once; a refusal comes back as an error.
    const written: string[] = [];
    const clipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (t: string) => { if (t === "no") throw new Error("not allowed"); written.push(t); } } });
    try {
      await platform.request("clipboard/write", new TextEncoder().encode("let x = 1"));
      await expect(platform.request("clipboard/write", new TextEncoder().encode("no"))).rejects.toThrow("not allowed");
      expect(written).toEqual(["let x = 1"]);
    } finally {
      if (clipboard) Object.defineProperty(navigator, "clipboard", clipboard); else Reflect.deleteProperty(navigator, "clipboard");
    }
    const pasted = new Reader(await platform.request("clipboard/pasted", wordBytes(3)));
    expect([pasted.str(), pasted.str()]).toEqual(["hi", ""]);
    await expect(platform.request("clipboard/pasted", wordBytes(4))).rejects.toThrow("isn't kept");
    await platform.request("a11y/announce", new TextEncoder().encode("3 items selected"));
    await platform.request("keys/find", new Uint8Array([1]));
    await platform.request("semantics/tree", new Uint8Array(3));
    await platform.request("semantics/geometry", new Uint8Array(4));
    expect(await platform.request("field/state", wordBytes(9))).toEqual(new Uint8Array([9]));
    await platform.request("form/submit", wordBytes(2));
    expect(calls).toEqual([
      ["text", true, { x: 1, y: 2, w: 3, h: 4 }], ["offer", "a", "<b>a</b>"], ["say", "3 items selected"], ["find", true],
      ["tree", 3], ["geometry", 4], ["submit", 2],
    ]);
  } finally {
    platform.dispose();
    names.forEach((n, i) => { const d = saved[i]; if (d) Object.defineProperty(globalThis, n, d); else Reflect.deleteProperty(globalThis, n); });
  }
});

test("only the page's own program reloads the page", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  let reloads = 0;
  const page: { parent?: unknown; addEventListener: () => void } = { addEventListener: () => {} };
  Object.defineProperty(globalThis, "window", { configurable: true, value: page });
  Object.defineProperty(globalThis, "location", { configurable: true, value: { reload: () => { reloads++; } } });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, () => {});
  try {
    // A program in a surface: its window's parent is the page that opened it.
    page.parent = {};
    await expect(platform.request("nav/reload", new Uint8Array())).rejects.toThrow("only the page's own program");
    expect(reloads).toBe(0);
    page.parent = page;
    expect(await platform.request("nav/reload", new Uint8Array())).toEqual(new Uint8Array());
    expect(reloads).toBe(1);
  } finally {
    platform.dispose();
    for (const [name, old] of [["window", oldWindow], ["location", oldLocation]] as const) {
      if (old) Object.defineProperty(globalThis, name, old); else Reflect.deleteProperty(globalThis, name);
    }
  }
});

test("an underlaid program shows behind the canvas, muted and without input, until it's shown", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const ports: MessagePort[] = [];
  type Frame = { style: { visibility?: string }; inert: boolean; heard: unknown[]; onload: (() => void) | null; onerror: (() => void) | null; contentWindow: unknown; remove: () => void };
  const frames: Frame[] = [];
  const makeFrame = (): Frame => {
    const frame: Frame = {
      style: {},
      inert: false,
      heard: [],
      onload: null,
      onerror: null,
      contentWindow: {
        postMessage: (_message: unknown, _origin: string, transferred: MessagePort[]) => {
          const port = transferred[0]!;
          ports.push(port);
          port.onmessage = ({ data }) => frame.heard.push(data);
          port.postMessage({ type: "connected" });
        },
        focus: () => {},
      },
      remove: () => {},
    };
    frames.push(frame);
    return frame;
  };
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent: {}, addEventListener: () => {} } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: {
    baseURI: "https://console.example/",
    createElement: makeFrame,
    body: { append: () => { const frame = frames.at(-1)!; queueMicrotask(() => frame.onload?.()); } },
  } });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const seeThrough: boolean[] = [];
  const platform = new BrowserPlatform(canvas, () => {}, on => seeThrough.push(on));
  const underlay = (handle: Uint8Array, on: boolean) => platform.request("surface/underlay", new Writer().raw(handle).u8(on ? 1 : 0).bytes());
  const heard = async (frame: Frame) => { await new Promise(r => setTimeout(r, 0)); return frame.heard; };
  try {
    const game = await platform.request("surface/open", new TextEncoder().encode("https://game.example/attract/"));
    const other = await platform.request("surface/open", new TextEncoder().encode("https://game.example/other/"));
    const [behind, beside] = [frames[0]!, frames[1]!];
    await expect(platform.request("surface/underlay", game)).rejects.toThrow("invalid underlay");
    await underlay(game, true);
    // The canvas lets it through; it takes no input and is told it's underlaid; the other hides.
    expect(seeThrough).toEqual([true]);
    expect(behind.inert).toBe(true);
    expect(behind.style.visibility).toBe("");
    expect(beside.style.visibility).toBe("hidden");
    expect(await heard(behind)).toEqual([{ type: "underlay" }]);
    // Off for one that isn't underlaid changes nothing.
    await underlay(other, false);
    expect(seeThrough).toEqual([true]);
    // Shown in the console's place, it's no longer underlaid.
    await platform.request("surface/show", game);
    expect(seeThrough).toEqual([true, false]);
    expect(behind.inert).toBe(false);
    expect(beside.style.visibility).toBe("");
    expect(await heard(behind)).toEqual([{ type: "underlay" }, { type: "underlay-end" }]);
    await expect(underlay(game, true)).rejects.toThrow("shown in this one's place");
    // Underlaid again and closed: the canvas is opaque again.
    await underlay(other, true);
    expect(seeThrough).toEqual([true, false, true]);
    await platform.request("surface/close", other);
    expect(seeThrough).toEqual([true, false, true, false]);
  } finally {
    platform.dispose();
    for (const port of ports) port.close();
    for (const [name, old] of [["window", oldWindow], ["document", oldDocument]] as const) {
      if (old) Object.defineProperty(globalThis, name, old); else Reflect.deleteProperty(globalThis, name);
    }
  }
});

test("a program its opener underlays hears so, and hears when it no longer is", async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const listeners = new Map<string, (event: unknown) => void>();
  const parent = {};
  Object.defineProperty(globalThis, "window", { configurable: true, value: { parent, addEventListener: (type: string, listener: (event: unknown) => void) => listeners.set(type, listener) } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { referrer: "https://console.example/" } });
  const platform = new BrowserPlatform({ style: {}, tabIndex: -1 } as HTMLCanvasElement, () => {});
  const told: boolean[] = [];
  platform.onUnderlaid = on => told.push(on);
  const channel = new MessageChannel();
  try {
    listeners.get("message")!({ source: parent, origin: "https://console.example", data: { type: "wrela-connect" }, ports: [channel.port2] });
    channel.port1.postMessage({ type: "underlay" });
    await new Promise(r => setTimeout(r, 0));
    expect(platform.underlaid).toBe(true);
    channel.port1.postMessage({ type: "underlay-end" });
    await new Promise(r => setTimeout(r, 0));
    expect(platform.underlaid).toBe(false);
    expect(told).toEqual([true, false]);
  } finally {
    platform.dispose(); channel.port1.close(); channel.port2.close();
    for (const [name, old] of [["window", oldWindow], ["document", oldDocument]] as const) {
      if (old) Object.defineProperty(globalThis, name, old); else Reflect.deleteProperty(globalThis, name);
    }
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
    (location as { pathname: string }).pathname = "//nested.example/";
    await show(second, "last-green");
    expect(at).toBe(1);
    expect(new URL(entries[at]!.url, "https://console.example/").href).toBe("https://console.example//nested.example/?from=link#last-green");
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

test("a program's address stays on its page's origin, and a link is an http or https URL", () => {
  const here = "https://docs.example/v0.1/#std/io";
  expect(sameOriginAddress("#std/net", here)).toBe("/v0.1/#std/net");
  expect(sameOriginAddress("/v0.2/?q=arena#std/arena", here)).toBe("/v0.2/?q=arena#std/arena");
  expect(sameOriginAddress("../latest/", here)).toBe("/latest/");
  for (const bad of ["https://elsewhere.example/", "//elsewhere.example/x", "javascript:alert(1)", "a\\b", "#a\nb", "", "#".repeat(3000)]) {
    expect(() => sameOriginAddress(bad, here)).toThrow();
  }
  expect(linkUrl("https://github.com/rywible/wrela")).toBe("https://github.com/rywible/wrela");
  for (const bad of ["/relative", "javascript:alert(1)", "data:text/html,x", "https://user:pass@example.com/", "file:///etc/passwd"]) {
    expect(() => linkUrl(bad)).toThrow();
  }
});

test("same-origin navigation cannot turn a double-slash path into another origin", () => {
  const here = "https://docs.example/v0.1/";
  const requested = new URL("https://docs.example//elsewhere.example/path?q=x#part");
  const address = sameOriginAddress(requested.href, here);
  const followed = new URL(address, here);
  expect(followed.href).toBe(requested.href);
  expect(() => sameOriginAddress("blob:https://docs.example/identifier", here)).toThrow();
});

test("a top-level program moves through its own addresses, and hears Back and Forward", async () => {
  const names = ["window", "history", "location"] as const;
  const saved = names.map((n) => Object.getOwnPropertyDescriptor(globalThis, n));
  const listeners = new Map<string, () => void>();
  const page: { parent?: unknown; addEventListener: (type: string, f: () => void) => void } = { addEventListener: (type, f) => listeners.set(type, f) };
  page.parent = page;
  // The session history, and the location it shows.
  const entries = ["/v0.1/"];
  let at = 0;
  const location = {
    get pathname() { return new URL(entries[at]!, "https://docs.example").pathname; },
    get search() { return new URL(entries[at]!, "https://docs.example").search; },
    get hash() { return new URL(entries[at]!, "https://docs.example").hash; },
    get href() { return new URL(entries[at]!, "https://docs.example").href; },
  };
  const pages = {
    state: null,
    pushState: (_s: unknown, _t: string, url: string) => { entries.splice(at + 1); entries.push(url); at++; },
    replaceState: (_s: unknown, _t: string, url: string) => { entries[at] = url; },
  };
  Object.defineProperty(globalThis, "window", { configurable: true, value: page });
  Object.defineProperty(globalThis, "history", { configurable: true, value: pages });
  Object.defineProperty(globalThis, "location", { configurable: true, value: location });
  const canvas = { style: {}, tabIndex: -1, focus: () => {} } as HTMLCanvasElement;
  const platform = new BrowserPlatform(canvas, () => {});
  const text = async (service: string, body = "") => new TextDecoder().decode(await platform.request(service, new TextEncoder().encode(body)));
  const back = () => { at--; listeners.get("popstate")!(); };
  try {
    await text("nav/push", "#std/io");
    await text("nav/push", "#std/net");
    await text("nav/replace", "#std/net?x");
    expect(entries).toEqual(["/v0.1/", "/v0.1/#std/io", "/v0.1/#std/net?x"]);
    expect(await text("nav/location")).toBe("/v0.1/#std/net?x");
    // Back while the program waits: it hears the address it's at now.
    const heard = text("nav/changed");
    back();
    expect(await heard).toBe("/v0.1/#std/io");
    // Back before it waits: its next wait answers at once.
    back();
    expect(await text("nav/changed")).toBe("/v0.1/");
    await expect(text("nav/push", "https://elsewhere.example/")).rejects.toThrow("own origin");
    // The reported address can be passed back to navigation, even when its path starts //.
    await text("nav/push", "https://docs.example//nested.example/path");
    const current = await text("nav/location");
    await text("nav/replace", current);
    expect(location.href).toBe("https://docs.example//nested.example/path");
    await text("nav/push", "/next");
    back();
    expect(await text("nav/changed")).toBe(current);
    // A program in a frame has no address of its own.
    page.parent = {};
    await expect(text("nav/push", "#x")).rejects.toThrow("no address of its own");
  } finally {
    platform.dispose();
    names.forEach((n, i) => { const d = saved[i]; if (d) Object.defineProperty(globalThis, n, d); else Reflect.deleteProperty(globalThis, n); });
  }
});

const buffer = (bytes: number[]): ArrayBuffer => new Uint8Array(bytes).buffer;

test("making a passkey asks for a discoverable, verified key of passkeys' algorithms, and answers its bytes", async () => {
  let asked: CredentialCreationOptions | undefined;
  const credentials = {
    create: async (options: CredentialCreationOptions) => {
      asked = options;
      return { type: "public-key", rawId: buffer([1, 2]), response: { clientDataJSON: buffer([123, 125]), attestationObject: buffer([9]) } };
    },
    get: async () => null,
  } as unknown as Parameters<typeof createPasskey>[1];
  const body = new Writer().str("wrela.dev").str("wrela").chunk(new Uint8Array([7, 7])).str("bea").str("Bea")
    .chunk(new Uint8Array([5, 6, 7])).u32(1).chunk(new Uint8Array([3])).u32(60_000).bytes();
  const answer = new Reader(await createPasskey(body, credentials));
  expect([...answer.chunk()]).toEqual([1, 2]);
  expect([...answer.chunk()]).toEqual([123, 125]);
  expect([...answer.chunk()]).toEqual([9]);
  answer.end();
  const o = asked!.publicKey!;
  expect(o.rp).toEqual({ id: "wrela.dev", name: "wrela" });
  expect(o.user.name).toBe("bea");
  expect([...new Uint8Array(o.user.id as ArrayBuffer)]).toEqual([7, 7]);
  expect([...new Uint8Array(o.challenge as ArrayBuffer)]).toEqual([5, 6, 7]);
  expect(o.pubKeyCredParams.map(p => p.alg)).toEqual(PASSKEY_ALGORITHMS);
  expect(o.authenticatorSelection).toEqual({ residentKey: "required", requireResidentKey: true, userVerification: "required" });
  expect(o.attestation).toBe("none");
  expect(o.excludeCredentials!.length).toBe(1);
  expect(o.timeout).toBe(60_000);
});

test("using a passkey asks for the person's verification and answers the assertion's bytes", async () => {
  let asked: CredentialRequestOptions | undefined;
  const credentials = {
    create: async () => null,
    get: async (options: CredentialRequestOptions) => {
      asked = options;
      return { type: "public-key", rawId: buffer([4]), response: { clientDataJSON: buffer([1]), authenticatorData: buffer([2, 2]), signature: buffer([3]), userHandle: null } };
    },
  } as unknown as Parameters<typeof usePasskey>[1];
  const body = new Writer().str("wrela.dev").chunk(new Uint8Array([8])).u32(0).u32(30_000).bytes();
  const answer = new Reader(await usePasskey(body, credentials));
  expect([...answer.chunk()]).toEqual([4]);
  expect([...answer.chunk()]).toEqual([1]);
  expect([...answer.chunk()]).toEqual([2, 2]);
  expect([...answer.chunk()]).toEqual([3]);
  expect([...answer.chunk()]).toEqual([]); // no user handle
  answer.end();
  expect(asked!.publicKey!.rpId).toBe("wrela.dev");
  expect(asked!.publicKey!.userVerification).toBe("required");
  expect(asked!.publicKey!.allowCredentials).toEqual([]);
});

test("a passkey request with bytes past its end, or a person who cancels, fails", async () => {
  const credentials = {
    create: async () => { throw new DOMException("cancelled", "NotAllowedError"); },
    get: async () => null,
  } as unknown as Parameters<typeof usePasskey>[1];
  const body = new Writer().str("wrela.dev").chunk(new Uint8Array([8])).u32(0).u32(30_000).bytes();
  await expect(usePasskey(new Writer().raw(body).u8(0).bytes(), credentials)).rejects.toThrow("longer than it says");
  await expect(usePasskey(body, credentials)).rejects.toThrow("no passkey was used");
  const create = new Writer().str("a").str("b").chunk(new Uint8Array([1])).str("c").str("d").chunk(new Uint8Array([2])).u32(0).u32(1).bytes();
  await expect(createPasskey(create, credentials)).rejects.toThrow("cancelled");
});
