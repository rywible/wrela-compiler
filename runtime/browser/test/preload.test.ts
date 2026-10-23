import { expect, test } from "bun:test";
import { Handed } from "../src/loader.ts";
import type { ToWorker } from "../src/messages.ts";
import { handOver } from "../src/preload.ts";

const bytes = (text: string) => new TextEncoder().encode(text) as Uint8Array<ArrayBuffer>;

test("the worker takes a file the page handed it, once, and fetches the rest", async () => {
  const fetched: string[] = [];
  const redirects: (RequestRedirect | undefined)[] = [];
  const network = async (url: URL, init?: RequestInit) => {
    fetched.push(url.href);
    redirects.push(init?.redirect);
    return new Response(`net ${url.pathname}`);
  };
  const handed = new Handed(["http://x/a/manifest.json", "http://x/a/game.wasm"]);
  const fetch = handed.fetch(network);
  const manifest = fetch(new URL("http://x/a/manifest.json"));
  // The page's fetch ends after the worker asked: it waits for it.
  handed.give("http://x/a/manifest.json", bytes("handed"));
  expect(await (await manifest).text()).toBe("handed");
  // Once only: a second fetch goes to the network.
  expect(await (await fetch(new URL("http://x/a/manifest.json"), { redirect: "manual" })).text()).toBe("net /a/manifest.json");
  // One the page couldn't fetch, and one it never fetched, go to the network.
  handed.give("http://x/a/game.wasm", null);
  expect(await (await fetch(new URL("http://x/a/game.wasm"), { redirect: "manual" })).text()).toBe("net /a/game.wasm");
  expect(await (await fetch(new URL("http://x/a/pipeline_0.wgsl"), { redirect: "manual" })).text()).toBe("net /a/pipeline_0.wgsl");
  expect(fetched).toEqual(["http://x/a/manifest.json", "http://x/a/game.wasm", "http://x/a/pipeline_0.wgsl"]);
  expect(redirects).toEqual(["manual", "manual", "manual"]);
});

test("the page hands each preloaded file to the worker, or null where its fetch fails", async () => {
  const posted: ToWorker[] = [];
  let transferred = 0;
  const redirects: (RequestRedirect | undefined)[] = [];
  const worker = {
    postMessage(msg: ToWorker, transfer: Transferable[]) {
      posted.push(msg);
      transferred += transfer.length;
    },
  };
  const fetch = (async (url: string, init?: RequestInit) => {
    redirects.push(init?.redirect);
    return url.endsWith("gone") ? new Response("", { status: 404 }) : url.endsWith("down") ? Promise.reject(new Error("offline")) : new Response(`bytes of ${url}`);
  }) as typeof globalThis.fetch;
  handOver(worker as Pick<Worker, "postMessage">, ["http://x/manifest.json", "http://x/gone", "http://x/down"], fetch);
  await new Promise((r) => setTimeout(r, 10));
  const files = new Map(posted.map((m) => (m.type === "file" ? [m.url, m.bytes] : ["", null])));
  expect(new TextDecoder().decode(files.get("http://x/manifest.json")!)).toBe("bytes of http://x/manifest.json");
  expect(files.get("http://x/gone")).toBeNull();
  expect(files.get("http://x/down")).toBeNull();
  expect(transferred).toBe(1);
  expect(redirects).toEqual(["manual", "manual", "manual"]);
});
