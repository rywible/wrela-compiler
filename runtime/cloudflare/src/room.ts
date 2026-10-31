// A server build's room on Cloudflare (language.md §19.6): one Durable Object a room, its peers'
// connections WebSockets that the object holds through the Hibernation API, its journal in the
// object's SQLite, and the server's store in the server object (cloudflare.ts), reached over RPC.
// It is `wrela_host::rooms` on another host, and keeps its rules:
//
// - **A call's records are kept before what it sent goes:** its store writes commit first (in the
//   server object), then its records (`net/keep`, `net/compact`) in one transaction here, then its
//   sends, closes and tags. A call that fails keeps, writes and sends nothing.
// - **A room that starts reads its journal back first,** as messages from connection `JOURNAL`
//   (0), then hears of its open connections again with their tags. The object starts again after
//   eviction or a deploy, its sockets still open (each socket's attachment holds its connection's
//   number and tag), so its peers stay connected while it reads back.
// - **A failed call's events are taken again one at a time** on a fresh instance: the message the
//   program traps on alone comes back with `POISONED` set, to be refused unrun; a trap on that, or
//   on the journal, quarantines the room (kept in its storage: it stays closed). One past the
//   host's limits waits, with those after it, for the alarm (250 ms), and after `RETRIES` failures
//   in a row the room's peers are let go, to come back later.
//
// Each WebSocket binary message is one frame (no varint), at most `MAX_UP` bytes from a peer; a
// text message, or a longer one, closes its connection. A connection with more than `CONN_IN`
// bytes waiting for the room is closed too, and so is one whose message would take what all wait
// past `Limits.queued`. Past a turn (`Limits.turn`), the room goes on in its alarm.
//
// A room's storage is its build's: the object's name holds the build's pair id, but a deploy
// gives a hibernated object the new build's code with the old build's sockets and journal. Such an
// object reads nothing back; it closes its sockets (1012), and their peers come back to the new
// build's rooms.

import { SERVICE_RESPOND } from "../../browser/src/abi.gen.ts";
import { connectionEvent } from "../../browser/src/input.ts";
import { Inbox, MAX_FRAME, MAX_UP, OVERHEAD, pairOf, pairParam, withinCall } from "../../browser/src/net.ts";
import { blob, commonAnswer, Failure, Instance, LIMITS, type Limits, type Owner, POISONED, type Storage, writes } from "./host.ts";

/** The connection a room's journal comes from (`std::net::JOURNAL`). */
export const JOURNAL = 0;
/** Failures at the host's limits in a row before a room lets its peers go. */
export const RETRIES = 3;
/** The bytes a connection's messages may hold waiting for the room, each counted with OVERHEAD
 * more (`wrela_serve`'s). */
export const CONN_IN = 1 << 20;
/** How soon a room that failed at the host's limits tries again, in milliseconds. */
export const RETRY_MS = 250;
const UNREAD = "`serve` returned without reading all its events";

/** A WebSocket as the room uses it (the Hibernation API's: its attachment survives eviction). */
export interface Socket {
  send(bytes: Uint8Array): void;
  close(code?: number, reason?: string): void;
  serializeAttachment(value: unknown): void;
  deserializeAttachment(): unknown;
}

/** The server object's store, as a room reaches it (cloudflare.ts's RPC). */
export interface StoreClient {
  answer(service: string, body: Uint8Array): Promise<Uint8Array | null>;
  commit(batches: Uint8Array[]): Promise<void>;
}

export interface RoomOptions {
  limits?: Limits;
  /** The build's pair id, if its module was read already (default: read from it). */
  pair?: bigint | null;
  now?: () => number;
  print?: (line: string) => void;
  log?: (line: string) => void;
}

/** What a socket's attachment holds: its connection, and the tag the program gave it. */
interface Attached {
  conn: number;
  tag: number;
}
const attached = (socket: Socket) => socket.deserializeAttachment() as Attached | null;

/** Closes `socket`, if it isn't closed already. */
function shut(socket: Socket, code: number, why: string): void {
  try {
    socket.close(code, why);
  } catch {
    // It's closed already.
  }
}

/** A room's network command (`std::net`), carried out once the call returns. */
type Net =
  | { kind: "keep"; bytes: Uint8Array }
  | { kind: "compact"; bytes: Uint8Array }
  | { kind: "send"; conn: number; bytes: Uint8Array }
  | { kind: "close"; conn: number }
  | { kind: "tag"; conn: number; tag: number }
  | { kind: "write"; body: Uint8Array };

/** What waits for the room. */
type Incoming = { kind: "opened" | "closed"; conn: number } | { kind: "message"; conn: number; bytes: Uint8Array };

const word = (b: Uint8Array, at: number) => new DataView(b.buffer, b.byteOffset).getUint32(at, true);

/** How a call failed: the room's (its program, or the host's limits), or the journal's. */
type Fail = { room: Failure } | { journal: Failure };

/** A room of a server build, kept in a Durable Object's storage. */
export class Room implements Owner<Net> {
  queue = new Inbox();
  readonly limits: Limits;
  readonly print: ((line: string) => void) | undefined;
  private instance: Instance<Net> | null = null;
  /** Its open connections' sockets; each socket's attachment says its connection and tag. */
  private readonly sockets = new Map<number, Socket>();
  private incoming: Incoming[] = [];
  /** The bytes each open connection's messages hold in `incoming`, and all of them. */
  private readonly waiting = new Map<number, number>();
  private held = 0;
  private failures = 0;
  /** When a room that failed at the host's limits tries again (0: whenever it's woken). */
  private retryAt = 0;
  /** Whether a turn ended with work left, which the alarm goes on with. */
  private handedOn = false;
  private quarantined: string | null;
  /** Whether its storage is another build's (a deploy since): it serves nothing. */
  private readonly replaced: boolean;
  /** The last connection number given, kept so none is given twice. */
  private lastConn: number;
  /** Unix milliseconds the room's time counts from, kept so it goes on after a restart. */
  private readonly origin: number;
  private lastClock = 0;
  private running: Promise<void> = Promise.resolve();
  private readonly now: () => number;

  constructor(
    readonly module: WebAssembly.Module,
    readonly storage: Storage & { setAlarm(at: number): unknown },
    readonly store: StoreClient,
    open: Socket[],
    readonly options: RoomOptions = {},
  ) {
    this.limits = options.limits ?? LIMITS;
    this.print = options.print;
    this.now = options.now ?? Date.now;
    const sql = storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS journal (seq INTEGER PRIMARY KEY AUTOINCREMENT, record BLOB NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value)");
    const meta = new Map(sql.exec("SELECT key, value FROM meta").toArray().map((r) => [r["key"], r["value"]]));
    const origin = meta.get("origin");
    this.origin = typeof origin === "number" ? origin : this.now();
    if (origin === undefined) sql.exec("INSERT INTO meta (key, value) VALUES ('origin', ?)", this.origin);
    const q = meta.get("quarantined");
    this.quarantined = typeof q === "string" ? q : null;
    const last = meta.get("conn");
    this.lastConn = typeof last === "number" ? last : JOURNAL;
    const pair = options.pair !== undefined ? options.pair : pairOf(module);
    const build = pair === null ? "none" : pairParam(pair);
    const kept = meta.get("pair");
    if (kept === undefined) sql.exec("INSERT INTO meta (key, value) VALUES ('pair', ?)", build);
    this.replaced = kept !== undefined && kept !== build;
    if (this.replaced) {
      this.options.log?.("wrela: a room's storage is another build's, and it serves nothing");
      for (const socket of open) shut(socket, 1012, "the room's build was replaced");
      return;
    }
    // Its sockets outlive the object: each says which connection it is.
    for (const socket of open) {
      const a = attached(socket);
      if (typeof a?.conn === "number") this.sockets.set(a.conn, socket);
    }
  }

  /** Seconds since the room was first made, never going back. */
  clock(): number {
    this.lastClock = Math.max(this.lastClock, (this.now() - this.origin) / 1000);
    return this.lastClock;
  }

  /** The next connection's number: never one used before in this room. */
  private nextConn(): number {
    this.lastConn = this.lastConn + 1 >= POISONED ? 1 : this.lastConn + 1;
    this.storage.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('conn', ?)", this.lastConn);
    return this.lastConn;
  }

  /** A peer connected on `socket`. A quarantined room closes it. */
  connect(socket: Socket): void {
    if (this.replaced || this.quarantined !== null) {
      shut(socket, this.replaced ? 1012 : 1011, "the room is closed");
      return;
    }
    const conn = this.nextConn();
    socket.serializeAttachment({ conn, tag: 0 } satisfies Attached);
    this.sockets.set(conn, socket);
    this.waiting.set(conn, 0);
    this.incoming.push({ kind: "opened", conn });
    this.wake();
  }

  /** A message from `socket`: binary, at most `MAX_UP` bytes, or its connection closes. */
  message(socket: Socket, data: ArrayBuffer | string): void {
    const conn = this.connOf(socket);
    if (conn === null) return;
    if (typeof data === "string") return this.cut(conn, 1003, "a room takes binary messages");
    if (data.byteLength > MAX_UP) return this.cut(conn, 1009, `a message past ${MAX_UP} bytes`);
    const cost = data.byteLength + OVERHEAD;
    const waiting = (this.waiting.get(conn) ?? 0) + cost;
    if (waiting > CONN_IN) return this.cut(conn, 1008, `more than ${CONN_IN} bytes wait for the room`);
    if (this.held + cost > this.limits.queued) return this.cut(conn, 1008, `more than ${this.limits.queued} bytes wait for the room`);
    this.waiting.set(conn, waiting);
    this.held += cost;
    this.incoming.push({ kind: "message", conn, bytes: new Uint8Array(data) });
    this.wake();
  }

  /** `socket` closed. */
  closed(socket: Socket): void {
    const conn = this.connOf(socket);
    if (conn !== null) this.disconnect(conn);
  }

  /** The object's alarm: a room that failed at the host's limits tries again. */
  alarm(): Promise<void> {
    return this.wake(true);
  }

  /** Settles once the room has taken what it was given. */
  get settled(): Promise<void> {
    return this.running;
  }

  /** Whatever came, taken in order, one wake at a time; while a failure at the host's limits, or
   * a turn's end, waits for the alarm (`alarm`), only the alarm's. */
  wake(alarm = false): Promise<void> {
    this.running = this.running.then(
      () => this.wakeNow(alarm),
      () => this.wakeNow(alarm),
    );
    return this.running;
  }

  private connOf(socket: Socket): number | null {
    const conn = attached(socket)?.conn;
    return conn !== undefined && this.sockets.get(conn) === socket ? conn : null;
  }

  private disconnect(conn: number): void {
    if (!this.sockets.delete(conn)) return;
    this.forget(conn);
    this.incoming.push({ kind: "closed", conn });
    this.wake();
  }

  /** Connection `conn` is gone: its messages that wait count toward nothing. */
  private forget(conn: number): void {
    this.held -= this.waiting.get(conn) ?? 0;
    this.waiting.delete(conn);
  }

  /** Closes connection `conn`'s socket, and the room hears it closed. */
  private cut(conn: number, code: number, why: string): void {
    const socket = this.sockets.get(conn);
    if (socket !== undefined) shut(socket, code, why);
    this.disconnect(conn);
  }

  /** What waits goes, and every connection closes: its peers come back later. */
  private closeAll(code: number, why: string): void {
    this.incoming = [];
    this.waiting.clear();
    this.held = 0;
    for (const socket of this.sockets.values()) shut(socket, code, why);
    this.sockets.clear();
  }

  /** Counts `e`'s bytes (`sign` 1) toward what waits for the room, or no longer (-1). */
  private count(e: Incoming, sign: 1 | -1): void {
    if (e.kind !== "message") return;
    const w = this.waiting.get(e.conn);
    if (w === undefined) return;
    const cost = sign * (e.bytes.length + OVERHEAD);
    this.waiting.set(e.conn, w + cost);
    this.held += cost;
  }

  /** `events` wait again, before what came since. */
  private requeue(events: Incoming[]): void {
    for (const e of events) this.count(e, 1);
    this.incoming.unshift(...events);
  }

  private quarantine(why: string): void {
    this.quarantined = why;
    this.storage.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('quarantined', ?)", why);
    this.options.log?.(`wrela: the room is quarantined: ${why}`);
    this.closeAll(1011, "the room is closed");
  }

  private failAtLimits(f: Failure): void {
    this.failures++;
    this.options.log?.(`wrela: a room's call failed at the host's limits: ${f.message}`);
    if (this.failures >= RETRIES) {
      this.failures = 0;
      this.retryAt = 0;
      this.closeAll(1013, "the room is unavailable");
    } else {
      this.retryAt = this.now() + RETRY_MS;
      void this.storage.setAlarm(this.retryAt);
    }
  }

  /** A fresh instance (`start`): null, or why it didn't start. */
  private async restart(): Promise<Failure | null> {
    try {
      await this.start();
      return null;
    } catch (e) {
      return Failure.of(e);
    }
  }

  /** The room's events, a call's worth at a time, until none waits and no answer is due. */
  private async wakeNow(alarm: boolean): Promise<void> {
    if (this.replaced || this.quarantined !== null) return;
    if (!alarm && (this.handedOn || this.now() < this.retryAt)) return;
    if (alarm) this.handedOn = false;
    const began = this.now();
    for (;;) {
      if (this.instance === null) {
        const f = await this.restart();
        if (f !== null) {
          if (this.quarantined === null) this.failAtLimits(f);
          return;
        }
      }
      if (this.incoming.length === 0 && this.instance!.arriving.length === 0) return;
      const batch = this.nextBatch();
      const r = await this.call(batch);
      if (r === null) {
        // Past a turn, the rest goes on in the alarm, so no one invocation runs past its CPU.
        const more = this.incoming.length > 0 || this.instance!.arriving.length > 0;
        if (more && this.now() - began > this.limits.turn) {
          this.handedOn = true;
          void this.storage.setAlarm(this.now());
          return;
        }
        continue;
      }
      if ("journal" in r) return;
      if (batch.length === 1 && !r.room.program) {
        // The host's limits on one event: nothing was kept or sent, and it waits.
        this.requeue(batch);
        this.failAtLimits(r.room);
        return;
      }
      if (!(await this.oneByOne(batch))) return;
    }
  }

  /** The events one call is given: messages up to a call's payload bytes, and those between. */
  private nextBatch(): Incoming[] {
    let given = 0;
    let n = 0;
    for (; n < this.incoming.length; n++) {
      const e = this.incoming[n]!;
      if (e.kind !== "message") continue;
      if (!withinCall(given, e.bytes.length)) break;
      given += e.bytes.length;
      this.count(e, -1);
    }
    return this.incoming.splice(0, n);
  }

  /** A fresh instance: `serve_init`, the journal read back, then the open connections again. */
  private async start(): Promise<void> {
    this.instance = null;
    this.queue = new Inbox();
    const instance = await Instance.start<Net>(this, this.module);
    const sql = this.storage.sql;
    const records = sql.exec("SELECT record FROM journal ORDER BY seq").toArray().map((r) => new Uint8Array(r["record"] as ArrayBuffer));
    const fresh = records.length === 0;
    for (const r of records) this.queue.pushMessage(JOURNAL, r);
    // Reading back keeps nothing new but a compaction: the last one's snapshot, then what was
    // still unread when it was made.
    let compacted: { snapshot: Uint8Array; unread: number } | null = null;
    while (this.queue.length > 0) {
      const before = this.queue.length;
      // What its last call asked is answered for this one, as on an ordinary call.
      await instance.settle();
      try {
        instance.call(() => instance.serve(0));
        if (this.queue.length >= before) throw new Failure(true, UNREAD);
      } catch (e) {
        const f = e as Failure;
        if (f.program) this.quarantine(`its journal traps it: ${f.message}`);
        throw f;
      }
      for (const c of instance.commands) if (c.kind === "compact") compacted = { snapshot: c.bytes, unread: this.queue.length };
    }
    if (compacted !== null) {
      const { snapshot, unread } = compacted;
      this.storage.transactionSync(() => {
        sql.exec("DELETE FROM journal");
        for (const r of [snapshot, ...records.slice(records.length - unread)]) sql.exec("INSERT INTO journal (record) VALUES (?)", blob(r));
      });
    }
    // A room with nothing to read back carries out what `serve_init` did, as its first call would.
    if (fresh && instance.commands.length > 0) {
      const r = await this.carryOut(instance.commands);
      if (r !== null) throw "room" in r ? r.room : r.journal;
    }
    for (const [conn, socket] of this.sockets) this.queue.push(connectionEvent(conn, true, attached(socket)?.tag ?? 0));
    this.instance = instance;
  }

  /** One call with `batch`: the answers its last call's requests got, `serve`, then its commands
   * carried out. Null if it succeeded; if it failed, its instance goes. */
  private async call(batch: Incoming[]): Promise<Fail | null> {
    const instance = this.instance!;
    await instance.settle();
    for (const e of batch) {
      if (e.kind === "message") this.queue.pushMessage(e.conn, e.bytes);
      else this.queue.push(connectionEvent(e.conn, e.kind === "opened"));
    }
    let r: Fail | null;
    try {
      instance.call(() => instance.serve(Math.fround(this.clock())));
      // A call is given the whole batch: one that leaves some unread is the program's failure.
      if (this.queue.length > 0) throw new Failure(true, UNREAD);
      r = await this.carryOut(instance.commands);
    } catch (e) {
      r = { room: Failure.of(e) };
    }
    if (r === null) {
      this.failures = 0;
      this.retryAt = 0;
    } else {
      this.instance = null;
    }
    return r;
  }

  /** The failed call's events, one at a time on a fresh instance: one the program traps on alone
   * comes back poisoned, and if that traps too the room is quarantined; one past the host's limits
   * waits with those after it. Whether the room goes on. */
  private async oneByOne(batch: Incoming[]): Promise<boolean> {
    for (let i = 0; i < batch.length; i++) {
      const r = await this.alone(batch[i]!);
      if (r === null) continue;
      if ("room" in r && this.quarantined === null) {
        this.requeue(batch.slice(i));
        this.failAtLimits(r.room);
      }
      return false;
    }
    return true;
  }

  private async alone(e: Incoming): Promise<Fail | null> {
    if (this.instance === null) {
      const f = await this.restart();
      if (f !== null) return { room: f };
    }
    const r = await this.call([e]);
    if (r === null || "journal" in r || !r.room.program) return r;
    // A trap on a connection's opening or closing, alone, passes: only a message is poisoned.
    if (e.kind !== "message") return null;
    const f = await this.restart();
    if (f !== null) return { room: f };
    const poisoned = await this.call([{ kind: "message", conn: e.conn + POISONED, bytes: e.bytes }]);
    if (poisoned !== null && "room" in poisoned && poisoned.room.program) {
      this.quarantine(`a poisoned message traps it: ${poisoned.room.message}`);
    }
    return poisoned;
  }

  /** The call's writes committed (the server object's store), its records kept, then its tags,
   * sends and closes. A write the program got wrong is its failure; the store's or the journal's
   * own failure lets the room's peers go, to send again what wasn't settled. */
  private async carryOut(commands: Net[]): Promise<Fail | null> {
    const batches = commands.flatMap((c) => (c.kind === "write" ? [c.body] : []));
    try {
      writes(batches);
    } catch (e) {
      return { room: e as Failure };
    }
    // A compaction replaces what the records before it in the call would have added.
    let from = 0;
    for (let i = 0; i < commands.length; i++) if (commands[i]!.kind === "compact") from = i;
    const sql = this.storage.sql;
    try {
      if (batches.length > 0) await this.store.commit(batches);
      this.storage.transactionSync(() => {
        for (const c of commands.slice(from)) {
          if (c.kind === "keep") sql.exec("INSERT INTO journal (record) VALUES (?)", blob(c.bytes));
          if (c.kind === "compact") {
            sql.exec("DELETE FROM journal");
            sql.exec("INSERT INTO journal (record) VALUES (?)", blob(c.bytes));
          }
        }
      });
    } catch (e) {
      const f = new Failure(false, `the journal: ${Failure.of(e).message}`);
      this.options.log?.(`wrela: a room's records weren't kept: ${f.message}`);
      this.closeAll(1011, "the room's records weren't kept");
      return { journal: f };
    }
    // The call is settled now: a socket that fails from here is its peer's to come back on, and
    // the call isn't taken again.
    for (const c of commands) {
      const conn = "conn" in c ? c.conn : null;
      const s = conn === null ? undefined : this.sockets.get(conn);
      if (conn === null || s === undefined) continue;
      try {
        if (c.kind === "tag") s.serializeAttachment({ conn: c.conn, tag: c.tag } satisfies Attached);
        else if (c.kind === "send") s.send(c.bytes);
        else if (c.kind === "close") {
          this.sockets.delete(c.conn);
          this.forget(c.conn);
          s.close(1000, "");
        }
      } catch {
        this.cut(conn, 1011, "the connection failed");
      }
    }
    return null;
  }

  submit(self: Instance<Net>, id: number, service: string, body: Uint8Array): void {
    const conn = () => {
      if (body.length < 4) self.refuse(`\`${service}\` needs a connection's number`);
      return word(body, 0);
    };
    switch (service) {
      case "net/keep":
        return self.command({ kind: "keep", bytes: body }, body.length);
      case "net/compact":
        return self.command({ kind: "compact", bytes: body }, body.length);
      case "store/write":
        return self.command({ kind: "write", body }, body.length);
      case "net/send": {
        const bytes = body.subarray(4);
        if (bytes.length > MAX_FRAME) self.refuse(`a message past ${MAX_FRAME} bytes`);
        return self.command({ kind: "send", conn: conn(), bytes }, body.length);
      }
      case "net/close":
        return self.command({ kind: "close", conn: conn() }, body.length);
      case "net/tag":
        if (body.length !== 8) self.refuse("`net/tag` needs a connection's number and a tag");
        return self.command({ kind: "tag", conn: conn(), tag: word(body, 4) }, body.length);
      case SERVICE_RESPOND:
        return self.refuse("`http/respond`: a room's connections are its peers', not HTTP requests");
      case "store/get":
      case "store/scan":
        return self.answer(
          id,
          this.store.answer(service, body).catch(() => null),
        );
      default:
        return self.answer(id, commonAnswer(service, body, this.now()));
    }
  }
}
