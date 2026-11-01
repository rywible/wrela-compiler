// A verifier artifact's entry (server/verifier/main.js, language.md §19.5): what the Worker Loader
// runs for `std::server::call_build`, with no bindings and no network. Its `verify(bytes)` gives
// the bytes to the build's edge instance as a call, and answers with its reply (null: none).
// The host module beside it (cloudflare.js) imports the build's module (game.wasm).

import { WorkerEntrypoint } from "cloudflare:workers";
import { verify } from "./cloudflare.js";

export default class Verifier extends WorkerEntrypoint {
  async verify(bytes) {
    return verify(bytes);
  }
}
