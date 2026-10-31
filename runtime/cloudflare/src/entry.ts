// The host as `wrela build` writes it beside a server build (server/cloudflare.js): the Durable
// Object class for the build's module, and the API's router for the site's Worker.
//
//     import { WrelaServer, api } from "./server/cloudflare.js";
//     export { WrelaServer };
//     export default { async fetch(request, env) { return (await api(request, env)) ?? env.ASSETS.fetch(request); } };

import module from "./game.wasm";
import { api, pairOf, roomClass, rooms as roomsOf, serverClass } from "./cloudflare.ts";

export const WrelaServer = serverClass(module);
export const WrelaRoom = roomClass(module);
const pair = pairOf(module);
/** A room's WebSocket for `/room/<name>?pair=<id>`, or null for another path. */
export const rooms = (request: Request, env: import("./cloudflare.ts").Env) => roomsOf(request, env, pair);
export { api };
