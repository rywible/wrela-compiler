// What the host uses of the Workers runtime's types (Cloudflare's `@cloudflare/workers-types`
// has them all; these few keep the host free of another dependency).

declare module "cloudflare:workers" {
  export abstract class DurableObject<Env = unknown> {
    protected ctx: DurableObjectState;
    protected env: Env;
    constructor(ctx: DurableObjectState, env: Env);
  }
}

interface DurableObjectState {
  storage: import("./host.ts").Storage & { setAlarm(at: number): Promise<void> };
}

declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
