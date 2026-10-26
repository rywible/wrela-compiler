// Getting builds ready for a hand-over (`std::reload`), on a fake device, mirroring
// compiler/tests/tests/suite/handover.rs (where the native host makes the hand-overs too).

import { expect, spyOn, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { SCREEN_FORMAT } from "../src/abi.gen.ts";
import { buildPipelines, type PipelineCache } from "../src/gpu.ts";
import { HandOvers } from "../src/handover.ts";
import { type Fetch, loadBuild } from "../src/loader.ts";
import { FakeDevice } from "./fake-gpu.ts";
import { FIRST_LIGHT, readFixtureText } from "./fixtures.ts";

const RUNNING = "http://localhost/tower/first/";

/** Serves the first-light fixture as `first`, and as `second` with its fill shader changed;
 * there's nothing else. The paths asked for. */
function server(): { fetch: Fetch; requested: string[] } {
  const requested: string[] = [];
  const fetch: Fetch = async (url) => {
    requested.push(url.pathname);
    const [, , build, name] = url.pathname.split("/");
    if (build !== "first" && build !== "second") return new Response("nope", { status: 404, statusText: "Not Found" });
    if (build === "second" && name === "fill.wgsl") return new Response(`${readFixtureText("fill.wgsl")}\n// second's own\n`);
    const path = join(FIRST_LIGHT, name!);
    if (!existsSync(path)) return new Response("nope", { status: 404, statusText: "Not Found" });
    return new Response(readFileSync(path));
  };
  return { fetch, requested };
}

/** The hand-overs of a program running `first`, whose pipelines the device holds; each shader
 * the device compiles. */
async function running() {
  const device = new FakeDevice();
  const { fetch, requested } = server();
  const first = await loadBuild(RUNNING, fetch);
  const cache: PipelineCache = new Map();
  await Promise.all(buildPipelines(device.gpu, first.manifest, first.shaders, SCREEN_FORMAT, cache).each);
  const compiled = spyOn(device, "createShaderModule");
  const handOvers = new HandOvers(device.gpu, SCREEN_FORMAT, () => RUNNING, () => [cache], fetch);
  return { handOvers, compiled, requested };
}

test("a build got ready builds only the pipelines the device doesn't hold, once", async () => {
  const { handOvers, compiled, requested } = await running();
  await Promise.all([handOvers.prepare("../second"), handOvers.prepare("../second")]);
  expect(compiled).toHaveBeenCalledTimes(1);
  expect(requested.filter((p) => p === "/tower/second/manifest.json")).toHaveLength(1);
  const ready = await handOvers.ready(handOvers.baseOf("../second"));
  expect(ready.pipelines.size).toBe(2);
  expect(ready.manifest.pipelines).toHaveLength(2);
});

test("a build path's percent escapes stay literal during resolution", () => {
  const device = new FakeDevice();
  const handOvers = new HandOvers(device.gpu, SCREEN_FORMAT, () => RUNNING, () => []);
  expect(handOvers.baseOf("%2e%2e/second")).toBe("http://localhost/tower/first/%252e%252e/second/");
  expect(handOvers.baseOf("../%2e%2e/second")).toBe("http://localhost/tower/%252e%252e/second/");
});

test("build paths keep spaces and controls literal while allowing one parent", () => {
  const handOvers = new HandOvers(new FakeDevice().gpu, SCREEN_FORMAT, () => RUNNING, () => []);
  expect(handOvers.baseOf(" /admin")).toBe("http://localhost/tower/first/%20/admin/");
  expect(handOvers.baseOf("\t/admin")).toBe("http://localhost/tower/first/%09/admin/");
  expect(handOvers.baseOf("../next ")).toBe("http://localhost/tower/next%20/");
});

test("a build that isn't there fails, each time it's asked for", async () => {
  const { handOvers } = await running();
  const why = "the build at http://localhost/tower/missing/ isn't ready: can't fetch http://localhost/tower/missing/manifest.json: 404";
  await expect(handOvers.prepare("../missing")).rejects.toThrow(why);
  await expect(handOvers.prepare("../missing")).rejects.toThrow(why);
});

test("a build let go is got ready again from the start", async () => {
  const { handOvers, requested } = await running();
  await handOvers.prepare("../second");
  handOvers.unprepare("../second");
  await handOvers.prepare("../second");
  expect(requested.filter((p) => p === "/tower/second/manifest.json")).toHaveLength(2);
});

test("a build let go before it's ready fails the requests waiting on it", async () => {
  const { handOvers } = await running();
  const waiting = handOvers.prepare("../second");
  handOvers.unprepare("../second");
  await expect(waiting).rejects.toThrow("the build at http://localhost/tower/second/ was let go before it was ready");
});

test("the last hand-over asked for is the one made", async () => {
  const { handOvers } = await running();
  expect(handOvers.takeAsked()).toBeNull();
  const first = handOvers.handOver("../second");
  void handOvers.handOver("../third");
  await expect(first).rejects.toThrow("a later hand-over took its place");
  const asked = handOvers.takeAsked();
  expect(asked?.base).toBe("http://localhost/tower/third/");
  expect(handOvers.takeAsked()).toBeNull();
});
