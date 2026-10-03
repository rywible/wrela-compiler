// The state hash: FNV-1a 64 over every byte a program submits, in order (runtime/abi/src/hash.rs).
// Kept as two u32 halves so hashing costs no BigInt work per byte.

import { FNV_OFFSET, FNV_PRIME } from "./abi.gen.ts";

const PRIME_LO = Number(FNV_PRIME & 0xffff_ffffn); // 0x1b3
const PRIME_HI = Number(FNV_PRIME >> 32n); // 0x100

export class StateHash {
  #hi = Number(FNV_OFFSET >> 32n);
  #lo = Number(FNV_OFFSET & 0xffff_ffffn);

  update(bytes: Uint8Array): void {
    let hi = this.#hi;
    let lo = this.#lo;
    for (let i = 0; i < bytes.length; i++) {
      lo = (lo ^ bytes[i]!) >>> 0;
      // (hi:lo) * (PRIME_HI:PRIME_LO) mod 2^64. Every product below is exact in a double:
      // lo * PRIME_LO < 2^41, and hi's terms are reduced mod 2^32 before they're added.
      const low = lo * PRIME_LO;
      const carry = Math.floor(low / 0x1_0000_0000);
      hi = (Math.imul(hi, PRIME_LO) + Math.imul(lo, PRIME_HI) + carry) >>> 0;
      lo = low >>> 0;
    }
    this.#hi = hi;
    this.#lo = lo;
  }

  /** Sixteen lowercase hex digits, as both hosts print it. */
  hex(): string {
    return this.#hi.toString(16).padStart(8, "0") + this.#lo.toString(16).padStart(8, "0");
  }
}
