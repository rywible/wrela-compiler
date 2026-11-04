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
// tagged may join (its tag names it to the others), and at most `MAX_RELAY_PEERS` at once, since
// each one more adds a subscription to every other. A relay's state lives in its socket's
// attachment, so it outlives the object's eviction.

import { type Control, parseControl, RELAY_CHANNEL } from "../../browser/src/relay.ts";

/** The most peers on a room's relay at once. */
export const MAX_RELAY_PEERS = 8;
/** Realtime SFU's API. */
export const SFU_API = "https://rtc.live.cloudflare.com/v1/apps";

/** The SFU app a room speaks to, and how it reaches the network (a test's fake). */
export interface SfuConfig {
  app: string;
  token: string;
  fetch?: (url: string, init: RequestInit) => Promise<Response>;
}

/** A joined peer's relay, as its socket's attachment keeps it: its SFU session, how far it got,
 * the channel it publishes on, and the channel it hears each other peer on (by their tags). */
export interface RelayState {
  session: string;
  stage: "offered" | "answered" | "live";
  publish: number | null;
  peers: Record<string, number>;
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

/** A room's relay: each control message taken in order, one at a time. */
export class Relay {
  #running: Promise<void> = Promise.resolve();

  constructor(
    readonly room: RelayRoom,
    readonly sfu: SfuConfig | null,
  ) {}

  /** Settles once what was asked is done. */
  get settled(): Promise<void> {
    return this.#running;
  }

  /** A text message from connection `conn`. */
  message(conn: number, text: string): void {
    const control = parseControl(text);
    if (control === null) return;
    this.#then(() => this.#take(conn, control));
  }

  /** Connection `conn` closed: it leaves the relay. */
  closed(conn: number, peer: RelayPeer | null): void {
    if (peer?.relay == null) return;
    const gone = { ...peer, relay: peer.relay };
    this.#then(() => this.#leave(gone, false));
  }

  #then(work: () => Promise<void>): void {
    this.#running = this.#running.then(work).catch((e: unknown) => this.room.log(`wrela: a room's relay: ${e instanceof Error ? e.message : String(e)}`));
  }

  async #take(conn: number, control: Control): Promise<void> {
    const peer = this.room.peer(conn);
    if (peer === null) return;
    try {
      switch (control.relay) {
        case "join":
          return await this.#join(peer);
        case "answer":
          if (peer.relay?.stage !== "offered") return;
          await this.#api("PUT", `/sessions/${peer.relay.session}/renegotiate`, { sessionDescription: { type: "answer", sdp: control.sdp } });
          return this.room.keep(conn, { ...peer.relay, stage: "answered" });
        case "ready":
          if (peer.relay?.stage !== "answered") return;
          return await this.#live(peer, peer.relay);
        case "leave":
          if (peer.relay !== null) await this.#leave(peer, true);
          return;
        default:
          return;
      }
    } catch (e) {
      // The SFU refused, or couldn't be reached: this peer's relay ends, and it may join again.
      this.room.log(`wrela: a room's relay: ${e instanceof Error ? e.message : String(e)}`);
      const now = this.room.peer(conn);
      if (now?.relay != null) await this.#leave(now, true).catch(() => {});
      this.room.text(conn, { relay: "closed", why: "the relay couldn't be reached" });
    }
  }

  async #join(peer: RelayPeer): Promise<void> {
    const refuse = (why: string) => this.room.text(peer.conn, { relay: "closed", why });
    if (peer.relay !== null) return;
    if (this.sfu === null) return refuse("this server has no relay");
    if (peer.tag === 0) return refuse("the room hasn't let this connection in");
    if (this.room.peers().filter((p) => p.relay !== null).length >= MAX_RELAY_PEERS) return refuse(`the room's relay is full (${MAX_RELAY_PEERS})`);
    const made = await this.#api("POST", "/sessions/new", undefined);
    const session = made["sessionId"];
    if (typeof session !== "string") throw new Error("the SFU gave no session");
    const established = await this.#api("POST", `/sessions/${session}/datachannels/establish`, {
      dataChannel: { location: "remote", dataChannelName: "server-events" },
    });
    const sdp = (established["sessionDescription"] as { sdp?: unknown } | undefined)?.sdp;
    if (typeof sdp !== "string") throw new Error("the SFU gave no offer");
    this.room.keep(peer.conn, { session, stage: "offered", publish: null, peers: {} });
    this.room.text(peer.conn, { relay: "offer", sdp });
  }

  /** The peer's WebRTC is up: it publishes, hears every other joined peer, and they hear it. */
  async #live(peer: RelayPeer, relay: RelayState): Promise<void> {
    const [mine] = await this.#channels(relay.session, [{ location: "local", ...policy }]);
    const state: RelayState = { ...relay, stage: "live", publish: mine! };
    this.room.keep(peer.conn, state);
    this.room.text(peer.conn, { relay: "publish", id: mine! });
    const others = this.room.peers().filter((p) => p.conn !== peer.conn && p.relay?.stage === "live");
    if (others.length === 0) return;
    const heard = await this.#channels(relay.session, others.map((o) => ({ location: "remote", sessionId: o.relay!.session, ...policy })));
    others.forEach((o, i) => {
      state.peers[String(o.tag)] = heard[i]!;
      this.room.text(peer.conn, { relay: "peer", peer: o.tag, id: heard[i]! });
    });
    this.room.keep(peer.conn, state);
    for (const o of others) {
      const [id] = await this.#channels(o.relay!.session, [{ location: "remote", sessionId: relay.session, ...policy }]);
      const now = this.room.peer(o.conn);
      if (now?.relay == null) continue;
      this.room.keep(o.conn, { ...now.relay, peers: { ...now.relay.peers, [String(peer.tag)]: id! } });
      this.room.text(o.conn, { relay: "peer", peer: peer.tag, id: id! });
    }
  }

  /** A peer leaves: every other stops hearing it, and it hears that it closed (if `tell`). */
  async #leave(peer: RelayPeer, tell: boolean): Promise<void> {
    this.room.keep(peer.conn, null);
    if (tell) this.room.text(peer.conn, { relay: "closed", why: "" });
    for (const o of this.room.peers()) {
      const id = o.relay?.peers[String(peer.tag)];
      if (o.conn === peer.conn || o.relay == null || id === undefined) continue;
      const rest = { ...o.relay.peers };
      delete rest[String(peer.tag)];
      this.room.keep(o.conn, { ...o.relay, peers: rest });
      this.room.text(o.conn, { relay: "gone", peer: peer.tag });
      await this.#api("PUT", `/sessions/${o.relay.session}/datachannels/close`, { dataChannels: [{ id }] }).catch(() => {});
    }
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
