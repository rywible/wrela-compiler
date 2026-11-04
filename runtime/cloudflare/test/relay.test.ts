// A room's relay against a fake Realtime SFU: who may join, the talk with each peer (offer,
// answer, ready), each joined peer subscribed to every other, a peer that leaves or closes, the
// relay's cap, and an SFU that refuses.

import { expect, test } from "bun:test";
import type { Control } from "../../browser/src/relay.ts";
import { MAX_RELAY_PEERS, Relay, type RelayPeer, type RelayRoom, type RelayState, SFU_API } from "../src/relay.ts";

/** A room of peers (conn -> tag), what each was told, and a fake SFU that logs each call. */
function setup(tags: Record<number, number>, refuse: (path: string) => boolean = () => false) {
  const peers = new Map<number, RelayPeer>(Object.entries(tags).map(([c, t]) => [Number(c), { conn: Number(c), tag: t, relay: null }]));
  const told = new Map<number, Control[]>();
  const calls: string[] = [];
  let sessions = 0;
  let channels = 100;
  const room: RelayRoom = {
    peer: (conn) => peers.get(conn) ?? null,
    peers: () => [...peers.values()],
    text: (conn, c) => void (told.get(conn) ?? told.set(conn, []).get(conn)!).push(c),
    keep: (conn, relay: RelayState | null) => {
      const p = peers.get(conn);
      if (p) p.relay = relay === null ? null : structuredClone(relay);
    },
    log: () => {},
  };
  const fetch = async (url: string, init: RequestInit) => {
    const path = url.slice(`${SFU_API}/app-1`.length);
    calls.push(`${init.method} ${path}${init.body ? " " + init.body : ""}`);
    expect((init.headers as Record<string, string>)["authorization"]).toBe("Bearer secret");
    if (refuse(path)) return Response.json({ errorCode: "nope", errorDescription: "refused" }, { status: 400 });
    if (path === "/sessions/new") return Response.json({ sessionId: `s${++sessions}` });
    if (path.endsWith("/datachannels/establish")) return Response.json({ sessionDescription: { type: "offer", sdp: `offer-${path.split("/")[2]}` } });
    if (path.endsWith("/datachannels/new")) {
      const asked = JSON.parse(init.body as string).dataChannels as unknown[];
      return Response.json({ dataChannels: asked.map(() => ({ id: ++channels })) });
    }
    return Response.json({});
  };
  const relay = new Relay(room, { app: "app-1", token: "secret", fetch });
  const say = async (conn: number, c: Control) => {
    relay.message(conn, JSON.stringify(c));
    await relay.settled;
  };
  /** Takes `conn` through join, answer and ready. */
  const join = async (conn: number) => {
    await say(conn, { relay: "join" });
    await say(conn, { relay: "answer", sdp: `answer-${conn}` });
    await say(conn, { relay: "ready" });
  };
  return { peers, told, calls, relay, say, join };
}

test("each joined peer publishes, and hears every other through the SFU", async () => {
  const { peers, told, calls, join } = setup({ 1: 7, 2: 9 });
  await join(1);
  expect(told.get(1)).toEqual([{ relay: "offer", sdp: "offer-s1" }, { relay: "publish", id: 101 }]);
  expect(calls).toEqual([
    "POST /sessions/new",
    'POST /sessions/s1/datachannels/establish {"dataChannel":{"location":"remote","dataChannelName":"server-events"}}',
    'PUT /sessions/s1/renegotiate {"sessionDescription":{"type":"answer","sdp":"answer-1"}}',
    'POST /sessions/s1/datachannels/new {"dataChannels":[{"location":"local","dataChannelName":"relay","ordered":false,"maxRetransmits":0}]}',
  ]);
  calls.length = 0;
  await join(2);
  // The newcomer hears peer 7 on one channel; peer 7 hears it (tag 9) on another.
  expect(told.get(2)).toEqual([{ relay: "offer", sdp: "offer-s2" }, { relay: "publish", id: 102 }, { relay: "peer", peer: 7, id: 103 }]);
  expect(told.get(1)!.at(-1)).toEqual({ relay: "peer", peer: 9, id: 104 });
  expect(calls.slice(-2)).toEqual([
    'POST /sessions/s2/datachannels/new {"dataChannels":[{"location":"remote","sessionId":"s1","dataChannelName":"relay","ordered":false,"maxRetransmits":0}]}',
    'POST /sessions/s1/datachannels/new {"dataChannels":[{"location":"remote","sessionId":"s2","dataChannelName":"relay","ordered":false,"maxRetransmits":0}]}',
  ]);
  expect(peers.get(1)!.relay).toEqual({ session: "s1", stage: "live", publish: 101, peers: { "9": 104 } });
  expect(peers.get(2)!.relay).toEqual({ session: "s2", stage: "live", publish: 102, peers: { "7": 103 } });
});

test("a peer that leaves or closes is gone from the others", async () => {
  const { peers, told, calls, relay, say, join } = setup({ 1: 7, 2: 9, 3: 11 });
  await join(1);
  await join(2);
  await join(3);
  await say(3, { relay: "leave" });
  expect(told.get(3)!.at(-1)).toEqual({ relay: "closed", why: "" });
  expect(told.get(1)!.at(-1)).toEqual({ relay: "gone", peer: 11 });
  expect(peers.get(1)!.relay!.peers).toEqual({ "9": expect.any(Number) });
  expect(calls.at(-1)).toMatch(/^PUT \/sessions\/s2\/datachannels\/close /);
  // A socket that closes leaves as well, and isn't told.
  const two = peers.get(2)!;
  peers.delete(2);
  relay.closed(2, two);
  await relay.settled;
  expect(told.get(1)!.at(-1)).toEqual({ relay: "gone", peer: 9 });
  expect(peers.get(1)!.relay!.peers).toEqual({});
});

test("only a tagged connection joins, out of turn steps do nothing, and the relay has a cap", async () => {
  const tags: Record<number, number> = { 1: 0 };
  for (let c = 2; c <= MAX_RELAY_PEERS + 2; c++) tags[c] = c;
  const { told, calls, say, join } = setup(tags);
  await say(1, { relay: "join" });
  expect(told.get(1)).toEqual([{ relay: "closed", why: "the room hasn't let this connection in" }]);
  // `ready` before an answer, or an answer never offered, is ignored.
  await say(2, { relay: "ready" });
  await say(2, { relay: "answer", sdp: "x" });
  expect(calls).toEqual([]);
  for (let c = 2; c < MAX_RELAY_PEERS + 2; c++) await join(c);
  await say(MAX_RELAY_PEERS + 2, { relay: "join" });
  expect(told.get(MAX_RELAY_PEERS + 2)).toEqual([{ relay: "closed", why: `the room's relay is full (${MAX_RELAY_PEERS})` }]);
});

test("an SFU that refuses ends that peer's relay, and it may join again", async () => {
  let refusing = true;
  const { peers, told, say, join } = setup({ 1: 7 }, (path) => refusing && path.endsWith("/renegotiate"));
  await say(1, { relay: "join" });
  await say(1, { relay: "answer", sdp: "a" });
  expect(told.get(1)!.at(-1)).toEqual({ relay: "closed", why: "the relay couldn't be reached" });
  expect(peers.get(1)!.relay).toBeNull();
  refusing = false;
  await join(1);
  expect(peers.get(1)!.relay!.stage).toBe("live");
});
