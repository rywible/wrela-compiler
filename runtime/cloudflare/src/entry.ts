// The host as `wrela build` writes it beside a server build (server/cloudflare.js): the Durable
// Object class for the build's module, and the API's router for the site's Worker.
//
//     import { WrelaServer, api } from "./server/cloudflare.js";
//     export { WrelaServer };
//     export default { async fetch(request, env) { return (await api(request, env)) ?? env.ASSETS.fetch(request); } };

import module from "./game.wasm";
import { api, serverClass } from "./cloudflare.ts";

export const WrelaServer = serverClass(module);
export { api };
