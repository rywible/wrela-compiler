// A built server program of instances through its Cloudflare host, as the Worker runs it: `api`
// and the edge in front, a namespace of `WrelaServer` objects behind, one an instance, each with
// its own bun:sqlite storage. The compiler's suite builds the program (compiler/tests/tests/
// suite/http.rs, `INSTANCES`) and runs this with WRELA_INSTANCES_BUILD naming the build; run
// alone, it has nothing to test and skips.
//
// The program: `account/a` and `account/b` add a request's byte to a sum and send the board
// (`board/all`) their sum with a revision; the board keeps each account's newest and acknowledges
// in the call that writes it. The edge routes, calls `account/a`, asks an `hmac`, and calls the
// build's verifier, which replies `ok` to `good`, `no` to `bad` and traps on anything else.

import { beforeAll, describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { Env } from "../src/cloudflare.ts";
import { bucket, FakeLoader, Namespace, workers } from "./fakes.ts";

const BUILD = process.env["WRELA_INSTANCES_BUILD"];
const SITE = "https://wrela.dev";

interface Host {
  api(request: Request, env: Env): Promise<Response | null>;
  WrelaServer: new (ctx: unknown, env: Env) => ReturnType<Namespace["object"]>;
}

let host: Host;
let env: Env;
let ns: Namespace;
const loader = new FakeLoader();

beforeAll(async () => {
  if (BUILD === undefined) return;
  workers();
  host = (await import(join(BUILD, "server/cloudflare.js"))) as Host;
  const file = (name: string) => readFileSync(join(BUILD, "server/verifier", name));
  const files: Record<string, Uint8Array> = {};
  for (const name of ["main.js", "cloudflare.js", "game.wasm", "verifier.json"]) files[`verifiers/ledger/1/${name}`] = file(name);
  ns = new Namespace((ctx) => new host.WrelaServer(ctx, env));
  env = { WRELA_SERVER: ns, WRELA_ORIGINS: SITE, WRELA_LOADER: loader, WRELA_BUILDS: bucket(files) };
});

async function ask(path: string, body?: Uint8Array<ArrayBuffer> | string): Promise<Response> {
  const init: RequestInit = body === undefined ? {} : { method: "POST", headers: { origin: SITE }, body };
  const r = await host.api(new Request(`${SITE}${path}`, init), env);
  if (r === null) throw new Error(`${path} isn't the API's`);
  return r;
}

const text = async (path: string, body?: Uint8Array<ArrayBuffer> | string) => (await ask(path, body)).text();
const add = (who: string, n: number) => text(`/api/add/${who}`, new Uint8Array([n]));

/** Waits until `ok` holds, or fails after a few seconds. */
async function until(ok: () => boolean | Promise<boolean>, what: string): Promise<void> {
  for (let i = 0; i < 400; i++) {
    if (await ok()) return;
    await new Promise((r) => setTimeout(r, 5));
  }
  throw new Error(`never: ${what}`);
}

/** Makes every message in `name`'s outbox due now, as if its backoff had passed, and runs its
 * alarm. */
async function retry(name: string): Promise<void> {
  ns.dbs.get(name)!.run("UPDATE wrela_outbox SET due = 0");
  await ns.object(name).alarm();
}

describe.skipIf(BUILD === undefined)("a server build of instances", () => {
  test("the edge routes each request: to an instance, kept, or refused before any object", async () => {
    expect(await text("/api/me")).toBe("edge");
    expect(await text("/api/me/a")).toBe("account/a");
    const refused = await ask("/api/nowhere");
    expect(refused.status).toBe(404);
    expect(refused.headers.get("x-content-type-options")).toBe("nosniff");
    expect([...ns.objects.keys()]).toEqual(["account/a"]);
  });

  test("a call is answered with the callee's reply, after its writes", async () => {
    expect(await add("a", 5)).toBe("5");
    expect(await text("/api/peek")).toBe("5");
  });

  test("a message is acknowledged once its effect commits, and only then dropped", async () => {
    await until(async () => (await text("/api/board")) === "account/a=5;", "the board has a's sum");
    expect(ns.waiting("account/a")).toBe(0);
    expect(await add("b", 2)).toBe("2");
    await until(async () => (await text("/api/board")) === "account/a=5;account/b=2;", "the board has b's sum");
    expect(ns.waiting("account/b")).toBe(0);
  });

  test("messages delivered out of order and twice leave the newest", async () => {
    const held: (() => void)[] = [];
    ns.wire = (rpc, go) => {
      if (rpc.method !== "deliver") return go();
      // Each delivery waits; the second is made twice.
      return new Promise((resolve, reject) => held.push(() => void go().then(resolve, reject)));
    };
    expect(await add("a", 1)).toBe("6");
    expect(await add("a", 2)).toBe("8");
    await until(() => held.length === 2, "both messages are on their way");
    ns.wire = (_rpc, go) => go();
    held[1]!();
    held[1]!();
    await until(async () => (await text("/api/board")).startsWith("account/a=8;"), "the newer lands");
    held[0]!();
    await until(() => ns.waiting("account/a") === 0, "every message is acknowledged");
    expect(await text("/api/board")).toBe("account/a=8;account/b=2;");
  });

  test("a receiver reset between its commit and the acknowledgement's return gets it again", async () => {
    let lost = 0;
    ns.wire = async (rpc, go) => {
      if (rpc.method !== "deliver" || lost > 0) return go();
      // The board commits and acknowledges; the answer is lost on its way back.
      await go();
      lost++;
      ns.restart("board/all");
      throw new Error("the receiver was reset");
    };
    expect(await add("b", 3)).toBe("5");
    await until(() => lost === 1, "the acknowledgement is lost");
    expect(ns.waiting("account/b")).toBe(1);
    expect(await text("/api/board")).toBe("account/a=8;account/b=5;");
    ns.wire = (_rpc, go) => go();
    await retry("account/b");
    expect(ns.waiting("account/b")).toBe(0);
    expect(await text("/api/board")).toBe("account/a=8;account/b=5;");
  });

  test("a sender reset between its commit and the delivery delivers from its alarm", async () => {
    ns.wire = async (rpc, go) => {
      if (rpc.method === "deliver") throw new Error("the sender was reset first");
      return go();
    };
    expect(await add("a", 4)).toBe("12");
    await until(() => ns.waiting("account/a") === 1, "the message waits");
    ns.restart("account/a");
    ns.wire = (_rpc, go) => go();
    await retry("account/a");
    expect(ns.waiting("account/a")).toBe(0);
    expect(await text("/api/board")).toBe("account/a=12;account/b=5;");
  });

  test("hmac is HMAC-SHA256", async () => {
    const message = new TextEncoder().encode("a challenge");
    const key = await crypto.subtle.importKey("raw", new TextEncoder().encode("key"), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const want = new Uint8Array(await crypto.subtle.sign("HMAC", key, message));
    expect(new Uint8Array(await (await ask("/api/hmac", message)).arrayBuffer())).toEqual(want);
  });

  test("the build's verifier loads through the Worker Loader with no bindings and no network", async () => {
    expect(await text("/api/verify", "good")).toBe("ok");
    expect(await text("/api/verify", "bad")).toBe("no");
    // A malformed replay traps the verifier: the call fails, and the next is answered afresh.
    expect((await ask("/api/verify", "junk!")).status).toBe(502);
    expect(await text("/api/verify", "good")).toBe("ok");
    // A board with no artifact.
    expect((await ask("/api/verify-old", "good")).status).toBe(502);
    const code = loader.loaded.get("verifiers/ledger/1")!;
    expect(code.globalOutbound).toBeNull();
    expect(code.env).toEqual({});
    expect(code.mainModule).toBe("main.js");
    expect(Object.keys(code.modules).sort()).toEqual(["cloudflare.js", "game.wasm", "main.js"]);
  });
});
