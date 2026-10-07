// The imports of another thread's instance of the program: a helper's, the ticker's, the voice's.
// It needs only the ABI, so the audio worklet's small bundle can have it too.

import { HOST_FUNCTIONS, IMPORT_MEMORY, IMPORT_MODULE } from "./abi.gen.ts";

/** The shared `memory`, and host functions that refuse to run: they throw a `Refusal` that says
 * `who` can't call the host. Another thread's code records no GPU work and makes no requests (its
 * effects are checked). */
export function refusingImports(
  memory: WebAssembly.Memory,
  who: string,
  Refusal: new (message: string) => Error,
): WebAssembly.Imports {
  const refuse = () => {
    throw new Refusal(`${who} can't call the host`);
  };
  return { [IMPORT_MODULE]: { [IMPORT_MEMORY]: memory, ...Object.fromEntries(HOST_FUNCTIONS.map(([name]) => [name, refuse])) } };
}
