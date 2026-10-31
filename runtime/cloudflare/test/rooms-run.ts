// A built server program's rooms through its Cloudflare host, as the Worker runs them: `rooms` in
// front, a room's Durable Object behind it with its sockets held through the Hibernation API, the
// server object for the store, each object's storage on bun:sqlite. The compiler's suite runs it
// (compiler/tests/tests/suite/http.rs) with a build and a scenario, and checks what each peer was
// sent:
//
//     bun test/rooms-run.ts <build-dir> <scenario.json>
//
// A scenario is a list of steps: `{ "connect": peer }`, `{ "send": peer, "hex": bytes }`,
// `{ "text": peer }` (a text message), `{ "restart": true }` (the room's object is reset: a new
// one over the same storage and sockets) and `{ "route": path, "origin"?: page }` (the router's
// answer's status). It prints `{ sent: { peer: [hex] }, closed: { peer: code }, routes: [status] }`.

import { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { FakeSocket as Socket, storage, workers } from "./fakes.ts";

const [build, scenarioPath] = process.argv.slice(2) as [string, string];
type Step = { connect?: string; send?: string; hex?: string; text?: string; restart?: boolean; route?: string; origin?: string };
const steps = JSON.parse(readFileSync(scenarioPath, "utf8")) as Step[];

workers();

Object.assign(globalThis, {
  WebSocketPair: class {
    0 = new Socket();
    1 = new Socket();
  },
});
// The Workers runtime answers an upgrade with 101 and the client's end; Bun's Response takes no 101.
const NativeResponse = Response;
Object.assign(globalThis, {
  Response: class extends NativeResponse {
    readonly upgraded: boolean;
    constructor(body?: BodyInit | null, init?: ResponseInit & { webSocket?: unknown }) {
      super(body, init?.status === 101 ? { ...init, status: 200 } : init);
      this.upgraded = init?.status === 101;
    }
  },
});

interface Host {
  WrelaServer: new (ctx: unknown, env: unknown) => unknown;
  WrelaRoom: new (ctx: unknown, env: unknown) => { fetch(r: Request): Promise<Response>; webSocketMessage(ws: unknown, m: ArrayBuffer | string): Promise<void>; webSocketClose(ws: unknown): Promise<void>; room: { wake(): Promise<void> } };
  rooms(request: Request, env: unknown): Promise<Response | null>;
}
const host = (await import(join(build, "server/cloudflare.js"))) as Host;
const serverDb = new Database(":memory:");
const roomDb = new Database(":memory:");
const sockets: Socket[] = [];
const env: Record<string, unknown> = { WRELA_ORIGINS: "https://wrela.dev" };
const server = new host.WrelaServer({ storage: storage(serverDb) }, env);
env["WRELA_SERVER"] = { getByName: () => server };
const ctx = { storage: storage(roomDb), acceptWebSocket: (ws: Socket) => sockets.push(ws), getWebSockets: () => sockets.filter((s) => s.closed === null) };
let room = new host.WrelaRoom(ctx, env);
env["WRELA_ROOM"] = { getByName: () => ({ fetch: (r: Request) => room.fetch(r) }) };

const peers = new Map<string, Socket>();
const routes: number[] = [];
const hex = (b: Uint8Array) => Buffer.from(b).toString("hex");
for (const step of steps) {
  if (step.connect !== undefined) {
    const before = sockets.length;
    await room.fetch(new Request("https://wrela.dev/room/a", { headers: { upgrade: "websocket" } }));
    peers.set(step.connect, sockets[before]!);
  } else if (step.send !== undefined) {
    const bytes = Uint8Array.from(Buffer.from(step.hex!, "hex"));
    await room.webSocketMessage(peers.get(step.send), bytes.buffer);
  } else if (step.text !== undefined) {
    await room.webSocketMessage(peers.get(step.text), "a text message");
  } else if (step.restart) {
    room = new host.WrelaRoom(ctx, env);
  } else if (step.route !== undefined) {
    const headers: Record<string, string> = { upgrade: "websocket" };
    if (step.origin !== undefined) headers["origin"] = step.origin;
    const r = await host.rooms(new Request(`https://wrela.dev${step.route}`, { headers }), env);
    routes.push(r === null ? 0 : (r as Response & { upgraded?: boolean }).upgraded ? 101 : r.status);
  }
  await room.room.wake();
}
const sent: Record<string, string[]> = {};
const closed: Record<string, number | null> = {};
for (const [name, s] of peers) {
  sent[name] = s.sent.map(hex);
  closed[name] = s.closed;
}
console.log(JSON.stringify({ sent, closed, routes }));
