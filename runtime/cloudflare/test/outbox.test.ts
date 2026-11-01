// An instance's outbox on a hand-built module whose `serve_init` sends one message: it's kept
// with the call's writes, tried at once, then with backoff up to an hour, logged once past an
// hour without what it says, dropped only once acknowledged, and a reset object goes on from its
// alarm. The edge has no store: a send or a write there fails the call as the program's.

import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { Encoder } from "../../browser/test/encoder.ts";
import { buildModule, op, SUBMIT } from "../../browser/test/wasm-builder.ts";
import { Out } from "../../browser/src/http.ts";
import { backoff, decodeResponse, encodeRequest, type Peers, Server } from "../src/host.ts";
import { storage } from "./fakes.ts";

const SERVE = { params: ["f32" as const], results: [] };

/** A module whose `serve_init` asks `service` with `body`, and whose `serve` does nothing. */
function initAsking(service: string, body: Uint8Array): WebAssembly.Module {
  const batch = new Encoder().hostRequest(1, service, body).finish();
  const init = [...op.i32(64), ...op.i32(batch.length), ...op.call(0)];
  return new WebAssembly.Module(
    buildModule({
      imports: [SUBMIT],
      memory: 1,
      ownMemory: true,
      data: [{ offset: 64, bytes: batch }],
      funcs: [
        { type: { params: [], results: [] }, body: init, export: "serve_init" },
        { type: SERVE, body: [], export: "serve" },
      ],
    }),
  );
}

const SECRET = new Uint8Array([7, 7, 7]);
const sends = initAsking("server/send", new Out().text("board/all").chunk(SECRET).bytes());
const settled = () => new Promise((r) => setTimeout(r, 5));

/** Peers whose `deliver` answers what `acks` says, and keeps what it was given. */
function peers(acks: () => boolean): Peers & { given: [string, string, number, number[]][] } {
  const given: [string, string, number, number[]][] = [];
  return {
    given,
    request: async () => new Uint8Array(0),
    call: async () => null,
    callBuild: async () => null,
    deliver: async (name, from, id, body) => {
      given.push([name, from, id, [...body]]);
      return acks();
    },
  };
}

const waiting = (db: Database) => db.query("SELECT id, tries, due, told FROM wrela_outbox").all() as { id: number; tries: number; due: number; told: number }[];

test("a message is tried with backoff to an hour, logged once past it, and dropped once acknowledged", async () => {
  const db = new Database(":memory:");
  let now = 10_000;
  let ack = false;
  const alarms: number[] = [];
  const logged: string[] = [];
  const p = peers(() => ack);
  const s = new Server(sends, storage(db), { now: () => now, peers: p, setAlarm: (at) => alarms.push(at), log: (l) => logged.push(l) });
  s.name("account/a");
  await s.alarm();
  await settled();
  // Tried at once, from its sender, and missed: a second's wait.
  expect(p.given).toEqual([["board/all", "account/a", 1, [7, 7, 7]]]);
  expect(waiting(db)).toEqual([{ id: 1, tries: 1, due: 11_000, told: 0 }]);
  expect(alarms.at(-1)).toBe(11_000);
  // Each miss waits twice as long, up to an hour.
  expect([1, 2, 3, 12, 13, 40].map(backoff)).toEqual([1000, 2000, 4000, 2_048_000, 3_600_000, 3_600_000]);
  for (let tries = 2; now - 10_000 < 3_600_000; tries++) {
    now = waiting(db)[0]!.due;
    await s.alarm();
    expect(waiting(db)[0]!.tries).toBe(tries);
  }
  // Past an hour, one line, naming the receiver's kind and nothing it says.
  expect(logged.length).toBe(1);
  expect(logged[0]).toContain("a `board` instance has waited more than an hour");
  expect(logged[0]).not.toContain("all");
  now = waiting(db)[0]!.due;
  await s.alarm();
  expect(logged.length).toBe(1);
  // The same message each time, until it's acknowledged.
  expect(new Set(p.given.map((g) => g[2]))).toEqual(new Set([1]));
  ack = true;
  now = waiting(db)[0]!.due;
  await s.alarm();
  expect(waiting(db)).toEqual([]);
});

test("a reset sender goes on from its alarm with the message its call committed", async () => {
  const db = new Database(":memory:");
  const now = 50_000;
  const p = peers(() => false);
  // The sender commits, and is reset before it delivers anything.
  const first = new Server(sends, storage(db), { now: () => now, setAlarm: () => {} });
  first.name("account/a");
  await first.alarm();
  expect(waiting(db).length).toBe(1);
  const alarms: number[] = [];
  const again = new Server(sends, storage(db), { now: () => now, peers: p, setAlarm: (at) => alarms.push(at) });
  expect(alarms).toEqual([50_000]);
  await again.alarm();
  await settled();
  // Its name was kept: the message comes from it.
  expect(p.given[0]).toEqual(["board/all", "account/a", 1, [7, 7, 7]]);
});

test("the edge has no store: a send or a write there is the program's failure", async () => {
  const request = encodeRequest({ time: 0n, method: "GET", path: "/api/x", query: "", headers: [], body: new Uint8Array(0) });
  for (const [module, why] of [
    [sends, "the edge has no outbox"],
    [initAsking("store/write", new Out().push(new Uint8Array([1])).chunk(SECRET).chunk(SECRET).bytes()), "the edge has no store"],
  ] as const) {
    const logged: string[] = [];
    const edge = new Server(module, null, { log: (l) => logged.push(l) });
    expect(decodeResponse(await edge.request(request)).status).toBe(500);
    expect(logged[0]).toContain(why);
  }
});
