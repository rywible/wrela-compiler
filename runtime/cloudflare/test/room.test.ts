// A room's host on hand-built modules: what a peer may send and hold waiting, and all its peers, a
// room whose program fails on every message quarantined for good, a socket that fails after a
// call is settled, the wait for the alarm after a failure at the host's limits, a turn's end, and
// a room whose storage is another build's.

import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { PAIR_SECTION } from "../../browser/src/abi.gen.ts";
import { MAX_UP, OVERHEAD } from "../../browser/src/net.ts";
import { Encoder } from "../../browser/test/encoder.ts";
import { buildModule, op, SUBMIT } from "../../browser/test/wasm-builder.ts";
import { LIMITS } from "../src/host.ts";
import { CONN_IN, Room } from "../src/room.ts";
import { FakeSocket, storage } from "./fakes.ts";

const INPUT = { module: "wrela", name: "input", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: ["i32" as const] } };
const SERVE = { params: ["f32" as const], results: [] };

const store = { answer: async () => null, commit: async () => {} };

/** A program that reads every event it's given and, with `trap`, traps if one of the first three
 * is a message: a fresh instance hears its open connections first, and a new one again in its
 * `opened` (`std::net` takes that as one). */
function program(trap = false): WebAssembly.Module {
  const SLOTS = [1024, 1048, 1072];
  const clear = SLOTS.flatMap((at) => [...op.i32(at), ...op.i32(0), 0x36, 0x02, 0x00]);
  const isMessage = (at: number) => [...op.i32(at), 0x28, 0x02, 0x00, ...op.i32(13), 0x46];
  const read = [...clear, ...op.i32(1024), ...op.i32(64), ...op.call(0), 0x1a];
  const check = trap ? [...SLOTS.flatMap(isMessage), 0x72, 0x72, 0x04, 0x40, ...op.unreachable, 0x0b] : [];
  return new WebAssembly.Module(buildModule({ imports: [INPUT], memory: 1, ownMemory: true, funcs: [{ type: SERVE, body: [...read, ...check], export: "serve" }] }));
}

test("a peer's text message, or one past MAX_UP, closes its connection", async () => {
  const room = new Room(program(), storage(), store, []);
  const a = new FakeSocket();
  const b = new FakeSocket();
  room.connect(a);
  room.connect(b);
  room.message(a, "text");
  room.message(b, new ArrayBuffer(MAX_UP + 1));
  await room.wake();
  expect([a.closed, b.closed]).toEqual([1003, 1009]);
  expect(a.attachment).toEqual({ conn: 1, tag: 0 });
  expect(b.attachment).toEqual({ conn: 2, tag: 0 });
});

test("a message the program traps on, then on poisoned, quarantines the room for good", async () => {
  const s = storage();
  const logs: string[] = [];
  const room = new Room(program(true), s, store, [], { log: (l) => logs.push(l) });
  const a = new FakeSocket();
  room.connect(a);
  room.message(a, new Uint8Array([1]).buffer);
  await room.wake();
  expect(a.closed).toBe(1011);
  expect(logs).toEqual([expect.stringContaining("the room is quarantined: a poisoned message traps it")]);
  // A new object over the same storage stays closed.
  const again = new Room(program(true), s, store, []);
  const b = new FakeSocket();
  again.connect(b);
  expect(b.closed).toBe(1011);
});

test("a peer's empty messages count toward what may wait for the room", () => {
  const room = new Room(program(), storage(), store, []);
  const a = new FakeSocket();
  room.connect(a);
  for (let i = 0; i < CONN_IN / OVERHEAD; i++) room.message(a, new ArrayBuffer(0));
  expect(a.closed).toBeNull();
  room.message(a, new ArrayBuffer(0));
  expect(a.closed).toBe(1008);
});

/** Each call reads its events, keeps a record and sends a byte to connection 1. */
function keepAndSend(): WebAssembly.Module {
  const batch = new Encoder()
    .hostRequest(1, "net/keep", new Uint8Array([7]))
    .hostRequest(2, "net/send", new Uint8Array([1, 0, 0, 0, 9]))
    .finish();
  const body = [...op.i32(1024), ...op.i32(64), ...op.call(0), 0x1a, ...op.i32(64), ...op.i32(batch.length), ...op.call(1)];
  return new WebAssembly.Module(
    buildModule({ imports: [INPUT, SUBMIT], memory: 1, ownMemory: true, data: [{ offset: 64, bytes: batch }], funcs: [{ type: SERVE, body, export: "serve" }] }),
  );
}

test("a socket that fails once a call is settled closes, and the call isn't taken again", async () => {
  const db = new Database(":memory:");
  const logs: string[] = [];
  const room = new Room(keepAndSend(), storage(db), store, [], { log: (l) => logs.push(l) });
  const a = new FakeSocket();
  a.send = () => {
    throw new Error("the socket is closing");
  };
  room.connect(a);
  await room.settled;
  expect(a.closed).toBe(1011);
  expect(logs).toEqual([]);
  // One record for the call that opened it, one for the call that heard it closed.
  expect(db.query("SELECT count(*) AS n FROM journal").get()).toEqual({ n: 2 });
});

test("after a failure at the host's limits, the room waits for its alarm", async () => {
  // Each call grows its memory past the cap: the host's limits.
  const body = [...op.i32(2), 0x40, 0x00, 0x1a];
  const module = new WebAssembly.Module(buildModule({ memory: 1, ownMemory: true, funcs: [{ type: SERVE, body, export: "serve" }] }));
  const alarms: number[] = [];
  const room = new Room(module, storage(new Database(":memory:"), (at) => alarms.push(at)), store, [], {
    now: () => 1000,
    limits: { ...LIMITS, memory: 2 * 65536 },
  });
  const a = new FakeSocket();
  room.connect(a);
  room.message(a, new Uint8Array([1]).buffer);
  room.message(a, new Uint8Array([2]).buffer);
  await room.settled;
  expect([a.closed, alarms]).toEqual([null, [1250]]);
  await room.alarm();
  expect(a.closed).toBeNull();
  // The third failure in a row lets its peers go.
  await room.alarm();
  expect(a.closed).toBe(1013);
});

test("a room whose storage is another build's reads nothing back and closes its sockets", () => {
  const db = new Database(":memory:");
  const s = storage(db);
  const first = new Room(program(), s, store, []);
  const a = new FakeSocket();
  first.connect(a);
  db.query("INSERT INTO journal (record) VALUES (?)").run(new Uint8Array([5]));
  // The same object after a deploy: the new build's code (another pair id), the old sockets.
  const name = new TextEncoder().encode(PAIR_SECTION);
  const custom = [0, 1 + name.length + 8, name.length, ...name, 1, 2, 3, 4, 5, 6, 7, 8];
  const paired = new WebAssembly.Module(new Uint8Array([...buildModule({ memory: 1, ownMemory: true, funcs: [{ type: SERVE, body: [], export: "serve" }] }), ...custom]));
  const again = new Room(paired, s, store, [a]);
  expect(a.closed).toBe(1012);
  const b = new FakeSocket();
  again.connect(b);
  expect(b.closed).toBe(1012);
  expect(db.query("SELECT count(*) AS n FROM journal").get()).toEqual({ n: 1 });
});

test("what all a room's peers hold waiting is bounded too", () => {
  const room = new Room(program(), storage(), store, [], { limits: { ...LIMITS, queued: 1000 } });
  const a = new FakeSocket();
  const b = new FakeSocket();
  room.connect(a);
  room.connect(b);
  room.message(a, new ArrayBuffer(900));
  room.message(b, new ArrayBuffer(100));
  expect([a.closed, b.closed]).toEqual([null, 1008]);
});

test("past a turn, a room goes on in its alarm", async () => {
  const alarms: number[] = [];
  const logs: string[] = [];
  const room = new Room(program(), storage(new Database(":memory:"), (at) => alarms.push(at)), store, [], {
    now: () => 1000,
    limits: { ...LIMITS, turn: -1 },
    log: (l) => logs.push(l),
  });
  const a = new FakeSocket();
  const b = new FakeSocket();
  room.connect(a);
  room.connect(b);
  // 1.2 MiB of messages: more than a call is given, so two calls.
  for (const peer of [a, b]) for (let i = 0; i < 10; i++) room.message(peer, new ArrayBuffer(60 * 1024));
  await room.settled;
  expect(alarms).toEqual([1000]);
  expect((room as unknown as { incoming: unknown[] }).incoming.length).toBeGreaterThan(0);
  await room.alarm();
  expect((room as unknown as { incoming: unknown[] }).incoming.length).toBe(0);
  expect([alarms, logs, a.closed, b.closed]).toEqual([[1000], [], null, null]);
});
