// The host as `wrela build` writes it beside a server build (server/cloudflare.js): the Durable
// Object classes for the build's module, the server's and its rooms', and the routers for the
// site's Worker (language.md §19.5, §19.6).
//
//     import { WrelaServer, WrelaRoom, api, rooms } from "./server/cloudflare.js";
//     export { WrelaServer, WrelaRoom };
//     export default {
//       async fetch(request, env) {
//         return (await api(request, env)) ?? (await rooms(request, env)) ?? env.ASSETS.fetch(request);
//       },
//     };

import module from "./game.wasm";
import { pairOf } from "../../browser/src/net.ts";
import { api, roomClass, rooms as roomsOf, serverClass } from "./cloudflare.ts";

export const WrelaServer = serverClass(module);
export const WrelaRoom = roomClass(module);
const pair = pairOf(module);
/** A room's WebSocket for `/room/<name>?pair=<id>`, or null for another path. */
export const rooms = (request: Request, env: import("./cloudflare.ts").Env) => roomsOf(request, env, pair);
export { api };
