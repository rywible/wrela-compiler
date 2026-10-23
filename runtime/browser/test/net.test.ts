// The browser host's network: its inbox of events and messages' bytes, frames on a stream, and
// connections' events (runtime/browser/src/net.ts).

import { describe, expect, test } from "bun:test";
import { EVENT_SIZE } from "../src/abi.gen.ts";
import { connectionEvent, isNetwork, textEvent } from "../src/input.ts";
import { Frames, frame, HELD, Inbox, MAX_FRAME, MAX_UP, Net, OVERHEAD, PENDING, parseServer, roomName } from "../src/net.ts";

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

  test("a largest frame can arrive one byte at a time", () => {
    const payload = Uint8Array.from({ length: MAX_FRAME }, (_, i) => (37 * i + 11) & 255);
    const bytes = frame(payload);
    const f = new Frames();
    const got: Uint8Array[] = [];
    for (let i = 0; i < bytes.length; i++) got.push(...f.push(bytes.subarray(i, i + 1)));
    expect(got).toEqual([payload]);
  });

  test("frames own their bytes while chunks and later frames change", () => {
    const f = new Frames();
    const first = Uint8Array.of(3, 7);
    expect(f.push(first)).toEqual([]);
    first.fill(0);
    // Finish the first body, then begin a second in the same chunk.
    const next = Uint8Array.of(8, 9, 2, 4);
    const got = f.push(next);
    next.fill(0);
    expect(got).toEqual([Uint8Array.of(7, 8, 9)]);
    expect(f.push(Uint8Array.of(5, 0, 1, 6))).toEqual([Uint8Array.of(4, 5), new Uint8Array(0), Uint8Array.of(6)]);
    expect(got).toEqual([Uint8Array.of(7, 8, 9)]);
    const whole = frame(Uint8Array.of(10, 11));
    const complete = f.push(whole);
    whole.fill(0);
    expect(complete).toEqual([Uint8Array.of(10, 11)]);
  });

  test("split invalid lengths are refused before their body arrives", () => {
    const large = new Frames();
    const prefix = frame(new Uint8Array(MAX_FRAME + 1)).subarray(0, 3);
    expect(large.push(prefix.subarray(0, 1))).toEqual([]);
    expect(large.push(prefix.subarray(1, 2))).toEqual([]);
    expect(() => large.push(prefix.subarray(2))).toThrow("past 65536");
    const long = new Frames();
    expect(long.push(Uint8Array.of(128))).toEqual([]);
    expect(long.push(Uint8Array.of(128))).toEqual([]);
    expect(() => long.push(Uint8Array.of(128))).toThrow("isn't a varint of at most three bytes");
  });

  test("past the largest, or with a length too long, are refused", () => {
    expect(() => new Frames().push(frame(new Uint8Array(MAX_FRAME + 1)).subarray(0, 3))).toThrow("past 65536");
    expect(() => new Frames().push(Uint8Array.of(128, 128, 128, 1))).toThrow("isn't a varint of at most three bytes");
  });

  test("a read with many small frames stays inside the available message budget", () => {
    const f = new Frames();
    expect(f.push(Uint8Array.of(0, 0), OVERHEAD * 2)).toEqual([new Uint8Array(), new Uint8Array()]);
    expect(() => f.push(new Uint8Array(11), OVERHEAD * 10)).toThrow(`more than ${HELD} bytes`);
    // A large read with many tiny frames cannot grow the returned messages past the budget.
    expect(() => new Frames().push(new Uint8Array(MAX_FRAME + OVERHEAD + 4), OVERHEAD)).toThrow(`more than ${HELD} bytes`);
    const partial = new Frames();
    expect(partial.push(Uint8Array.of(3, 7), OVERHEAD + 3)).toEqual([]);
    // Another connection can consume the remaining budget while this frame is incomplete.
    expect(() => partial.push(Uint8Array.of(8, 9), OVERHEAD + 2)).toThrow(`more than ${HELD} bytes`);
  });
});

describe("connections", () => {
  test("empty deliveries keep their queue and later arrivals are delivered once", () => {
    const net = new Net(() => "https://site.example/game/", () => {});
    const inbox = new Inbox();
    const empty = net.arrived;
    net.deliver(inbox);
    expect(net.arrived).toBe(empty);
    expect(inbox.length).toBe(0);
    net.arrived.push({ conn: 4, open: true });
    net.deliver(inbox);
    net.deliver(inbox);
    expect(inbox.events.map((e) => [word(e, 0), word(e, 2), word(e, 3)])).toEqual([[14, 4, 1]]);
  });

  test("a room's name is plain", () => {
    expect(roomName("board-7f3a_1")).toBe(true);
    for (const bad of ["", "../etc", "Big", "a".repeat(65)]) expect(roomName(bad)).toBe(false);
  });

  test("net.json names an https server and, maybe, a certificate's hash", () => {
    expect(parseServer('{"url": "https://localhost:4433"}')).toEqual({ url: "https://localhost:4433" });
    const hash = Array.from({ length: 32 }, (_, i) => i);
    expect(parseServer(JSON.stringify({ url: "https://x", certificateHash: hash })).certificateHash).toEqual(hash);
    for (const url of ["http://x", "https://", "https://trusted.example@attacker.example", "https://x/?redirect=other", "https://x/#room", "https://x/?", "https://x/#"]) {
      expect(() => parseServer(JSON.stringify({ url }))).toThrow("isn't an https URL");
    }
    expect(() => parseServer('{"url": "https://x", "certificateHash": [1]}')).toThrow("isn't 32 bytes");
  });

  test("each open ends in exactly one closed event, however it ends", async () => {
    const lines: string[] = [];
    // Nothing listens on the discard port: the server's address can't be fetched.
    const net = new Net(() => "http://127.0.0.1:9/", (l) => lines.push(l));
    net.open(1, "Not A Room", 1n);
    expect(lines).toEqual(["connection 1: `Not A Room` isn't a room's name"]);
    net.close(1);
    net.open(2, "lobby", 1n);
    expect(() => net.open(2, "lobby", 1n)).toThrow("connection 2 is open");
    // Sending on what isn't open does nothing.
    net.send(3, Uint8Array.of(1));
    const inbox = new Inbox();
    for (let i = 0; i < 200 && net.arrived.length < 2; i++) await new Promise((r) => setTimeout(r, 10));
    net.deliver(inbox);
    expect(inbox.events.map((e) => [word(e, 0), word(e, 2), word(e, 3)])).toEqual([[14, 1, 0], [14, 2, 0]]);
    expect(lines.length).toBe(2);
    // Its number is free again; closing every connection tells the program nothing.
    net.open(2, "lobby", 1n);
    net.closeAll();
    await new Promise((r) => setTimeout(r, 50));
    expect(net.arrived).toEqual([]);
  });

  test("a build of no pair id joins no room", () => {
    const lines: string[] = [];
    const net = new Net(() => "http://127.0.0.1:9/", (l) => lines.push(l));
    net.open(1, "lobby", null);
    expect(lines).toEqual(["connection 1: this build has no pair id, so no server takes it"]);
    expect(net.arrived).toEqual([{ conn: 1, open: false }]);
  });
});

/** Runs `body` with a server whose stream's writes never finish and whose reads are `reads`'s
 * chunks; the URLs the sessions opened. */
async function withServer(reads: Uint8Array[], body: (urls: string[]) => Promise<void>): Promise<void> {
  const urls: string[] = [];
  const saved = { fetch: globalThis.fetch, transport: globalThis.WebTransport };
  globalThis.fetch = (async () => new Response('{"url": "https://localhost:1"}')) as unknown as typeof fetch;
  globalThis.WebTransport = class {
    ready = Promise.resolve();
    closed = new Promise(() => {});
    constructor(url: string) {
      urls.push(url);
    }
    close() {}
    async createBidirectionalStream() {
      const readable = new ReadableStream<Uint8Array>({
        start(c) {
          for (const r of reads) c.enqueue(r);
        },
      });
      return { readable, writable: new WritableStream<Uint8Array>({ write: () => new Promise(() => {}) }) };
    }
  } as unknown as typeof WebTransport;
  try {
    await body(urls);
  } finally {
    globalThis.fetch = saved.fetch;
    globalThis.WebTransport = saved.transport;
  }
}

const settle = async (until: () => boolean) => {
  for (let i = 0; i < 200 && !until(); i++) await new Promise((r) => setTimeout(r, 5));
};

describe("what a connection holds", () => {
  test("a call's network plan checks connection lifetimes without changing the network", async () => {
    await withServer([], async (urls) => {
      const net = new Net(() => "https://site.example/game/", () => {});
      net.open(1, "lobby", 1n);
      await settle(() => net.arrived.length > 0);
      try {
        const plan = net.plan();
        expect(() => plan.open(1, "lobby", 1n)).toThrow("connection 1 is open");
        plan.close(1);
        plan.open(1, "lobby", 1n);
        // A newly opened stream has no writer yet, so sends cannot close it.
        plan.send(1, new Uint8Array(MAX_UP + 1));
        expect(() => plan.open(1, "lobby", 1n)).toThrow("connection 1 is open");
        plan.close(1);
        plan.open(1, "Invalid Room", 1n);
        plan.open(1, "lobby", 1n);
        plan.open(2, "lobby", null);
        plan.open(2, "lobby", 1n);

        const oversized = net.plan();
        oversized.send(1, new Uint8Array(MAX_UP + 1));
        oversized.open(1, "lobby", 1n);
        // A plan starts from the bytes already waiting, and counts its own sends too.
        for (let i = 0; i < 17; i++) net.send(1, new Uint8Array(60_000));
        const queued = net.plan();
        queued.send(1, new Uint8Array(60_000));
        queued.open(1, "lobby", 1n);
        expect(() => net.open(1, "lobby", 1n)).toThrow("connection 1 is open");
        expect(net.arrived).toEqual([{ conn: 1, open: true }]);
        expect(urls).toHaveLength(1);
      } finally { net.closeAll(); }
    });
  });

  test("a later connection retries a server configuration that could not be fetched", async () => {
    await withServer([], async (urls) => {
      let requests = 0;
      globalThis.fetch = (async () => {
        if (++requests === 1) throw new Error("temporarily offline");
        return new Response('{"url":"https://server.example"}');
      }) as unknown as typeof fetch;
      const net = new Net(() => "https://site.example/game/", () => {});
      try {
        net.open(1, "lobby", 1n);
        await settle(() => net.arrived.length > 0);
        expect(net.arrived).toEqual([{ conn: 1, open: false }]);
        net.open(2, "lobby", 1n);
        await settle(() => net.arrived.length > 1);
        expect(net.arrived[1]).toEqual({ conn: 2, open: true });
        expect(requests).toBe(2);
        expect(urls).toEqual(["https://server.example/room/lobby?pair=0000000000000001"]);
      } finally { net.closeAll(); }
    });
  });

  test("a replacement build reads its own server configuration", async () => {
    await withServer([], async (urls) => {
      const fetched: string[] = [];
      const redirects: (RequestRedirect | undefined)[] = [];
      globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
        fetched.push(String(url));
        redirects.push(init?.redirect);
        return new Response(JSON.stringify({ url: fetched.length === 1 ? "https://first.example" : "https://next.example" }));
      }) as typeof fetch;
      let base = "https://site.example/first/";
      const net = new Net(() => base, () => {});
      net.open(1, "lobby", 1n);
      await settle(() => net.arrived.length > 0);
      net.closeAll();
      base = "https://site.example/next/";
      net.open(2, "lobby", 2n);
      await settle(() => net.arrived.length > 0);
      expect(fetched).toEqual(["https://site.example/first/net.json", "https://site.example/next/net.json"]);
      expect(redirects).toEqual(["error", "error"]);
      expect(urls).toEqual(["https://first.example/room/lobby?pair=0000000000000001", "https://next.example/room/lobby?pair=0000000000000002"]);
      net.closeAll();
    });
  });

  test("it gives its build's pair id, and writes waiting past PENDING bytes close it", async () => {
    await withServer([], async (urls) => {
      const lines: string[] = [];
      const net = new Net(() => "http://x/", (l) => lines.push(l));
      net.open(1, "lobby", 0xabcn);
      await settle(() => net.arrived.length > 0);
      expect(urls).toEqual(["https://localhost:1/room/lobby?pair=0000000000000abc"]);
      expect(net.arrived).toEqual([{ conn: 1, open: true }]);
      for (let i = 0; i < 40; i++) net.send(1, new Uint8Array(60_000));
      expect(lines).toEqual([`connection 1: more than ${PENDING} bytes wait to be written`]);
      expect(net.arrived).toEqual([{ conn: 1, open: true }, { conn: 1, open: false }]);
    });
  });

  test("a message past MAX_UP closes its connection: the room's answer to it wouldn't fit a frame", async () => {
    await withServer([], async () => {
      const lines: string[] = [];
      const net = new Net(() => "http://x/", (l) => lines.push(l));
      net.open(1, "lobby", 1n);
      await settle(() => net.arrived.length > 0);
      net.send(1, new Uint8Array(MAX_UP));
      expect(lines).toEqual([]);
      net.send(1, new Uint8Array(MAX_UP + 1));
      expect(lines).toEqual([`connection 1: a message of ${MAX_UP + 1} bytes, past ${MAX_UP}`]);
      expect(net.arrived.at(-1)).toEqual({ conn: 1, open: false });
    });
  });

  test("messages waiting past HELD bytes close their connection, once", async () => {
    // More than HELD bytes of messages arrive, faster than the program reads them.
    const reads = Array.from({ length: 300 }, () => frame(new Uint8Array(60_000)));
    await withServer(reads, async () => {
      const lines: string[] = [];
      const net = new Net(() => "http://x/", (l) => lines.push(l));
      net.open(1, "lobby", 1n);
      await settle(() => net.arrived.some((a) => "open" in a && !a.open));
      expect(lines).toEqual([`connection 1: more than ${HELD} bytes wait for the program`]);
      const inbox = new Inbox();
      net.deliver(inbox);
      expect(inbox.held).toBeLessThanOrEqual(HELD);
      const kinds = inbox.events.map((e) => (word(e, 0) === 14 ? `connection ${word(e, 3)}` : "message"));
      expect(kinds[0]).toBe("connection 1");
      expect(kinds.at(-1)).toBe("connection 0");
      expect(kinds.filter((k) => k.startsWith("connection")).length).toBe(2);
    });
  });

  test("an inbox past HELD bytes closes the connection its messages come from", async () => {
    await withServer([], async () => {
      const lines: string[] = [];
      const net = new Net(() => "http://x/", (l) => lines.push(l));
      net.open(1, "lobby", 1n);
      await settle(() => net.arrived.length > 0);
      for (let i = 0; i < 300; i++) net.arrived.push({ conn: 1, bytes: new Uint8Array(60_000) });
      const inbox = new Inbox();
      net.deliver(inbox);
      expect(inbox.held).toBeLessThanOrEqual(HELD);
      expect(lines).toEqual([`connection 1: more than ${HELD} bytes wait for the program`]);
      expect(net.arrived).toEqual([{ conn: 1, open: false }]);
      // What the program reads gives its bytes back.
      while (inbox.length > 0) {
        inbox.take(64);
        inbox.endCall();
      }
      expect(inbox.held).toBe(0);
    });
  });
});
