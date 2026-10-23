// The network (language.md §19): the events a program reads, with messages' bytes, and its
// connections to its server's rooms over WebTransport. A connection is one session at
// `/room/<name>?pair=<id>` of the server that `net.json` beside the build names, with one
// bidirectional stream; each message on it is a frame: its length (a varint, at most MAX_FRAME),
// then its bytes. What a connection holds is bounded: past HELD bytes waiting for the program,
// or past PENDING bytes waiting to be written, it's closed, and its peer comes back.

import { EVENT_SIZE, EventKind, PAYLOAD_PER_CALL } from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import { connectionEvent, isNetwork, messageEvent } from "./input.ts";

/** The largest frame, in bytes: as the server host's (`wrela-serve`). */
export const MAX_FRAME = 64 * 1024;
/** The largest message a client sends: a room's answer adds a header to it, and must still fit
 * a frame. */
export const MAX_UP = MAX_FRAME - 64;
/** The bytes a message is counted as past its own: what holding it costs. */
export const OVERHEAD = 64;
/** The bytes messages may hold waiting for the program: in what arrived, and in an inbox. */
export const HELD = 16 << 20;
/** The bytes a connection's messages may hold waiting to be written. */
export const PENDING = 1 << 20;

/**
 * The events a program hasn't read, as the native host's `Queue` keeps them (runtime/abi
 * `input`): a message's bytes are kept while its event is queued and during the call given it,
 * then go; a serial is never given twice, so an old event's reads as unknown.
 */
export class Inbox {
  readonly #events: Uint8Array[] = [];
  readonly #payloads = new Map<number, Uint8Array>();
  /** The serials of the messages the current call was given. */
  #delivered: number[] = [];
  #serial = 0;
  /** The bytes its messages hold, each counted with OVERHEAD more. */
  #held = 0;
  /** The pair id of the build it serves, once it serves one. */
  #pair: bigint | null | undefined = undefined;

  /** Queues an event that isn't a message. */
  push(event: Uint8Array): void {
    this.#events.push(event);
  }

  /** Queues a message of `bytes` from connection `conn`: its serial. */
  pushMessage(conn: number, bytes: Uint8Array): number {
    const serial = ++this.#serial;
    this.#events.push(messageEvent(conn, serial, bytes.length));
    this.#payloads.set(serial, bytes);
    this.#held += bytes.length + OVERHEAD;
    return serial;
  }

  /** The bytes its messages hold, each counted with OVERHEAD more. */
  get held(): number {
    return this.#held;
  }

  get length(): number {
    return this.#events.length;
  }

  /** The events not read, oldest first. */
  get events(): readonly Uint8Array[] {
    return this.#events;
  }

  /** Up to `cap` events, oldest first; the rest stay queued. Messages stop once the call's bytes
   * reach PAYLOAD_PER_CALL (the first is given whatever its size). */
  take(cap: number): Uint8Array[] {
    let given = 0;
    for (const s of this.#delivered) given += this.#payloads.get(s)?.length ?? 0;
    let n = 0;
    for (; n < cap && n < this.#events.length; n++) {
      const view = new DataView(this.#events[n]!.buffer, this.#events[n]!.byteOffset, EVENT_SIZE);
      if (view.getUint32(0, true) !== EventKind.Message) continue;
      const length = view.getUint32(20, true);
      if (given > 0 && given + length > PAYLOAD_PER_CALL) break;
      given += length;
      // Words 3 and 4: the serial's low and high words.
      this.#delivered.push(view.getUint32(12, true) + view.getUint32(16, true) * 2 ** 32);
    }
    return this.#events.splice(0, n);
  }

  /** The bytes of message `serial`, if the current call was given it. */
  payload(serial: number): Uint8Array | undefined {
    return this.#delivered.includes(serial) ? this.#payloads.get(serial) : undefined;
  }

  /** The call has returned (or trapped): the bytes of the messages it was given go. */
  endCall(): void {
    for (const s of this.#delivered) this.#drop(s);
    this.#delivered = [];
  }

  #drop(serial: number): void {
    const bytes = this.#payloads.get(serial);
    if (bytes === undefined) return;
    this.#held -= bytes.length + OVERHEAD;
    this.#payloads.delete(serial);
  }

  /** It now serves the build whose pair id is `pair` (null: a build of no pair): if it served
   * another program's build, its network events go, as their bytes were for that build's types.
   * Whether they went. */
  forBuild(pair: bigint | null): boolean {
    const changed = this.#pair !== undefined && this.#pair !== pair;
    if (changed) {
      for (let i = this.#events.length - 1; i >= 0; i--) if (isNetwork(this.#events[i]!)) this.#events.splice(i, 1);
      this.#delivered = [];
      this.#payloads.clear();
      this.#held = 0;
    }
    this.#pair = pair;
    return changed;
  }
}

/** Whether `name` names a room: 1 to 64 of a-z, 0-9, `-` and `_`, as the server takes them. */
export const roomName = (name: string) => /^[a-z0-9_-]{1,64}$/.test(name);

/** A frame: `bytes`' length, then them. */
export function frame(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(bytes.length + 3);
  let at = 0;
  for (let x = bytes.length; ; ) {
    const group = x & 127;
    x >>>= 7;
    out[at++] = x === 0 ? group : group | 128;
    if (x === 0) break;
  }
  out.set(bytes, at);
  return out.subarray(0, at + bytes.length);
}

/** Reads frames from a stream's chunks, however they're split. */
export class Frames {
  #length = 0;
  #prefix = 0;
  #body: Uint8Array | null = null;
  #used = 0;

  /** The whole frames `chunk` completes, oldest first. `available` bounds the messages returned
   * by this read, including their overhead. Each frame owns only its own bytes. */
  push(chunk: Uint8Array, available = HELD): Uint8Array[] {
    const out: Uint8Array[] = [];
    let at = 0;
    let held = 0;
    while (at < chunk.length) {
      if (this.#body === null) {
        const byte = chunk[at++]!;
        this.#length |= (byte & 127) << (7 * this.#prefix++);
        if (byte >= 128) {
          if (this.#prefix === 3) throw new Error("a frame's length isn't a varint of at most three bytes");
          continue;
        }
        if (this.#length > MAX_FRAME) throw new Error(`a frame of ${this.#length} bytes, past ${MAX_FRAME}`);
        if (held + this.#length + OVERHEAD > available) throw new Error(`more than ${HELD} bytes wait for the program`);
        // Allocate once per frame: each chunk copies only the bytes it adds, and a delivered
        // frame keeps its own storage when the stream reuses its chunk or the next frame starts.
        this.#body = new Uint8Array(this.#length);
        this.#length = 0;
        this.#prefix = 0;
        this.#used = 0;
      }
      const body = this.#body;
      // Other connections may have used the budget since this frame's prefix arrived.
      if (held + body.length + OVERHEAD > available) throw new Error(`more than ${HELD} bytes wait for the program`);
      const n = Math.min(body.length - this.#used, chunk.length - at);
      body.set(chunk.subarray(at, at + n), this.#used);
      this.#used += n;
      at += n;
      if (this.#used === body.length) {
        held += body.length + OVERHEAD;
        out.push(body);
        this.#body = null;
      }
    }
    return out;
  }
}

/** Where a build's server is (`net.json` beside it): its address, and a development
 * certificate's SHA-256 hash, which the browser takes in place of a trusted one. */
export interface Server {
  url: string;
  certificateHash?: number[];
}

/** What `net.json` says, checked. */
export function parseServer(text: string): Server {
  const v: unknown = JSON.parse(text);
  if (typeof v !== "object" || v === null) throw new Error("net.json isn't an object");
  const { url, certificateHash } = v as Record<string, unknown>;
  if (typeof url !== "string") throw new Error("net.json's `url` isn't an https URL");
  let address: URL;
  try { address = new URL(url); }
  catch { throw new Error("net.json's `url` isn't an https URL"); }
  if (address.protocol !== "https:" || address.username || address.password || url.includes("?") || url.includes("#")) {
    throw new Error("net.json's `url` isn't an https URL without credentials, query or fragment");
  }
  if (certificateHash === undefined) return { url };
  const whole = Array.isArray(certificateHash) && certificateHash.length === 32 && certificateHash.every((x) => Number.isInteger(x) && x >= 0 && x < 256);
  if (!whole) throw new Error("net.json's `certificateHash` isn't 32 bytes");
  return { url, certificateHash: certificateHash as number[] };
}

/** One connection: its session and its stream's writer, once it's open, and the bytes waiting
 * to be written on it. */
interface Session {
  transport: WebTransport | null;
  writer: WritableStreamDefaultWriter<Uint8Array> | null;
  closed: boolean;
  pending: number;
}

/** The pair id in a session's path: 16 hex digits. */
export const pairParam = (pair: bigint) => pair.toString(16).padStart(16, "0");

/**
 * A program's connections (`std::net`'s `open`, `send`, `close`). Each opened connection
 * gives exactly one `Connection` event that it's open (if it opens), then exactly one that it's
 * closed, whatever closes it; its messages come between. What arrives waits in `arrived` for the
 * frame that takes it.
 */
export class Net {
  readonly #sessions = new Map<number, Session>();
  #server: { url: string; ready: Promise<Server> } | null = null;
  /** What arrived: a message's bytes, or a connection's opening or closing. */
  arrived: ({ conn: number; bytes: Uint8Array } | { conn: number; open: boolean })[] = [];
  /** The bytes the messages in `arrived` hold, each counted with OVERHEAD more. */
  #held = 0;

  /** `base`: where `net.json` is; `print`: a line for whoever runs the program. */
  constructor(
    readonly base: () => string,
    readonly print: (line: string) => void,
  ) {}

  /** Checks one export's commands without opening, sending or closing anything. A send can
   * close a connection at its byte limits, so later opens see that same planned lifetime. */
  plan(): Pick<Net, "open" | "send" | "close"> {
    const sessions = new Map<number, { open: boolean; writer: boolean; pending: number }>();
    const state = (conn: number) => {
      let planned = sessions.get(conn);
      if (planned === undefined) {
        const session = this.#sessions.get(conn);
        planned = { open: session !== undefined && !session.closed, writer: session?.writer != null, pending: session?.pending ?? 0 };
        sessions.set(conn, planned);
      }
      return planned;
    };
    return {
      open(conn, room, pair) {
        const session = state(conn);
        if (session.open) throw new Error(`connection ${conn} is open`);
        session.open = roomName(room) && pair !== null;
        session.writer = false;
        session.pending = 0;
      },
      send(conn, bytes) {
        const session = state(conn);
        if (!session.open || !session.writer) return;
        // The frame's varint prefix is one, two or three bytes (MAX_UP is below 2^21).
        const cost = bytes.length + (bytes.length < 128 ? 1 : bytes.length < 16384 ? 2 : 3) + OVERHEAD;
        if (bytes.length > MAX_UP || session.pending + cost > PENDING) session.open = false;
        else session.pending += cost;
      },
      close(conn) { state(conn).open = false; },
    };
  }

  #fetchServer(): Promise<Server> {
    const url = new URL("net.json", this.base()).href;
    if (this.#server?.url === url) return this.#server.ready;
    const ready = fetch(url, { redirect: "error" })
      .then((r) => {
        if (!r.ok) throw new Error(`net.json: ${r.status}`);
        return r.text();
      })
      .then(parseServer);
    const server = { url, ready };
    this.#server = server;
    void ready.catch(() => {
      // A temporary fetch failure must not make every later connection fail with it.
      if (this.#server === server) this.#server = null;
    });
    return ready;
  }

  /** Opens connection `conn` to room `room`, for the build of pair id `pair` (a build of none
   * can't join a room). A number already open is the program's mistake. */
  open(conn: number, room: string, pair: bigint | null): void {
    if (this.#sessions.has(conn)) throw new Error(`connection ${conn} is open`);
    const session: Session = { transport: null, writer: null, closed: false, pending: 0 };
    this.#sessions.set(conn, session);
    if (!roomName(room)) {
      this.print(`connection ${conn}: \`${room}\` isn't a room's name`);
      this.#end(conn, session);
      return;
    }
    if (pair === null) {
      this.print(`connection ${conn}: this build has no pair id, so no server takes it`);
      this.#end(conn, session);
      return;
    }
    void this.#connect(conn, `${room}?pair=${pairParam(pair)}`, session);
  }

  async #connect(conn: number, room: string, session: Session): Promise<void> {
    try {
      const server = await this.#fetchServer();
      if (session.closed) return;
      const options: WebTransportOptions = {};
      if (server.certificateHash) {
        options.serverCertificateHashes = [{ algorithm: "sha-256", value: new Uint8Array(server.certificateHash) }];
      }
      const transport = new WebTransport(`${server.url.replace(/\/$/, "")}/room/${room}`, options);
      session.transport = transport;
      // Its end, however it comes, ends the connection.
      transport.closed.then(
        () => this.#end(conn, session),
        () => this.#end(conn, session),
      );
      await transport.ready;
      const stream = await transport.createBidirectionalStream();
      if (session.closed) return;
      session.writer = stream.writable.getWriter();
      this.arrived.push({ conn, open: true });
      const reader = stream.readable.getReader();
      const frames = new Frames();
      for (;;) {
        const { value, done } = await reader.read();
        if (done || session.closed) break;
        for (const bytes of frames.push(value, HELD - this.#held)) {
          this.#held += bytes.length + OVERHEAD;
          this.arrived.push({ conn, bytes });
        }
      }
    } catch (e) {
      if (!session.closed) this.print(`connection ${conn}: ${errorMessage(e)}`);
    }
    this.#end(conn, session);
  }

  /** Sends `bytes` on connection `conn`: nothing if it isn't open (it may have closed since the
   * program last heard). A message past MAX_UP closes it, and so does one past PENDING bytes
   * not yet written. */
  send(conn: number, bytes: Uint8Array): void {
    const session = this.#sessions.get(conn);
    if (!session?.writer || session.closed) return;
    if (bytes.length > MAX_UP) {
      this.print(`connection ${conn}: a message of ${bytes.length} bytes, past ${MAX_UP}`);
      this.close(conn);
      return;
    }
    const framed = frame(bytes);
    const cost = framed.length + OVERHEAD;
    if (session.pending + cost > PENDING) {
      this.print(`connection ${conn}: more than ${PENDING} bytes wait to be written`);
      this.close(conn);
      return;
    }
    session.pending += cost;
    session.writer.write(framed).then(
      () => (session.pending -= cost),
      () => this.#end(conn, session),
    );
  }

  /** Closes connection `conn`. */
  close(conn: number): void {
    const session = this.#sessions.get(conn);
    if (session) this.#end(conn, session);
  }

  /** Closes every connection, giving no events: the build changed, and its program goes. */
  closeAll(): void {
    for (const [conn, session] of [...this.#sessions]) this.#end(conn, session, false);
    this.arrived = [];
    this.#held = 0;
  }

  /** Ends a connection once: its session closes, and the program hears (if `tell`). */
  #end(conn: number, session: Session, tell = true): void {
    if (session.closed) return;
    session.closed = true;
    if (this.#sessions.get(conn) === session) this.#sessions.delete(conn);
    try {
      session.transport?.close();
    } catch {
      // It's closed already.
    }
    if (tell) this.arrived.push({ conn, open: false });
  }

  /** Queues what arrived since the last call into `inbox`. A message past what the inbox may
   * hold (HELD) closes its connection: its later messages go, and its closing comes next. */
  deliver(inbox: Inbox): void {
    const arrived = this.arrived;
    if (arrived.length === 0) return;
    this.arrived = [];
    this.#held = 0;
    const full = new Set<number>();
    for (const a of arrived) {
      if (!("bytes" in a)) {
        inbox.push(connectionEvent(a.conn, a.open));
      } else if (full.has(a.conn)) {
        continue;
      } else if (inbox.held + a.bytes.length + OVERHEAD > HELD) {
        full.add(a.conn);
        this.print(`connection ${a.conn}: more than ${HELD} bytes wait for the program`);
        this.close(a.conn);
      } else {
        inbox.pushMessage(a.conn, a.bytes);
      }
    }
  }
}
