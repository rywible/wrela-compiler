// The host as `wrela build` writes it beside a server build (server/cloudflare.js): the Durable
// Object classes for the build's module, the server's instances' and its rooms', the routers for
// the site's Worker (language.md §19.5, §19.6), and `verify`, which a verifier artifact's entry
// calls (server/verifier/main.js).
//
//     import { WrelaServer, WrelaRoom, api, rooms } from "./server/cloudflare.js";
//     export { WrelaServer, WrelaRoom };
//     export default {
//       async fetch(request, env, ctx) {
//         return (await api(request, env, { ctx })) ?? (await rooms(request, env)) ?? env.ASSETS.fetch(request);
//       },
//     };

import module from "./game.wasm";
import { pairOf } from "../../browser/src/net.ts";
import { type ApiOptions, api as apiOf, type Env, roomClass, rooms as roomsOf, serverClass } from "./cloudflare.ts";
import { Server } from "./host.ts";

export const WrelaServer = serverClass(module);
export const WrelaRoom = roomClass(module);
const pair = pairOf(module);
/** The API's response to `request` (under `/api/`), or null for another path. */
export const api = (request: Request, env: Env, options?: ApiOptions) => apiOf(module, request, env, options);
/** A room's WebSocket for `/room/<name>?pair=<id>`, or null for another path. */
export const rooms = (request: Request, env: Env) => roomsOf(request, env, pair);

/** A verifier artifact's call (`std::server::call_build`): a fresh edge instance of the build,
 * with no store and no bindings, hears it as a call from the host (an empty name), and its reply
 * is the answer; null if it didn't reply. Each call is a request of its own to the Workers
 * runtime, which cancels what one left waiting, so no instance outlives its call. */
export function verify(bytes: Uint8Array): Promise<Uint8Array | null> {
  const verifier = new Server(module, null, { log: (line) => console.error(line) });
  return verifier.call("", bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
}
