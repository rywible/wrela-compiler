// One build hands its place to another in the same page (`std::reload::prepare`, `unprepare` and
// `hand_over`), as in the native host (runtime/native/src/handover.rs): a game made of several
// builds, such as a tower of floors, each started when the player reaches it.
//
// - Prepare: the build's files are fetched, its WASM compiled and the pipelines the device
//   doesn't hold yet built, while the program runs. They wait until a hand-over takes them or
//   the program lets the build go.
// - Hand over: after the frame that asked, the build is got ready (then, if it isn't), and the
//   render worker stops the running program and starts the build with what the old one kept
//   (worker.ts). A build that can't be got ready fails the request, and the old program goes on.

import { errorMessage } from "./errors.ts";
import { type PipelineCache, preparePipelines } from "./gpu.ts";
import { type Build, type Fetch, loadBuild } from "./loader.ts";
import { pathUrl } from "./path.ts";
import { type Builds, type Compiled, Program } from "./program.ts";

/** A build got ready: fetched, compiled, and every pipeline built. */
export interface Ready extends Build {
  compiled: Compiled;
  /** Its pipelines, by key (`preparePipelines`). */
  pipelines: PipelineCache;
}

/** A hand-over a program asked for: the build's base, and how to fail the request. */
export interface Asked {
  base: string;
  fail(why: unknown): void;
}

/** A build being got ready, or ready: its pipelines, once they're started. */
interface Preparing {
  ready: Promise<Ready>;
  pipelines: PipelineCache | null;
  /** Rejected when the program lets the build go: the `prepare` requests still waiting fail. */
  letGo: PromiseWithResolvers<never>;
}

/** The builds the running program got ready, and the hand-over it asked for. */
export class HandOvers implements Builds {
  readonly #prepared = new Map<string, Preparing>();
  #asked: Asked | null = null;

  /**
   * `screenFormat` gives the format the screen's pipelines draw in now (`ScreenTarget.format`,
   * which a `ConfigureScreen` changes). `base`
   * gives the running build's base, which paths are from. `held` gives the pipelines the device
   * holds: the running build's, and those kept for the way back. `fetchFn` fetches the builds'
   * files.
   */
  constructor(
    readonly device: GPUDevice,
    readonly screenFormat: () => GPUTextureFormat,
    readonly base: () => string,
    readonly held: () => readonly PipelineCache[],
    readonly fetchFn?: Fetch,
    /** Whether a build's WASM is hashed as it compiles (the tick log's header). */
    readonly hashWasm = false,
  ) {}

  /** The base of the build at `path`, from the running build's. */
  baseOf(path: string): string {
    return pathUrl(this.base(), `${path}/`).href;
  }

  prepare(path: string): Promise<void> {
    const base = this.baseOf(path);
    const ready = this.ready(base);
    return Promise.race([ready, this.#prepared.get(base)!.letGo.promise]).then(() => {});
  }

  unprepare(path: string): void {
    // A build still on its way ends all the same; nothing keeps what it built, and the requests
    // waiting on it fail, as in the native host.
    const base = this.baseOf(path);
    this.#prepared.get(base)?.letGo.reject(new Error(`the build at ${base} was let go before it was ready`));
    this.#prepared.delete(base);
  }

  handOver(path: string): Promise<void> {
    // The last one asked is the one carried out.
    this.#asked?.fail(new Error("a later hand-over took its place"));
    const asked = Promise.withResolvers<void>();
    this.#asked = { base: this.baseOf(path), fail: asked.reject };
    return asked.promise;
  }

  /** The hand-over the program asked for since this was last called, if it asked: taken. */
  takeAsked(): Asked | null {
    const asked = this.#asked;
    this.#asked = null;
    return asked;
  }

  /** The build at `base`, ready: got ready now, unless it is or it's on its way. Rejected with
   * why if it can't be, then and each time after. */
  ready(base: string): Promise<Ready> {
    let p = this.#prepared.get(base);
    if (!p) {
      const entry = { pipelines: null as PipelineCache | null };
      const ready = this.#getReady(base, (started) => (entry.pipelines = started));
      ready.catch(() => {});
      const letGo = Promise.withResolvers<never>();
      letGo.promise.catch(() => {});
      p = Object.assign(entry, { ready, letGo });
      this.#prepared.set(base, p);
    }
    return p.ready;
  }

  /** Forgets the build at `base`: the hand-over to it took what was got ready. */
  take(base: string): void {
    this.#prepared.delete(base);
  }

  /** Gets the build at `base` ready; `started` is given its pipelines once they're started. */
  async #getReady(base: string, started: (pipelines: PipelineCache) => void): Promise<Ready> {
    try {
      const build = await loadBuild(base, this.fetchFn);
      const others = [...this.#prepared.values()].map((p) => p.pipelines).filter((c) => c !== null);
      const pipelines = preparePipelines(this.device, build.manifest, build.shaders, this.screenFormat(), [...this.held(), ...others]);
      started(pipelines);
      const [compiled] = await Promise.all([Program.compile(build.wasm, build.manifest.wasm_abi, this.hashWasm), Promise.all(pipelines.values())]);
      return { ...build, compiled, pipelines };
    } catch (e) {
      throw new Error(`the build at ${base} isn't ready: ${errorMessage(e)}`);
    }
  }
}
