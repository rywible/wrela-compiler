import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { type Fetch, loadBuild } from "../src/loader.ts";
import { ManifestError } from "../src/manifest.ts";
import { FIRST_LIGHT } from "./fixtures.ts";

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
  expect(build.wasm).toEqual(new Uint8Array(readFileSync(join(FIRST_LIGHT, "game.wasm"))));
  expect(build.shaders).toEqual(
    ["first-light.wgsl", "fill.wgsl"].map((f) => readFileSync(join(FIRST_LIGHT, f), "utf8")),
  );
  expect(requested.sort()).toEqual(
    ["/game/fill.wgsl", "/game/first-light.wgsl", "/game/game.wasm", "/game/manifest.json"].sort(),
  );
});

test("rejects a manifest of another version, loudly", async () => {
  const manifest = readFileSync(join(FIRST_LIGHT, "manifest.json"), "utf8").replace(
    '"manifest_version": 1',
    '"manifest_version": 2',
  );
  const load = loadBuild(BASE, server({ "manifest.json": manifest }).fetch);
  await expect(load).rejects.toThrow(ManifestError);
  await expect(loadBuild(BASE, server({ "manifest.json": manifest }).fetch)).rejects.toThrow(
    "invalid manifest: manifest version 2, but this host reads version 1",
  );
});

test("names a file it can't fetch", async () => {
  const manifest = readFileSync(join(FIRST_LIGHT, "manifest.json"), "utf8").replace("fill.wgsl", "missing.wgsl");
  await expect(loadBuild(BASE, server({ "manifest.json": manifest }).fetch)).rejects.toThrow(
    "can't fetch http://localhost/game/missing.wgsl: 404 Not Found",
  );
  const failing: Fetch = async () => {
    throw new TypeError("network down");
  };
  await expect(loadBuild(BASE, failing)).rejects.toThrow("can't fetch http://localhost/game/manifest.json: network down");
});
