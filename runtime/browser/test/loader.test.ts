import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { MANIFEST_VERSION } from "../src/abi.gen.ts";
import { pathProblem } from "../src/check.ts";
import { browserIo, type Fetch, loadBuild } from "../src/loader.ts";
import { ManifestError } from "../src/manifest.ts";
import { FIRST_LIGHT, readFixture, readFixtureText } from "./fixtures.ts";

const BASE = "http://localhost/game/index.html";

/** Serves `files` (or the first-light fixture) under http://localhost/game/. */
function server(files: Record<string, string> = {}): { fetch: Fetch; requested: string[] } {
  const requested: string[] = [];
  const fetch: Fetch = async (url) => {
    requested.push(url.pathname);
    const name = url.pathname.replace(/^\/game\//, "");
    if (name in files) return new Response(files[name]);
    const path = join(FIRST_LIGHT, name);
    if (!existsSync(path)) return new Response("nope", { status: 404, statusText: "Not Found" });
    return new Response(readFileSync(path));
  };
  return { fetch, requested };
}

test("loads a build's manifest, WASM and shaders", async () => {
  const { fetch, requested } = server();
  const build = await loadBuild(BASE, fetch);
  expect(build.manifest.pipelines.length).toBe(2);
  expect(build.wasm).toEqual(readFixture("game.wasm"));
  expect(build.shaders).toEqual(
    ["first-light.wgsl", "fill.wgsl"].map(readFixtureText),
  );
  expect(requested.sort()).toEqual(
    ["/game/fill.wgsl", "/game/first-light.wgsl", "/game/game.wasm", "/game/manifest.json"].sort(),
  );
});

test("rejects a manifest of another version, loudly", async () => {
  const other = MANIFEST_VERSION + 1;
  const manifest = readFixtureText("manifest.json").replace(
    `"manifest_version": ${MANIFEST_VERSION}`,
    `"manifest_version": ${other}`,
  );
  const load = loadBuild(BASE, server({ "manifest.json": manifest }).fetch);
  await expect(load).rejects.toThrow(ManifestError);
  await expect(loadBuild(BASE, server({ "manifest.json": manifest }).fetch)).rejects.toThrow(
    `invalid manifest: manifest version ${other}, but this host reads version ${MANIFEST_VERSION}`,
  );
});

test("reads text files as strict UTF-8 that keeps a byte-order mark, as the native host does", async () => {
  const manifest = readFixtureText("manifest.json");
  await expect(loadBuild(BASE, server({ "manifest.json": `\uFEFF${manifest}` }).fetch)).rejects.toThrow(ManifestError);
  const bad = new Uint8Array([...new TextEncoder().encode(manifest), 0xff]);
  const files: Fetch = async (url) => (url.pathname.endsWith("manifest.json") ? new Response(bad) : server().fetch(url));
  await expect(loadBuild(BASE, files)).rejects.toThrow("http://localhost/game/manifest.json isn't UTF-8");
});

test("names a file it can't fetch", async () => {
  const manifest = readFixtureText("manifest.json").replace("fill.wgsl", "missing.wgsl");
  await expect(loadBuild(BASE, server({ "manifest.json": manifest }).fetch)).rejects.toThrow(
    "can't fetch http://localhost/game/missing.wgsl: 404 Not Found",
  );
  const failing: Fetch = async () => {
    throw new TypeError("network down");
  };
  await expect(loadBuild(BASE, failing)).rejects.toThrow("can't fetch http://localhost/game/manifest.json: network down");
});

test("fetches and posts literal percent escapes inside the build", async () => {
  const requested: { url: string; method: string }[] = [];
  const fetch: Fetch = async (url, init) => {
    requested.push({ url: url.href, method: init?.method ?? "GET" });
    return new Response(Uint8Array.of(1));
  };
  const io = browserIo(BASE, fetch);
  await io.fetch("%2e%2e/secret.bin");
  await io.post(".%2E/admin", new Uint8Array());
  expect(requested).toEqual([
    { url: "http://localhost/game/%252e%252e/secret.bin", method: "GET" },
    { url: "http://localhost/game/.%252E/admin", method: "POST" },
  ]);
});

test("checked URL paths keep spaces and controls literal inside the build", async () => {
  const paths = [" /admin", "\t/admin", "sa\nve.bin", "folder/name "];
  const requested: string[] = [];
  const io = browserIo(BASE, async (url) => { requested.push(url.href); return new Response(""); });
  for (const path of paths) {
    expect(pathProblem(path)).toBeUndefined();
    await io.fetch(path);
    await io.post(path, new Uint8Array());
  }
  const expected = ["%20/admin", "%09/admin", "sa%0Ave.bin", "folder/name%20"];
  expect(requested).toEqual(expected.flatMap((path) => [`http://localhost/game/${path}`, `http://localhost/game/${path}`]));
});

test("failed storage writes and closes abort their stream so a retry can take the file lock", async () => {
  const original = Object.getOwnPropertyDescriptor(navigator, "storage");
  try {
    for (const failed of ["write", "close"]) {
      const calls: string[] = [];
      let locked = false;
      let fail = true;
      const file = {
        async createWritable() {
          if (locked) throw new Error("the file is locked");
          locked = true;
          return {
            async write() {
              calls.push("write");
              if (failed === "write" && fail) throw new Error("write failed");
            },
            async close() {
              calls.push("close");
              if (failed === "close" && fail) throw new Error("close failed");
              locked = false;
            },
            async abort() {
              calls.push("abort");
              locked = false;
              // Cleanup's failure must not replace the error that failed the request.
              throw new Error("abort failed");
            },
          };
        },
      };
      const dir: { getDirectoryHandle(): Promise<object>; getFileHandle(): Promise<typeof file> } = {
        async getDirectoryHandle() { return dir; }, async getFileHandle() { return file; },
      };
      Object.defineProperty(navigator, "storage", { value: { getDirectory: async () => dir }, configurable: true });
      const io = browserIo(BASE);
      await expect(io.storageWrite("save.bin", Uint8Array.of(1))).rejects.toThrow(`${failed} failed`);
      expect(locked).toBe(false);
      fail = false;
      await io.storageWrite("save.bin", Uint8Array.of(2));
      expect(calls).toEqual(failed === "write" ? ["write", "abort", "write", "close"] : ["write", "close", "abort", "write", "close"]);
      expect(locked).toBe(false);
    }
  } finally {
    if (original) Object.defineProperty(navigator, "storage", original);
    else delete (navigator as unknown as Record<string, unknown>)["storage"];
  }
});

test("stable storage survives release paths while default page storage stays separate", async () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const saved = new Map<string, Uint8Array>();
  const directory = (path: string[]): unknown => ({
    getDirectoryHandle: async (name: string) => directory([...path, name]),
    getFileHandle: async (name: string) => ({
      createWritable: async () => ({ write: async (data: Uint8Array) => saved.set([...path, name].join("/"), data.slice()), close: async () => {} }),
      getFile: async () => ({ arrayBuffer: async () => { const bytes = saved.get([...path, name].join("/")); if (!bytes) throw new Error("missing"); return bytes.slice().buffer; } }),
    }),
  });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { storage: { getDirectory: async () => directory([]) } } });
  try {
    const first = browserIo("https://game.example/releases/old/session/", undefined, undefined, undefined, "game");
    const next = browserIo("https://game.example/releases/new/floor/", undefined, undefined, undefined, "game");
    await first.storageWrite("tower/save", Uint8Array.of(3, 7));
    expect(await next.storageRead("tower/save")).toEqual(Uint8Array.of(3, 7));
    expect([...saved.keys()]).toEqual(["wrela/game/tower/save"]);
    await expect(browserIo(BASE).storageRead("tower/save")).rejects.toThrow("missing");
    expect(() => browserIo(BASE, undefined, undefined, undefined, "../game")).toThrow("namespace");
  } finally {
    if (previous) Object.defineProperty(globalThis, "navigator", previous);
    else Reflect.deleteProperty(globalThis, "navigator");
  }
});
