// The network (language.md §19): the events a program reads, with messages' bytes, and its
// connections to its server's rooms. A connection is to `/room/<name>?pair=<id>` of the server
// that `net.json` beside the build names. To an `https:` server (`wrela-serve`) it's a
// WebTransport session with one bidirectional stream, each message on it a frame: its length (a
// varint, at most MAX_FRAME), then its bytes. To a `wss:` server (the Cloudflare host) it's a
// WebSocket, each binary message a frame. What a connection holds is bounded: past HELD bytes
// waiting for the program, or past PENDING bytes waiting to be written, it's closed, and its peer
// comes back.

import { EVENT_SIZE, EventKind, PAIR_SECTION, PAYLOAD_PER_CALL } from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import { connectionEvent, isNetwork, messageEvent } from "./input.ts";
import { type Channel, type Control, fromPeer, MAX_RELAY, parseControl, RELAY_PENDING, type Rtc } from "./relay.ts";

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
  /** The serials of the messages the current call was given, and their bytes. */
  #delivered = new Set<number>();
  #given = 0;
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
    let n = 0;
    for (; n < cap && n < this.#events.length; n++) {
      const view = new DataView(this.#events[n]!.buffer, this.#events[n]!.byteOffset, EVENT_SIZE);
      if (view.getUint32(0, true) !== EventKind.Message) continue;
      const length = view.getUint32(20, true);
      if (!withinCall(this.#given, length)) break;
      this.#given += length;
      // Words 3 and 4: the serial's low and high words.
      this.#delivered.add(view.getUint32(12, true) + view.getUint32(16, true) * 2 ** 32);
    }
    return this.#events.splice(0, n);
  }

  /** The bytes of message `serial`, if the current call was given it. */
  payload(serial: number): Uint8Array | undefined {
    return this.#delivered.has(serial) ? this.#payloads.get(serial) : undefined;
  }

  /** The call has returned (or trapped): the bytes of the messages it was given go. */
  endCall(): void {
    for (const s of this.#delivered) this.#drop(s);
    this.#delivered.clear();
    this.#given = 0;
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
      this.#delivered.clear();
      this.#given = 0;
      this.#payloads.clear();
      this.#held = 0;
    }
    this.#pair = pair;
    return changed;
  }
}

/** Whether a call given `given` bytes of messages may be given a message of `length` more: the
 * first, whatever its size, and then up to PAYLOAD_PER_CALL (`wrela_abi::input::within_call`). */
export const withinCall = (given: number, length: number) => given === 0 || given + length <= PAYLOAD_PER_CALL;

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
      // Within the budget, which other connections may have used since this frame's prefix
      // arrived.
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
  if (typeof url !== "string") throw new Error("net.json's `url` isn't an https or wss URL");
  let address: URL;
  try { address = new URL(url); }
  catch { throw new Error("net.json's `url` isn't an https or wss URL"); }
  // `https:`: WebTransport (wrela-serve); `wss:`: a WebSocket a connection (the Cloudflare host),
  // and `ws:` only to a loopback development server.
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(address.hostname);
  const scheme = address.protocol === "https:" || address.protocol === "wss:" || (address.protocol === "ws:" && loopback);
  if (!scheme || address.username || address.password || url.includes("?") || url.includes("#")) {
    throw new Error("net.json's `url` isn't an https or wss URL without credentials, query or fragment");
  }
  if (certificateHash === undefined) return { url };
  if (address.protocol !== "https:") throw new Error("net.json's `certificateHash` is for an https (WebTransport) server");
  const whole = Array.isArray(certificateHash) && certificateHash.length === 32 && certificateHash.every((x) => Number.isInteger(x) && x >= 0 && x < 256);
  if (!whole) throw new Error("net.json's `certificateHash` isn't 32 bytes");
  return { url, certificateHash: certificateHash as number[] };
}

/** One connection: its session and its stream's writer, once it's open, and the bytes waiting
 * to be written on it. */
interface Session {
  transport: WebTransport | null;
  socket: WebSocket | null;
  writer: WritableStreamDefaultWriter<Uint8Array> | null;
  closed: boolean;
  pending: number;
  /** The relay that goes by it (`std::net::relay`), if one does. */
  relay: number | null;
}

/** A relay: the connection it goes by, the channel it sends on once the room gave it, and the
 * channels it hears each other peer on. */
interface RelaySession {
  via: number;
  /** Its number with the page's WebRTC, never given twice. */
  rtc: number;
  publish: Channel | null;
  peers: Map<number, Channel>;
  /** Whether the program heard it's open. */
  opened: boolean;
  closed: boolean;
}

/** The pair id in a session's path: 16 hex digits. */
export const pairParam = (pair: bigint) => pair.toString(16).padStart(16, "0");

/** The pair id in a module's PAIR_SECTION, if it has one. */
export function pairOf(module: WebAssembly.Module): bigint | null {
  const [section] = WebAssembly.Module.customSections(module, PAIR_SECTION);
  if (section === undefined || section.byteLength !== 8) return null;
  return new DataView(section).getBigUint64(0, true);
}

/** What a message of `n` bytes adds to what waits to be written on a connection: on a WebSocket
 * its bytes, as its `bufferedAmount` counts them; on WebTransport its frame (the varint prefix is
 * one, two or three bytes, MAX_UP being below 2^21) and OVERHEAD. */
const costOf = (socket: boolean, n: number) => (socket ? n : n + (n < 128 ? 1 : n < 16384 ? 2 : 3) + OVERHEAD);
/** What waits to be written on a connection. */
const pendingOf = (s: Session) => (s.socket !== null ? s.socket.bufferedAmount : s.pending);

/**
 * A program's connections (`std::net`'s `open`, `send`, `close`). Each opened connection
 * gives exactly one `Connection` event that it's open (if it opens), then exactly one that it's
 * closed, whatever closes it; its messages come between. What arrives waits in `arrived` for the
 * frame that takes it.
 */
export class Net {
  readonly #sessions = new Map<number, Session>();
  readonly #relays = new Map<number, RelaySession>();
  #nextRtc = 1;
  #server: { url: string; ready: Promise<Server> } | null = null;
  /** What arrived: a message's bytes, or a connection's opening or closing. */
  arrived: ({ conn: number; bytes: Uint8Array } | { conn: number; open: boolean })[] = [];
  /** The bytes the messages in `arrived` hold, each counted with OVERHEAD more. */
  #held = 0;

  /** `base`: where `net.json` is; `print`: a line for whoever runs the program. */
  constructor(
    readonly base: () => string,
    readonly print: (line: string) => void,
    /** The page's WebRTC, for relays (none: a relay closes as it opens). */
    readonly rtc: Rtc | null = null,
  ) {}

  /** Checks one export's commands without opening, sending or closing anything. A send can
   * close a connection at its byte limits, so later opens see that same planned lifetime. */
  plan(): Pick<Net, "open" | "send" | "close" | "relay"> {
    const sessions = new Map<number, { open: boolean; writer: boolean; socket: boolean; pending: number; relay: boolean }>();
    const state = (conn: number) => {
      let planned = sessions.get(conn);
      if (planned === undefined) {
        const session = this.#sessions.get(conn);
        const relay = this.#relays.get(conn);
        planned = relay !== undefined
          ? { open: !relay.closed, writer: false, socket: false, pending: 0, relay: true }
          : {
              open: session !== undefined && !session.closed,
              writer: session?.writer != null || session?.socket?.readyState === WebSocket.OPEN,
              socket: session?.socket != null,
              pending: session === undefined ? 0 : pendingOf(session),
              relay: false,
            };
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
        session.socket = false;
        session.pending = 0;
        session.relay = false;
      },
      relay(conn, via) {
        const session = state(conn);
        if (session.open) throw new Error(`connection ${conn} is open`);
        session.open = state(via).open;
        session.writer = false;
        session.socket = false;
        session.pending = 0;
        session.relay = true;
      },
      send(conn, bytes) {
        const session = state(conn);
        if (session.relay) {
          if (bytes.length > MAX_RELAY) session.open = false;
          return;
        }
        if (!session.open || !session.writer) return;
        const cost = costOf(session.socket, bytes.length);
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
    if (this.#sessions.has(conn) || this.#relays.has(conn)) throw new Error(`connection ${conn} is open`);
    const session: Session = { transport: null, socket: null, writer: null, closed: false, pending: 0, relay: null };
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
      const url = `${server.url.replace(/\/$/, "")}/room/${room}`;
      if (!server.url.startsWith("https:")) return this.#socket(conn, url, session);
      const options: WebTransportOptions = {};
      if (server.certificateHash) {
        options.serverCertificateHashes = [{ algorithm: "sha-256", value: new Uint8Array(server.certificateHash) }];
      }
      const transport = new WebTransport(url, options);
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

  /** Connection `conn` as a WebSocket at `url`: each binary message is one frame, so none is
   * framed; what it holds waiting to be written is its `bufferedAmount`. */
  #socket(conn: number, url: string, session: Session): void {
    const socket = new WebSocket(url);
    socket.binaryType = "arraybuffer";
    session.socket = socket;
    socket.onopen = () => {
      if (!session.closed) this.arrived.push({ conn, open: true });
    };
    socket.onmessage = (e: MessageEvent<unknown>) => {
      if (session.closed) return;
      if (typeof e.data === "string") {
        // The room's word about a relay: anything else from it is past what a room says.
        const control = parseControl(e.data);
        if (control === null || session.relay === null) return;
        this.#control(session.relay, control);
        return;
      }
      if (!(e.data instanceof ArrayBuffer) || e.data.byteLength > MAX_FRAME || this.#held + e.data.byteLength + OVERHEAD > HELD) {
        this.print(`connection ${conn}: a message that isn't a frame, or past what may wait for the program`);
        this.#end(conn, session);
        return;
      }
      const bytes = new Uint8Array(e.data);
      this.#held += bytes.length + OVERHEAD;
      this.arrived.push({ conn, bytes });
    };
    socket.onclose = () => this.#end(conn, session);
    socket.onerror = () => this.#end(conn, session);
  }

  /** Sends `bytes` on connection `conn`: nothing if it isn't open (it may have closed since the
   * program last heard). A message past MAX_UP closes it, and so does one past PENDING bytes
   * not yet written. */
  send(conn: number, bytes: Uint8Array): void {
    const relay = this.#relays.get(conn);
    if (relay !== undefined) return this.#relaySend(conn, relay, bytes);
    const session = this.#sessions.get(conn);
    if (session?.socket && !session.closed) {
      if (session.socket.readyState !== WebSocket.OPEN) return;
      if (bytes.length > MAX_UP || pendingOf(session) + costOf(true, bytes.length) > PENDING) {
        this.print(`connection ${conn}: a message of ${bytes.length} bytes, past ${MAX_UP} or past ${PENDING} waiting to be written`);
        this.close(conn);
        return;
      }
      session.socket.send(bytes.slice());
      return;
    }
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
    const relay = this.#relays.get(conn);
    if (relay !== undefined) return this.#endRelay(conn, relay, true, "");
    const session = this.#sessions.get(conn);
    if (session) this.#end(conn, session);
  }

  /** Closes every connection, giving no events: the build changed, and its program goes. */
  closeAll(): void {
    for (const [conn, relay] of [...this.#relays]) this.#endRelay(conn, relay, false, "");
    for (const [conn, session] of [...this.#sessions]) this.#end(conn, session, false);
    this.arrived = [];
    this.#held = 0;
  }

  /** Ends a connection once: its session closes, and the program hears (if `tell`). */
  #end(conn: number, session: Session, tell = true): void {
    if (session.closed) return;
    session.closed = true;
    if (this.#sessions.get(conn) === session) this.#sessions.delete(conn);
    if (session.relay !== null) {
      const relay = this.#relays.get(session.relay);
      if (relay !== undefined) this.#endRelay(session.relay, relay, tell, "");
    }
    try {
      session.transport?.close();
      session.socket?.close(1000);
    } catch {
      // It's closed already.
    }
    if (tell) this.arrived.push({ conn, open: false });
  }

  /** Opens connection `conn` as the relay of the room connection `via` is open to: on a
   * WebSocket to the room (the Cloudflare host), through Realtime SFU, which the room's object
   * speaks to for it. A relay of a connection that isn't open, of one with a relay already, or
   * on a host with none, closes as it opens. */
  relay(conn: number, via: number): void {
    if (this.#sessions.has(conn) || this.#relays.has(conn)) throw new Error(`connection ${conn} is open`);
    const relay: RelaySession = { via, rtc: this.#nextRtc++, publish: null, peers: new Map(), opened: false, closed: false };
    this.#relays.set(conn, relay);
    const session = this.#sessions.get(via);
    const why = session === undefined || session.closed
      ? `connection ${via} isn't open`
      : session.relay !== null
        ? `connection ${via} has a relay already`
        : session.socket === null
          ? "this server has no relay"
          : this.rtc === null
            ? "this page can't make a relay (no WebRTC)"
            : null;
    if (why !== null) return this.#endRelay(conn, relay, true, `connection ${conn}: ${why}`);
    session!.relay = conn;
    this.#text(via, { relay: "join" });
  }

  /** Sends a control message on connection `via`'s WebSocket, if it's open. */
  #text(via: number, control: Control): void {
    const socket = this.#sessions.get(via)?.socket;
    if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(control));
  }

  /** What the room said about relay `conn`. */
  #control(conn: number, control: Control): void {
    const relay = this.#relays.get(conn);
    if (relay === undefined || relay.closed) return;
    const rtc = this.rtc!;
    const fail = (e: unknown) => this.#endRelay(conn, relay, true, `connection ${conn}: ${errorMessage(e)}`);
    switch (control.relay) {
      case "offer":
        void rtc.answer(relay.rtc, control.sdp).then((sdp) => {
          if (relay.closed) return;
          this.#text(relay.via, { relay: "answer", sdp });
          return rtc.connected(relay.rtc).then(() => {
            if (!relay.closed) this.#text(relay.via, { relay: "ready" });
          });
        }).catch(fail);
        return;
      case "publish":
        void rtc.channel(relay.rtc, control.id).then((channel) => {
          if (relay.closed) return channel.close();
          relay.publish = channel;
          const opened = () => {
            if (relay.closed || relay.opened) return;
            relay.opened = true;
            this.arrived.push({ conn, open: true });
          };
          channel.onopen = opened;
          channel.onclose = () => this.#endRelay(conn, relay, true, "");
          if (channel.readyState === "open") opened();
        }).catch(fail);
        return;
      case "peer":
        void rtc.channel(relay.rtc, control.id).then((channel) => {
          if (relay.closed) return channel.close();
          relay.peers.get(control.peer)?.close();
          relay.peers.set(control.peer, channel);
          channel.binaryType = "arraybuffer";
          channel.onmessage = (e) => {
            // Until the program hears the relay is open, and past what may wait, it's dropped:
            // a relay may lose any message.
            if (relay.closed || !relay.opened || !(e.data instanceof ArrayBuffer) || e.data.byteLength > MAX_RELAY) return;
            if (this.#held + e.data.byteLength + 4 + OVERHEAD > HELD) return;
            const bytes = fromPeer(control.peer, new Uint8Array(e.data));
            this.#held += bytes.length + OVERHEAD;
            this.arrived.push({ conn, bytes });
          };
        }).catch(fail);
        return;
      case "gone":
        relay.peers.get(control.peer)?.close();
        relay.peers.delete(control.peer);
        return;
      case "closed":
        return this.#endRelay(conn, relay, true, control.why === "" ? "" : `connection ${conn}: ${control.why}`);
      default:
        return;
    }
  }

  /** A send on relay `conn`: past MAX_RELAY it closes; while it isn't open, or past what its
   * channel holds unsent, it's dropped, as the network may drop it. */
  #relaySend(conn: number, relay: RelaySession, bytes: Uint8Array): void {
    if (relay.closed) return;
    if (bytes.length > MAX_RELAY) {
      return this.#endRelay(conn, relay, true, `connection ${conn}: a relay message of ${bytes.length} bytes, past ${MAX_RELAY}`);
    }
    const channel = relay.publish;
    if (channel === null || channel.readyState !== "open" || channel.bufferedAmount + bytes.length > RELAY_PENDING) return;
    channel.send(bytes.slice());
  }

  /** Ends a relay once: it leaves the room's relay, its WebRTC closes, and the program hears
   * (if `tell`). */
  #endRelay(conn: number, relay: RelaySession, tell: boolean, why: string): void {
    if (relay.closed) return;
    relay.closed = true;
    if (this.#relays.get(conn) === relay) this.#relays.delete(conn);
    if (why !== "") this.print(why);
    const via = this.#sessions.get(relay.via);
    if (via?.relay === conn) {
      via.relay = null;
      this.#text(relay.via, { relay: "leave" });
    }
    relay.publish?.close();
    for (const c of relay.peers.values()) c.close();
    relay.peers.clear();
    this.rtc?.close(relay.rtc);
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
