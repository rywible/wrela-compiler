// The browser host's network: its inbox of events and messages' bytes, frames on a stream, and
// connections' events (runtime/browser/src/net.ts).

import { describe, expect, test } from "bun:test";
import { EVENT_SIZE } from "../src/abi.gen.ts";
import { connectionEvent, isNetwork, textEvent } from "../src/input.ts";
import { Frames, frame, Inbox, MAX_FRAME, Net, parseServer, roomName } from "../src/net.ts";

const word = (e: Uint8Array, i: number) => new DataView(e.buffer, e.byteOffset, EVENT_SIZE).getUint32(4 * i, true);

describe("the inbox", () => {
  test("keeps a message's bytes while it's queued and during the call given it", () => {
    const inbox = new Inbox();
    const serial = inbox.pushMessage(4, Uint8Array.of(1, 2));
    expect(inbox.payload(serial)).toBeUndefined();
    const [e] = inbox.take(8);
    expect([word(e!, 0), word(e!, 2), word(e!, 3), word(e!, 5)]).toEqual([13, 4, serial, 2]);
    expect(inbox.payload(serial)).toEqual(Uint8Array.of(1, 2));
    inbox.endCall();
    expect(inbox.payload(serial)).toBeUndefined();
    // A serial isn't given twice.
    expect(inbox.pushMessage(4, Uint8Array.of(3))).toBe(serial + 1);
  });

  test("drops network events when it serves another program's build, and keeps the rest", () => {
    const inbox = new Inbox();
    expect(inbox.forBuild(1n)).toBe(false);
    inbox.push(textEvent(65));
    inbox.pushMessage(1, Uint8Array.of(1));
    inbox.push(connectionEvent(1, false));
    expect(inbox.forBuild(1n)).toBe(false);
    expect(inbox.length).toBe(3);
    expect(inbox.forBuild(2n)).toBe(true);
    expect(inbox.events.map(isNetwork)).toEqual([false]);
  });
});

describe("frames", () => {
  test("are a varint length, then the bytes", () => {
    expect([...frame(Uint8Array.of(7, 8))]).toEqual([2, 7, 8]);
    expect([...frame(new Uint8Array(300)).subarray(0, 2)]).toEqual([0xac, 0x02]);
  });

  test("are read however the stream splits them", () => {
    const bytes = new Uint8Array([...frame(new Uint8Array(200).fill(5)), ...frame(Uint8Array.of(1)), ...frame(new Uint8Array(0))]);
    for (const cut of [1, 2, 3, 150, 203, 205]) {
      const f = new Frames();
      const got = [...f.push(bytes.subarray(0, cut)), ...f.push(bytes.subarray(cut))];
      expect(got.map((g) => g.length)).toEqual([200, 1, 0]);
    }
  });

  test("past the largest, or with a length too long, are refused", () => {
    expect(() => new Frames().push(frame(new Uint8Array(MAX_FRAME + 1)).subarray(0, 3))).toThrow("past 65536");
    expect(() => new Frames().push(Uint8Array.of(128, 128, 128, 1))).toThrow("isn't a varint of at most three bytes");
  });
});

describe("connections", () => {
  test("a room's name is plain", () => {
    expect(roomName("board-7f3a_1")).toBe(true);
    for (const bad of ["", "../etc", "Big", "a".repeat(65)]) expect(roomName(bad)).toBe(false);
  });

  test("net.json names an https server and, maybe, a certificate's hash", () => {
    expect(parseServer('{"url": "https://localhost:4433"}')).toEqual({ url: "https://localhost:4433" });
    const hash = Array.from({ length: 32 }, (_, i) => i);
    expect(parseServer(JSON.stringify({ url: "https://x", certificateHash: hash })).certificateHash).toEqual(hash);
    expect(() => parseServer('{"url": "http://x"}')).toThrow("isn't an https URL");
    expect(() => parseServer('{"url": "https://x", "certificateHash": [1]}')).toThrow("isn't 32 bytes");
  });

  test("each open ends in exactly one closed event, however it ends", async () => {
    const lines: string[] = [];
    // Nothing listens on the discard port: the server's address can't be fetched.
    const net = new Net(() => "http://127.0.0.1:9/", (l) => lines.push(l));
    net.open(1, "Not A Room");
    expect(lines).toEqual(["connection 1: `Not A Room` isn't a room's name"]);
    net.close(1);
    net.open(2, "lobby");
    expect(() => net.open(2, "lobby")).toThrow("connection 2 is open");
    // Sending on what isn't open does nothing.
    net.send(3, Uint8Array.of(1));
    const inbox = new Inbox();
    for (let i = 0; i < 200 && net.arrived.length < 2; i++) await new Promise((r) => setTimeout(r, 10));
    net.deliver(inbox);
    expect(inbox.events.map((e) => [word(e, 0), word(e, 2), word(e, 3)])).toEqual([[14, 1, 0], [14, 2, 0]]);
    expect(lines.length).toBe(2);
    // Its number is free again; closing every connection tells the program nothing.
    net.open(2, "lobby");
    net.closeAll();
    await new Promise((r) => setTimeout(r, 50));
    expect(net.arrived).toEqual([]);
  });
});
