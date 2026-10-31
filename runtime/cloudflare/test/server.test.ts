// The server's instance on hand-built modules: a trap answers the requests open on it with 500,
// a host limit (the memory cap) with 503, and the next request starts a fresh instance; a module
// that isn't a server build is the host's failure.

import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { Encoder } from "../../browser/test/encoder.ts";
import { buildModule, op, SUBMIT } from "../../browser/test/wasm-builder.ts";
import { Out } from "../../browser/src/http.ts";
import { decodeResponse, encodeRequest, LIMITS, Server, type Storage, Store } from "../src/host.ts";

function storage(): Storage {
  const db = new Database(":memory:");
  return {
    sql: {
      exec: (q: string, ...b: unknown[]) => {
        const rows = db.query(q).all(...(b as never[])) as Record<string, unknown>[];
        return Object.assign(rows[Symbol.iterator](), { toArray: () => rows });
      },
    },
    transactionSync: <T>(fn: () => T): T => db.transaction(fn)(),
  };
}

const SERVE = { params: ["f32" as const], results: [] };
const request = encodeRequest({ time: 0n, method: "GET", path: "/api/x", query: "", headers: [], body: new Uint8Array(0) });
const status = async (s: Server) => decodeResponse(await s.request(request)).status;

test("a trap answers 500, and the next request starts a fresh instance", async () => {
  const module = new WebAssembly.Module(
    buildModule({ memory: 1, ownMemory: true, funcs: [{ type: { params: [], results: [] }, body: [], export: "serve_init" }, { type: SERVE, body: op.unreachable, export: "serve" }] }),
  );
  const logged: string[] = [];
  const s = new Server(module, storage(), { log: (l) => logged.push(l) });
  expect(await status(s)).toBe(500);
  expect(logged[0]).toContain("the program's: ");
  expect(await status(s)).toBe(500);
  expect(logged.length).toBe(2);
});

test("a module that isn't a server build fails each request as the host's", async () => {
  const module = new WebAssembly.Module(buildModule({ memory: 1, ownMemory: true, funcs: [{ type: SERVE, body: [], export: "frame" }] }));
  const logged: string[] = [];
  const s = new Server(module, storage(), { log: (l) => logged.push(l) });
  expect(await status(s)).toBe(503);
  expect(logged[0]).toContain("doesn't export `serve`");
});

test("a call past the memory cap is the host's", async () => {
  const body = [...op.i32(1100), 0x40, 0x00, 0x1a];
  const module = new WebAssembly.Module(buildModule({ memory: 1, ownMemory: true, funcs: [{ type: SERVE, body, export: "serve" }] }));
  const logged: string[] = [];
  const s = new Server(module, storage(), { log: (l) => logged.push(l) });
  expect(await status(s)).toBe(503);
  expect(logged[0]).toContain("more memory");
});

test("work no request waits on goes on in the object's alarm", async () => {
  // Each call asks the time of day, so a call is always due once one has run.
  const batch = new Encoder().hostRequest(1, "time/unix", new Uint8Array(0)).finish();
  const body = [...op.i32(64), ...op.i32(batch.length), ...op.call(0)];
  const module = new WebAssembly.Module(
    buildModule({ imports: [SUBMIT], memory: 1, ownMemory: true, data: [{ offset: 64, bytes: batch }], funcs: [{ type: SERVE, body, export: "serve" }] }),
  );
  const alarms: number[] = [];
  let now = 1000;
  const s = new Server(module, storage(), { now: () => now, setAlarm: (at) => alarms.push(at), limits: { ...LIMITS, turn: 0 } });
  await s.alarm();
  // The turn is over at once: the alarm, now, goes on with the rest.
  expect(alarms.at(-1)).toBe(1000);
  now = 2000;
  const set = alarms.length;
  await s.alarm();
  expect(alarms.length).toBeGreaterThan(set);
  expect(alarms.at(-1)).toBe(2000);
});

test("what serve_init writes is committed before the first serve", async () => {
  const write = new Out().push(new Uint8Array([1])).chunk(new Uint8Array([7])).chunk(new Uint8Array([8, 9])).bytes();
  const batch = new Encoder().hostRequest(1, "store/write", write).finish();
  const init = [...op.i32(64), ...op.i32(batch.length), ...op.call(0)];
  const module = new WebAssembly.Module(
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
  const store = storage();
  const s = new Server(module, store);
  await s.alarm();
  expect(new Store(store).get(new Uint8Array([7]))).toEqual(new Uint8Array([1, 8, 9]));
});

test("a request past what the object may hold is turned away before it's queued", async () => {
  const module = new WebAssembly.Module(buildModule({ memory: 1, ownMemory: true, funcs: [{ type: SERVE, body: [], export: "serve" }] }));
  const s = new Server(module, storage(), { limits: { ...LIMITS, queued: 16 } });
  expect(await status(s)).toBe(503);
});

test("work a call leaves sets a recovery alarm, and a reset object comes back to it", async () => {
  // `serve` asks the time on its first call alone (its memory's word 0 says it asked), and prints
  // each time it's called.
  const PRINT = { module: "wrela", name: "print", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: [] } };
  const batch = new Encoder().hostRequest(1, "time/unix", new Uint8Array(0)).finish();
  const line = new TextEncoder().encode("served");
  const body = [
    ...op.i32(0), 0x28, 0x02, 0x00, 0x45, 0x04, 0x40, // if memory[0] == 0
    ...op.i32(64), ...op.i32(batch.length), ...op.call(0), // ask the time
    ...op.i32(0), ...op.i32(1), 0x36, 0x02, 0x00, // memory[0] = 1
    0x0b,
    ...op.i32(200), ...op.i32(line.length), ...op.call(1), // print
  ];
  const module = new WebAssembly.Module(
    buildModule({
      imports: [SUBMIT, PRINT],
      memory: 1,
      ownMemory: true,
      data: [{ offset: 64, bytes: batch }, { offset: 200, bytes: line }],
      funcs: [{ type: SERVE, body, export: "serve" }],
    }),
  );
  const store = storage();
  const alarms: number[] = [];
  let printed = 0;
  const now = 5000;
  const first = new Server(module, store, { now: () => now, setAlarm: (at) => alarms.push(at), print: () => printed++ });
  await first.alarm();
  // The first call left an answer due: the recovery alarm, a second ahead, before it was taken.
  expect(alarms).toEqual([6000]);
  expect(printed).toBe(2);
  // The object is reset; its alarm comes to a new one, which starts and calls the program.
  const second = new Server(module, store, { now: () => now, print: () => printed++ });
  await second.alarm();
  expect(printed).toBe(4);
});
