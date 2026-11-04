// A room's relay against a fake Realtime SFU: who may join, the talk with each peer (offer,
// answer, ready), each joined peer subscribed to every other, a peer that leaves, closes or comes
// back, words about a relay that ended, the relay's caps, and an SFU that refuses.

import { expect, test } from "bun:test";
import type { Control } from "../../browser/src/relay.ts";
import { JOINS, MAX_QUEUED, MAX_RELAY_PEERS, Relay, type RelayPeer, type RelayRoom, type RelayState, SFU_API, STALL_MS } from "../src/relay.ts";

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
  let clock = 1_000_000;
  const relay = new Relay(room, { app: "app-1", token: "secret", fetch }, () => clock);
  const say = async (conn: number, c: Control) => {
    relay.message(conn, JSON.stringify(c));
    await relay.settled;
  };
  /** Takes `conn` through join, answer and ready, as its relay `n` (by default, its conn). */
  const join = async (conn: number, n = conn) => {
    await say(conn, { relay: "join", n });
    await say(conn, { relay: "answer", n, sdp: `answer-${conn}` });
    await say(conn, { relay: "ready", n });
  };
  const tick = (ms: number) => void (clock += ms);
  return { peers, told, calls, relay, say, join, tick };
}

test("each joined peer publishes, and hears every other through the SFU", async () => {
  const { peers, told, calls, join } = setup({ 1: 7, 2: 9 });
  await join(1);
  expect(told.get(1)).toEqual([{ relay: "offer", n: 1, sdp: "offer-s1" }, { relay: "publish", n: 1, id: 101 }]);
  expect(calls).toEqual([
    "POST /sessions/new",
    'POST /sessions/s1/datachannels/establish {"dataChannel":{"location":"remote","dataChannelName":"server-events"}}',
    'PUT /sessions/s1/renegotiate {"sessionDescription":{"type":"answer","sdp":"answer-1"}}',
    'POST /sessions/s1/datachannels/new {"dataChannels":[{"location":"local","dataChannelName":"relay","ordered":false,"maxRetransmits":0}]}',
  ]);
  calls.length = 0;
  await join(2);
  // The newcomer hears peer 7 on one channel; peer 7 hears it (tag 9) on another.
  expect(told.get(2)).toEqual([
    { relay: "offer", n: 2, sdp: "offer-s2" },
    { relay: "publish", n: 2, id: 102 },
    { relay: "peer", n: 2, peer: 7, id: 103 },
  ]);
  expect(told.get(1)!.at(-1)).toEqual({ relay: "peer", n: 1, peer: 9, id: 104 });
  expect(calls.slice(-2)).toEqual([
    'POST /sessions/s2/datachannels/new {"dataChannels":[{"location":"remote","sessionId":"s1","dataChannelName":"relay","ordered":false,"maxRetransmits":0}]}',
    'POST /sessions/s1/datachannels/new {"dataChannels":[{"location":"remote","sessionId":"s2","dataChannelName":"relay","ordered":false,"maxRetransmits":0}]}',
  ]);
  expect(peers.get(1)!.relay).toEqual({ n: 1, session: "s1", stage: "live", since: expect.any(Number), publish: 101, peers: { "9": { id: 104, session: "s2" } } });
  expect(peers.get(2)!.relay).toEqual({ n: 2, session: "s2", stage: "live", since: expect.any(Number), publish: 102, peers: { "7": { id: 103, session: "s1" } } });
});

test("a peer that leaves or closes is gone from the others, and its channels close", async () => {
  const { peers, told, calls, relay, say, join } = setup({ 1: 7, 2: 9, 3: 11 });
  await join(1);
  await join(2);
  await join(3);
  const before = told.get(3)!.length;
  calls.length = 0;
  await say(3, { relay: "leave", n: 3 });
  // The page ended it, so it isn't told; the others are, and the SFU's channels close.
  expect(told.get(3)!.length).toBe(before);
  expect(peers.get(3)!.relay).toBeNull();
  expect(told.get(1)!.at(-1)).toEqual({ relay: "gone", n: 1, peer: 11 });
  expect(peers.get(1)!.relay!.peers).toEqual({ "9": expect.any(Object) });
  expect(calls).toEqual([
    expect.stringMatching(/^PUT \/sessions\/s1\/datachannels\/close /),
    expect.stringMatching(/^PUT \/sessions\/s2\/datachannels\/close /),
    expect.stringMatching(/^PUT \/sessions\/s3\/datachannels\/close /),
  ]);
  // A socket that closes leaves as well, and isn't told.
  const two = peers.get(2)!;
  peers.delete(2);
  relay.closed(2, two);
  await relay.settled;
  expect(told.get(1)!.at(-1)).toEqual({ relay: "gone", n: 1, peer: 9 });
  expect(peers.get(1)!.relay!.peers).toEqual({});
});

test("a peer back on a new connection is heard, and its old connection's close changes nothing", async () => {
  const { peers, told, relay, join } = setup({ 1: 7, 2: 9, 3: 9 });
  await join(1);
  await join(2);
  // Peer 9 again, on connection 3, while 2 is still open: 2's relay ends, and peer 7 hears 9's new one.
  await join(3);
  expect(peers.get(2)!.relay).toBeNull();
  expect(told.get(2)!.at(-1)).toEqual({ relay: "closed", n: 2, why: "" });
  expect(told.get(1)!.slice(-2)).toEqual([{ relay: "gone", n: 1, peer: 9 }, { relay: "peer", n: 1, peer: 9, id: expect.any(Number) }]);
  const heard = peers.get(1)!.relay!.peers["9"]!;
  expect(heard.session).toBe(peers.get(3)!.relay!.session);
  // Connection 2 closing now says nothing to peer 7.
  const two = { ...peers.get(2)!, relay: { ...peers.get(3)!.relay!, session: "s2" } };
  peers.delete(2);
  relay.closed(2, two);
  await relay.settled;
  expect(peers.get(1)!.relay!.peers["9"]).toEqual(heard);
});

test("a word about a relay that ended reaches nothing", async () => {
  const { peers, told, calls, say } = setup({ 1: 7 });
  await say(1, { relay: "join", n: 1 });
  await say(1, { relay: "leave", n: 1 });
  await say(1, { relay: "join", n: 2 });
  calls.length = 0;
  // The page answers the old relay's offer: nothing happens.
  await say(1, { relay: "answer", n: 1, sdp: "stale" });
  await say(1, { relay: "leave", n: 1 });
  expect(calls).toEqual([]);
  expect(peers.get(1)!.relay!.n).toBe(2);
  expect(told.get(1)!.at(-1)).toEqual({ relay: "offer", n: 2, sdp: "offer-s2" });
});

test("only a tagged connection joins, out of turn steps do nothing, and the relay has a cap", async () => {
  const tags: Record<number, number> = { 1: 0 };
  for (let c = 2; c <= MAX_RELAY_PEERS + 2; c++) tags[c] = c;
  const { told, calls, say, join, tick } = setup(tags);
  await say(1, { relay: "join", n: 1 });
  expect(told.get(1)).toEqual([{ relay: "closed", n: 1, why: "the room hasn't let this connection in" }]);
  // `ready` before an answer, or an answer never offered, is ignored.
  await say(2, { relay: "ready", n: 2 });
  await say(2, { relay: "answer", n: 2, sdp: "x" });
  expect(calls).toEqual([]);
  for (let c = 2; c < MAX_RELAY_PEERS + 1; c++) await join(c);
  // The last place is held by a peer that never comes up...
  await say(MAX_RELAY_PEERS + 1, { relay: "join", n: 1 });
  const last = MAX_RELAY_PEERS + 2;
  await say(last, { relay: "join", n: 1 });
  expect(told.get(last)).toEqual([{ relay: "closed", n: 1, why: `the room's relay is full (${MAX_RELAY_PEERS})` }]);
  // ...until its time is up.
  tick(STALL_MS);
  await say(last, { relay: "join", n: 2 });
  expect(told.get(MAX_RELAY_PEERS + 1)!.at(-1)).toEqual({ relay: "closed", n: 1, why: "" });
  expect(told.get(last)!.at(-1)).toEqual({ relay: "offer", n: 2, sdp: expect.any(String) });
});

test("a connection joins only so often, and one with too much waiting is refused", async () => {
  const { told, relay, say } = setup({ 1: 7 });
  for (let n = 1; n <= JOINS + 1; n++) await say(1, { relay: "join", n });
  expect(told.get(1)!.at(-1)).toEqual({ relay: "closed", n: JOINS + 1, why: `this connection joined the relay ${JOINS} times this minute` });
  let taken = true;
  for (let i = 0; i <= MAX_QUEUED; i++) taken = relay.message(1, JSON.stringify({ relay: "ready", n: 1 }));
  expect(taken).toBe(false);
  await relay.settled;
});

test("an SFU that refuses ends that peer's relay, and it may join again", async () => {
  let refusing = true;
  const { peers, told, say, join } = setup({ 1: 7 }, (path) => refusing && path.endsWith("/renegotiate"));
  await say(1, { relay: "join", n: 1 });
  await say(1, { relay: "answer", n: 1, sdp: "a" });
  expect(told.get(1)!.at(-1)).toEqual({ relay: "closed", n: 1, why: "the relay couldn't be reached" });
  expect(peers.get(1)!.relay).toBeNull();
  refusing = false;
  await join(1, 2);
  expect(peers.get(1)!.relay!.stage).toBe("live");
});
