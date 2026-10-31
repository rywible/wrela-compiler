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
// bytes waiting for the room is closed too.

import { EventKind, SERVICE_RESPOND } from "../../browser/src/abi.gen.ts";
import { encodeEvent } from "../../browser/src/input.ts";
import { Inbox } from "../../browser/src/net.ts";
import { blob, commonAnswer, Failure, Instance, LIMITS, type Limits, type Owner, type Storage, writes } from "./host.ts";

/** The connection a room's journal comes from (`std::net::JOURNAL`). */
export const JOURNAL = 0;
/** The bit set on a message's connection when it trapped the room alone (`std::net::POISONED`). */
export const POISONED = 2 ** 31;
/** Failures at the host's limits in a row before a room lets its peers go. */
export const RETRIES = 3;
/** The largest frame, and the largest a peer sends (`wrela_serve::MAX_FRAME`, `MAX_UP`). */
export const MAX_FRAME = 64 * 1024;
export const MAX_UP = MAX_FRAME - 64;
/** The bytes a connection's messages may hold waiting for the room. */
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
  now?: () => number;
  print?: (line: string) => void;
  log?: (line: string) => void;
}

/** What the room holds of a socket: its connection, and the tag the program gave it. */
interface Attached {
  conn: number;
  tag: number;
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

const connection = (conn: number, open: boolean, tag: number) => encodeEvent(EventKind.Connection, 0, conn, open ? 1 : 0, tag, 0);
const word = (b: Uint8Array, at: number) => new DataView(b.buffer, b.byteOffset).getUint32(at, true);

/** How a call failed: the room's (its program, or the host's limits), or the journal's. */
type Fail = { room: Failure } | { journal: Failure };

/** A room of a server build, kept in a Durable Object's storage. */
export class Room implements Owner<Net> {
  queue = new Inbox();
  readonly limits: Limits;
  readonly print: ((line: string) => void) | undefined;
  private instance: Instance<Net> | null = null;
  private readonly sockets = new Map<number, { socket: Socket; tag: number }>();
  private incoming: Incoming[] = [];
  private failures = 0;
  private quarantined: string | null;
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
    const meta = (key: string) => sql.exec("SELECT value FROM meta WHERE key = ?", key).toArray()[0]?.["value"];
    const origin = meta("origin");
    this.origin = typeof origin === "number" ? origin : this.now();
    if (origin === undefined) sql.exec("INSERT INTO meta (key, value) VALUES ('origin', ?)", this.origin);
    const q = meta("quarantined");
    this.quarantined = typeof q === "string" ? q : null;
    // Its sockets outlive the object: each says which connection it is.
    for (const socket of open) {
      const a = socket.deserializeAttachment() as Attached | null;
      if (a !== null && typeof a?.conn === "number") this.sockets.set(a.conn, { socket, tag: a.tag });
    }
  }

  /** Seconds since the room was first made, never going back. */
  clock(): number {
    this.lastClock = Math.max(this.lastClock, (this.now() - this.origin) / 1000);
    return this.lastClock;
  }

  /** The next connection's number: never one used before in this room. */
  private nextConn(): number {
    const sql = this.storage.sql;
    const row = sql.exec("SELECT value FROM meta WHERE key = 'conn'").toArray()[0];
    const last = typeof row?.["value"] === "number" ? row["value"] : JOURNAL;
    const conn = last + 1 >= POISONED ? 1 : last + 1;
    sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('conn', ?)", conn);
    return conn;
  }

  /** A peer connected on `socket`. A quarantined room closes it. */
  connect(socket: Socket): void {
    if (this.quarantined !== null) {
      socket.close(1011, "the room is closed");
      return;
    }
    const conn = this.nextConn();
    socket.serializeAttachment({ conn, tag: 0 } satisfies Attached);
    this.sockets.set(conn, { socket, tag: 0 });
    this.incoming.push({ kind: "opened", conn });
    this.wake();
  }

  /** A message from `socket`: binary, at most `MAX_UP` bytes, or its connection closes. */
  message(socket: Socket, data: ArrayBuffer | string): void {
    const conn = this.connOf(socket);
    if (conn === null) return;
    if (typeof data === "string") return this.cut(conn, 1003, "a room takes binary messages");
    if (data.byteLength > MAX_UP) return this.cut(conn, 1009, `a message past ${MAX_UP} bytes`);
    let waiting = data.byteLength;
    for (const e of this.incoming) if (e.kind === "message" && e.conn === conn) waiting += e.bytes.length;
    if (waiting > CONN_IN) return this.cut(conn, 1008, `more than ${CONN_IN} bytes wait for the room`);
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
    return this.wake();
  }

  /** Settles once the room has taken what it was given. */
  get settled(): Promise<void> {
    return this.running;
  }

  /** Whatever came, taken in order, one wake at a time. */
  wake(): Promise<void> {
    this.running = this.running.then(
      () => this.wakeNow(),
      () => this.wakeNow(),
    );
    return this.running;
  }

  private connOf(socket: Socket): number | null {
    for (const [conn, s] of this.sockets) if (s.socket === socket) return conn;
    return null;
  }

  private disconnect(conn: number): void {
    if (!this.sockets.delete(conn)) return;
    this.incoming.push({ kind: "closed", conn });
    this.wake();
  }

  /** Closes connection `conn`'s socket, and the room hears it closed. */
  private cut(conn: number, code: number, why: string): void {
    this.sockets.get(conn)?.socket.close(code, why);
    this.disconnect(conn);
  }

  /** What waits goes, and every connection closes: its peers come back later. */
  private closeAll(code: number, why: string): void {
    this.incoming = [];
    for (const { socket } of this.sockets.values()) socket.close(code, why);
    this.sockets.clear();
  }

  private quarantine(why: string): void {
    this.instance = null;
    this.quarantined = why;
    this.storage.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('quarantined', ?)", why);
    this.options.log?.(`wrela: the room is quarantined: ${why}`);
    this.closeAll(1011, "the room is closed");
  }

  private failAtLimits(f: Failure): void {
    this.instance = null;
    this.queue = new Inbox();
    this.failures++;
    this.options.log?.(`wrela: a room's call failed at the host's limits: ${f.message}`);
    if (this.failures >= RETRIES) {
      this.failures = 0;
      this.closeAll(1013, "the room is unavailable");
    } else {
      void this.storage.setAlarm(this.now() + RETRY_MS);
    }
  }

  /** The room's events, a call's worth at a time, until none waits and no answer is due. */
  private async wakeNow(): Promise<void> {
    if (this.quarantined !== null) return;
    for (;;) {
      if (this.instance === null) {
        try {
          await this.start();
        } catch (e) {
          const f = e instanceof Failure ? e : new Failure(false, String(e));
          if (this.quarantined === null) this.failAtLimits(f);
          return;
        }
      }
      const instance = this.instance!;
      if (this.incoming.length === 0 && instance.arriving.length === 0) return;
      const batch = this.nextBatch();
      const r = await this.call(batch);
      if (r === null) continue;
      if ("journal" in r) return;
      if (batch.length === 1 && !r.room.program) {
        // The host's limits on one event: nothing was kept or sent, and it waits.
        this.incoming.unshift(...batch);
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
      if (given > 0 && given + e.bytes.length > 1 << 20) break;
      given += e.bytes.length;
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
    while (this.queue.length > 0) {
      const before = this.queue.length;
      try {
        instance.call(() => instance.serve(0));
        if (this.queue.length >= before) throw new Failure(true, UNREAD);
      } catch (e) {
        const f = e instanceof Failure ? e : new Failure(false, String(e));
        if (f.program) this.quarantine(`its journal traps it: ${f.message}`);
        throw f;
      }
      // Reading back keeps nothing new but a compaction: the snapshot, then what's still unread.
      for (const c of instance.commands) {
        if (c.kind !== "compact") continue;
        const unread = records.slice(records.length - this.queue.length);
        this.storage.transactionSync(() => {
          sql.exec("DELETE FROM journal");
          for (const r of [c.bytes, ...unread]) sql.exec("INSERT INTO journal (record) VALUES (?)", blob(r));
        });
      }
    }
    // A room with nothing to read back carries out what `serve_init` did, as its first call would.
    if (fresh && instance.commands.length > 0) {
      const r = await this.carryOut(instance.commands);
      if (r !== null) throw "room" in r ? r.room : r.journal;
    }
    for (const [conn, { tag }] of this.sockets) this.queue.push(connection(conn, true, tag));
    this.instance = instance;
  }

  /** One call with `batch`: the answers its last call's requests got, `serve`, then its commands
   * carried out. Null if it succeeded. */
  private async call(batch: Incoming[]): Promise<Fail | null> {
    const instance = this.instance!;
    await instance.settle();
    for (const e of batch) {
      if (e.kind === "message") this.queue.pushMessage(e.conn, e.bytes);
      else this.queue.push(connection(e.conn, e.kind === "opened", 0));
    }
    try {
      instance.call(() => instance.serve(Math.fround(this.clock())));
      // A call is given the whole batch: one that leaves some unread is the program's failure.
      if (this.queue.length > 0) throw new Failure(true, UNREAD);
    } catch (e) {
      this.instance = null;
      this.queue = new Inbox();
      return { room: e instanceof Failure ? e : new Failure(false, String(e)) };
    }
    const r = await this.carryOut(instance.commands);
    if (r === null) this.failures = 0;
    return r;
  }

  /** The failed call's events, one at a time on a fresh instance: one the program traps on alone
   * comes back poisoned, and if that traps too the room is quarantined; one past the host's limits
   * waits with those after it. Whether the room goes on. */
  private async oneByOne(batch: Incoming[]): Promise<boolean> {
    for (let i = 0; i < batch.length; i++) {
      const e = batch[i]!;
      const r = await this.alone(e);
      if (r === null) continue;
      if ("journal" in r) return false;
      if (this.quarantined === null) {
        this.incoming.unshift(...batch.slice(i));
        this.failAtLimits(r.room);
      }
      return false;
    }
    return true;
  }

  private async alone(e: Incoming): Promise<Fail | null> {
    const fresh = async (): Promise<Fail | null> => {
      try {
        await this.start();
        return null;
      } catch (f) {
        return { room: f instanceof Failure ? f : new Failure(false, String(f)) };
      }
    };
    if (this.instance === null) {
      const s = await fresh();
      if (s !== null) return s;
    }
    const r = await this.call([e]);
    if (r === null || "journal" in r || !r.room.program) return r;
    // A trap on a connection's opening or closing, alone, passes: only a message is poisoned.
    if (e.kind !== "message") return null;
    const s = await fresh();
    if (s !== null) return s;
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
      this.instance = null;
      return { room: e instanceof Failure ? e : new Failure(true, String(e)) };
    }
    const sql = this.storage.sql;
    try {
      if (batches.length > 0) await this.store.commit(batches);
      this.storage.transactionSync(() => {
        for (const c of commands) {
          if (c.kind === "keep") sql.exec("INSERT INTO journal (record) VALUES (?)", blob(c.bytes));
          if (c.kind === "compact") {
            sql.exec("DELETE FROM journal");
            sql.exec("INSERT INTO journal (record) VALUES (?)", blob(c.bytes));
          }
        }
      });
    } catch (e) {
      const f = new Failure(false, `the journal: ${(e as Error)?.message ?? e}`);
      this.instance = null;
      this.options.log?.(`wrela: a room's records weren't kept: ${f.message}`);
      this.closeAll(1011, "the room's records weren't kept");
      return { journal: f };
    }
    for (const c of commands) {
      const s = "conn" in c ? this.sockets.get(c.conn) : undefined;
      if (c.kind === "tag" && s !== undefined) {
        s.tag = c.tag;
        s.socket.serializeAttachment({ conn: c.conn, tag: c.tag } satisfies Attached);
      } else if (c.kind === "send") {
        s?.socket.send(c.bytes);
      } else if (c.kind === "close" && s !== undefined) {
        s.socket.close(1000, "");
        this.sockets.delete(c.conn);
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
        return self.answer(id, commonAnswer(service, body, this.now()) ?? null);
    }
  }
}
