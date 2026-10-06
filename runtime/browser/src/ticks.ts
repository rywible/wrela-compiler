// The tick log (runtime/abi `ticks`): the ticks the ticker's thread ran in test mode, as bytes a
// native host replays (`wrela-host --replay`). The same bytes as the Rust writer's (vectors).

import { EVENT_SIZE, TICK_LOG_HEADER_LEN, TICK_LOG_VERSION } from "./abi.gen.ts";
import { StateHash } from "./hash.ts";

/** FNV-1a 64 of a build's WASM: which build a log is for. */
export function wasmHash(wasm: Uint8Array): bigint {
  const h = new StateHash();
  h.update(wasm);
  return BigInt(`0x${h.hex()}`);
}

/** A tick log being written: its header, then each tick's records and state hash. */
export class TickLogWriter {
  readonly #ticks: { records: Uint8Array; hash: bigint }[] = [];

  constructor(
    readonly wasmHash: bigint,
    readonly hz: number,
    readonly first: bigint,
  ) {}

  /** The next tick: its records (EVENT_SIZE bytes each, oldest first) and its state hash. */
  push(records: Uint8Array, hash: bigint): void {
    this.#ticks.push({ records: records.slice(), hash });
  }

  get length(): number {
    return this.#ticks.length;
  }

  /** The log's bytes. */
  encode(): Uint8Array<ArrayBuffer> {
    const size = this.#ticks.reduce((n, t) => n + 16 + t.records.length, TICK_LOG_HEADER_LEN);
    const out = new Uint8Array(size);
    const view = new DataView(out.buffer);
    out.set([0x57, 0x52, 0x54, 0x4c], 0); // "WRTL"
    view.setUint32(4, TICK_LOG_VERSION, true);
    view.setBigUint64(8, this.wasmHash, true);
    view.setUint32(16, this.hz, true);
    view.setUint32(20, 0, true);
    view.setBigUint64(24, this.first, true);
    let at = TICK_LOG_HEADER_LEN;
    this.#ticks.forEach((t, k) => {
      view.setUint32(at, k, true);
      view.setUint32(at + 4, t.records.length / EVENT_SIZE, true);
      out.set(t.records, at + 8);
      at += 8 + t.records.length;
      view.setBigUint64(at, t.hash, true);
      at += 8;
    });
    return out;
  }
}
