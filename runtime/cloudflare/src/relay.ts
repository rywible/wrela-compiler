// A room's relay on Cloudflare (language.md §19.6, `std::net::relay`): Realtime SFU forwards
// what each joined peer sends to every other, unordered and never resent, and the room's object
// speaks to the SFU for its peers, with the credentials only it holds (`WRELA_SFU_APP`, and the
// secret `WRELA_SFU_TOKEN`). Each peer's page connects to the SFU alone, so no peer learns
// another's address; the room's program never sees what the relay carries.
//
// The talk with a peer is text messages on its own WebSocket to the room (browser relay.ts's
// `Control`): it asks to `join`; the room makes it an SFU session and sends the SFU's `offer`;
// the page's `answer` goes to the SFU; once the page says it's `ready` (its WebRTC connected), the
// room publishes its channel (`publish`), subscribes it to every other joined peer's and each of
// them to its (`peer`), and tells them when it goes (`gone`). Only a connection the room's program
// tagged may join (its tag names it to the others), one connection a tag (a peer that joins again
// on a new connection ends its relay on the old), and at most `MAX_RELAY_PEERS` at once, since
// each one more adds a subscription to every other; one whose WebRTC hasn't come up in
// `STALL_MS` gives its place up. Every join costs SFU calls, so a connection may join at most
// `JOINS` times a minute, and one with more than `MAX_QUEUED` control messages waiting is cut. A
// relay's state lives in its socket's attachment, so it outlives the object's eviction.

import { type Control, parseControl, RELAY_CHANNEL } from "../../browser/src/relay.ts";

/** The most peers on a room's relay at once. */
export const MAX_RELAY_PEERS = 8;
/** How long a joining peer may hold its place before its WebRTC is up. */
export const STALL_MS = 30_000;
/** How many times a connection may join a minute. */
export const JOINS = 4;
/** The most control messages a connection may have waiting. */
export const MAX_QUEUED = 16;
/** Realtime SFU's API. */
export const SFU_API = "https://rtc.live.cloudflare.com/v1/apps";

/** The SFU app a room speaks to, and how it reaches the network (a test's fake). */
export interface SfuConfig {
  app: string;
  token: string;
  fetch?: (url: string, init: RequestInit) => Promise<Response>;
}

/** A channel a peer hears another on: its id, and the other's SFU session it reads. */
export interface Heard {
  id: number;
  session: string;
}

/** A joined peer's relay, as its socket's attachment keeps it: the page's number for it, its SFU
 * session, how far it got and since when, the channel it publishes on, and the channel it hears
 * each other peer on (by their tags). */
export interface RelayState {
  n: number;
  session: string;
  stage: "offered" | "answered" | "live";
  since: number;
  publish: number | null;
  peers: Record<string, Heard>;
}

/** A room's peer, as its relay needs it. */
export interface RelayPeer {
  conn: number;
  tag: number;
  relay: RelayState | null;
}

/** What a room gives its relay: its peers, and how to tell one something, or keep its state. */
export interface RelayRoom {
  peer(conn: number): RelayPeer | null;
  peers(): RelayPeer[];
  text(conn: number, control: Control): void;
  keep(conn: number, relay: RelayState | null): void;
  log(line: string): void;
}

const policy = { dataChannelName: RELAY_CHANNEL, ordered: false, maxRetransmits: 0 };
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** A room's relay: each control message taken in order, one at a time. */
export class Relay {
  #running: Promise<void> = Promise.resolve();
  /** Each connection's control messages waiting, and when it joined lately. */
  readonly #queued = new Map<number, number>();
  readonly #joins = new Map<number, number[]>();

  constructor(
    readonly room: RelayRoom,
    readonly sfu: SfuConfig | null,
    readonly now: () => number = Date.now,
  ) {}

  /** Settles once what was asked is done. */
  get settled(): Promise<void> {
    return this.#running;
  }

  /** A text message from connection `conn`: false if the connection has more waiting than it
   * may (then the room cuts it). */
  message(conn: number, text: string): boolean {
    const control = parseControl(text);
    if (control === null) return true;
    const queued = (this.#queued.get(conn) ?? 0) + 1;
    if (queued > MAX_QUEUED) return false;
    this.#queued.set(conn, queued);
    this.#then(async () => {
      this.#queued.set(conn, (this.#queued.get(conn) ?? 1) - 1);
      await this.#take(conn, control);
    });
    return true;
  }

  /** Connection `conn` closed (or its tag changed, if `tell`): it leaves the relay. */
  closed(conn: number, peer: RelayPeer | null, tell = false): void {
    if (!tell) {
      this.#queued.delete(conn);
      this.#joins.delete(conn);
    }
    if (peer?.relay == null) return;
    const gone = { ...peer, relay: peer.relay };
    this.#then(() => this.#leave(gone, tell));
  }

  #then(work: () => Promise<void>): void {
    this.#running = this.#running.then(work).catch((e: unknown) => this.room.log(`wrela: a room's relay: ${message(e)}`));
  }

  async #take(conn: number, control: Control): Promise<void> {
    const peer = this.room.peer(conn);
    if (peer === null) return;
    // A word about a relay that isn't this peer's now is about one that ended.
    if (control.relay !== "join" && peer.relay?.n !== control.n) return;
    try {
      switch (control.relay) {
        case "join":
          return await this.#join(peer, control.n);
        case "answer":
          if (peer.relay?.stage !== "offered") return;
          await this.#api("PUT", `/sessions/${peer.relay.session}/renegotiate`, { sessionDescription: { type: "answer", sdp: control.sdp } });
          return this.#keep(conn, { ...peer.relay, stage: "answered" });
        case "ready":
          if (peer.relay?.stage !== "answered") return;
          return await this.#live(peer, peer.relay);
        case "leave":
          // The page ended it already, so it isn't told.
          if (peer.relay !== null) await this.#leave({ ...peer, relay: peer.relay }, false);
          return;
        default:
          return;
      }
    } catch (e) {
      // The SFU refused, or couldn't be reached: this peer's relay ends, and it may join again.
      this.room.log(`wrela: a room's relay: ${message(e)}`);
      const now = this.room.peer(conn);
      if (now?.relay != null && now.relay.n === control.n) await this.#leave({ ...now, relay: now.relay }, false).catch(() => {});
      if (now !== null) this.room.text(conn, { relay: "closed", n: control.n, why: "the relay couldn't be reached" });
    }
  }

  /** Keeps a peer's state, if it's still there and still on the same relay. */
  #keep(conn: number, relay: RelayState): void {
    const now = this.room.peer(conn);
    if (now !== null && now.relay?.n === relay.n && now.relay.session === relay.session) this.room.keep(conn, relay);
  }

  async #join(peer: RelayPeer, n: number): Promise<void> {
    const refuse = (why: string) => this.room.text(peer.conn, { relay: "closed", n, why });
    // A join while another relay is on ends that one: the page ended it, and its leave was lost.
    if (peer.relay !== null) await this.#leave({ ...peer, relay: peer.relay }, false);
    if (this.sfu === null) return refuse("this server has no relay");
    if (peer.tag === 0) return refuse("the room hasn't let this connection in");
    const at = this.now();
    const recent = (this.#joins.get(peer.conn) ?? []).filter((t) => at - t < 60_000);
    if (recent.length >= JOINS) return refuse(`this connection joined the relay ${JOINS} times this minute`);
    this.#joins.set(peer.conn, [...recent, at]);
    // One relay a tag: the same peer on another connection leaves it. A peer whose WebRTC
    // hasn't come up in time gives its place up.
    for (const o of this.room.peers()) {
      if (o.conn === peer.conn || o.relay === null) continue;
      if (o.tag === peer.tag || (o.relay.stage !== "live" && at - o.relay.since >= STALL_MS)) await this.#leave({ ...o, relay: o.relay }, true);
    }
    if (this.room.peers().filter((p) => p.conn !== peer.conn && p.relay !== null).length >= MAX_RELAY_PEERS) {
      return refuse(`the room's relay is full (${MAX_RELAY_PEERS})`);
    }
    const made = await this.#api("POST", "/sessions/new", undefined);
    const session = made["sessionId"];
    if (typeof session !== "string") throw new Error("the SFU gave no session");
    const established = await this.#api("POST", `/sessions/${session}/datachannels/establish`, {
      dataChannel: { location: "remote", dataChannelName: "server-events" },
    });
    const sdp = (established["sessionDescription"] as { sdp?: unknown } | undefined)?.sdp;
    if (typeof sdp !== "string") throw new Error("the SFU gave no offer");
    // The connection may have closed meanwhile: then the session is left to the SFU's timeout.
    const now = this.room.peer(peer.conn);
    if (now === null || now.relay !== null || now.tag !== peer.tag) return;
    this.room.keep(peer.conn, { n, session, stage: "offered", since: at, publish: null, peers: {} });
    this.room.text(peer.conn, { relay: "offer", n, sdp });
  }

  /** The peer's WebRTC is up: it publishes, hears every other joined peer, and they hear it. */
  async #live(peer: RelayPeer, relay: RelayState): Promise<void> {
    const [mine] = await this.#channels(relay.session, [{ location: "local", ...policy }]);
    const state: RelayState = { ...relay, stage: "live", publish: mine! };
    this.#keep(peer.conn, state);
    this.room.text(peer.conn, { relay: "publish", n: relay.n, id: mine! });
    const others = this.room.peers().filter((p) => p.conn !== peer.conn && p.relay?.stage === "live");
    if (others.length === 0) return;
    const heard = await this.#channels(relay.session, others.map((o) => ({ location: "remote", sessionId: o.relay!.session, ...policy })));
    const still = this.room.peer(peer.conn);
    if (still?.relay?.n !== relay.n || still.relay.session !== relay.session) return;
    others.forEach((o, i) => {
      state.peers[String(o.tag)] = { id: heard[i]!, session: o.relay!.session };
      this.room.text(peer.conn, { relay: "peer", n: relay.n, peer: o.tag, id: heard[i]! });
    });
    this.#keep(peer.conn, state);
    for (const o of others) {
      const [id] = await this.#channels(o.relay!.session, [{ location: "remote", sessionId: relay.session, ...policy }]);
      const now = this.room.peer(o.conn);
      if (now?.relay == null || now.relay.session !== o.relay!.session) continue;
      const old = now.relay.peers[String(peer.tag)];
      if (old !== undefined) await this.#close(now.relay.session, old.id);
      this.room.keep(o.conn, { ...now.relay, peers: { ...now.relay.peers, [String(peer.tag)]: { id: id!, session: relay.session } } });
      this.room.text(o.conn, { relay: "peer", n: now.relay.n, peer: peer.tag, id: id! });
    }
  }

  /** A peer leaves: every other stops hearing it, its channel closes, and it hears that it
   * closed (if `tell`). */
  async #leave(peer: RelayPeer & { relay: RelayState }, tell: boolean): Promise<void> {
    const mine = peer.relay;
    const now = this.room.peer(peer.conn);
    if (now?.relay?.session === mine.session) this.room.keep(peer.conn, null);
    if (tell) this.room.text(peer.conn, { relay: "closed", n: mine.n, why: "" });
    for (const o of this.room.peers()) {
      const heard = o.relay?.peers[String(peer.tag)];
      // Only what heard this relay's session: the same tag may be on another connection now.
      if (o.conn === peer.conn || o.relay == null || heard === undefined || heard.session !== mine.session) continue;
      const rest = { ...o.relay.peers };
      delete rest[String(peer.tag)];
      this.room.keep(o.conn, { ...o.relay, peers: rest });
      this.room.text(o.conn, { relay: "gone", n: o.relay.n, peer: peer.tag });
      await this.#close(o.relay.session, heard.id);
    }
    if (mine.publish !== null) await this.#close(mine.session, mine.publish);
  }

  async #close(session: string, id: number): Promise<void> {
    await this.#api("PUT", `/sessions/${session}/datachannels/close`, { dataChannels: [{ id }] }).catch(() => {});
  }

  /** Allocates channels on `session`: their ids, in order; any one refused fails them all. */
  async #channels(session: string, channels: Record<string, unknown>[]): Promise<number[]> {
    const r = await this.#api("POST", `/sessions/${session}/datachannels/new`, { dataChannels: channels });
    const got = r["dataChannels"];
    if (!Array.isArray(got) || got.length !== channels.length) throw new Error("the SFU gave the wrong channels");
    return got.map((c: Record<string, unknown>) => {
      if (c["errorCode"] !== undefined || typeof c["id"] !== "number") throw new Error(`the SFU refused a channel: ${String(c["errorDescription"] ?? c["errorCode"])}`);
      return c["id"];
    });
  }

  async #api(method: string, path: string, body: unknown): Promise<Record<string, unknown>> {
    const sfu = this.sfu!;
    const init: RequestInit = {
      method,
      headers: { authorization: `Bearer ${sfu.token}`, "content-type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    };
    const r = await (sfu.fetch ?? fetch)(`${SFU_API}/${sfu.app}${path}`, init);
    const json = (await r.json().catch(() => ({}))) as Record<string, unknown>;
    if (!r.ok || json["errorCode"] !== undefined) {
      throw new Error(`the SFU's ${path.replace(/\/sessions\/[^/]+/, "/sessions/…")}: ${r.status} ${String(json["errorDescription"] ?? "")}`.trim());
    }
    return json;
  }
}
