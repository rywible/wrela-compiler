// A room's host on hand-built modules: what a peer may send, and a room whose program fails on
// every message is quarantined, and stays so in its storage.

import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { buildModule, op } from "../../browser/test/wasm-builder.ts";
import { MAX_UP, Room, type Socket } from "../src/room.ts";
import { sqliteBindings } from "./sqlite.ts";

const INPUT = { module: "wrela", name: "input", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: ["i32" as const] } };
const SERVE = { params: ["f32" as const], results: [] };

function storage(db = new Database(":memory:")) {
  return {
    db,
    sql: {
      exec(q: string, ...b: unknown[]) {
        const rows = db.query(q).all(...(sqliteBindings(b) as never[])) as Record<string, unknown>[];
        return Object.assign(rows[Symbol.iterator](), { toArray: () => rows });
      },
    },
    transactionSync: <T>(fn: () => T): T => db.transaction(fn)(),
    setAlarm: () => {},
  };
}

class FakeSocket implements Socket {
  sent: Uint8Array[] = [];
  closed: number | null = null;
  attachment: unknown = null;
  send(b: Uint8Array) {
    this.sent.push(b);
  }
  close(code = 1000) {
    this.closed ??= code;
  }
  serializeAttachment(v: unknown) {
    this.attachment = v;
  }
  deserializeAttachment() {
    return this.attachment;
  }
}

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
