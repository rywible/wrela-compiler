// The server's instance on hand-built modules: a trap answers the requests open on it with 500,
// a host limit (the memory cap) with 503, and the next request starts a fresh instance; a module
// that isn't a server build is the host's failure.

import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { Encoder } from "../../browser/test/encoder.ts";
import { buildModule, op, SUBMIT } from "../../browser/test/wasm-builder.ts";
import { decodeResponse, encodeRequest, Server, type Storage } from "../src/host.ts";

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
  const s = new Server(module, storage(), { setAlarm: (at) => alarms.push(at), limits: { memory: 64 << 20, commands: 1 << 20, turn: 0 } });
  await s.alarm();
  expect(alarms.length).toBe(1);
  await s.alarm();
  expect(alarms.length).toBe(2);
});
