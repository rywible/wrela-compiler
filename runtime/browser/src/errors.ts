// The text of anything thrown.

/** `e`'s message if it's an Error, else `e` as a string. */
export const errorMessage = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** This browser or machine can't run a program at all: it has no WebGPU, no hardware adapter or
 * no OffscreenCanvas. The page says what a program needs, not that it failed. */
export class UnsupportedError extends Error {
  constructor(why: string) {
    super(why);
    this.name = "UnsupportedError";
  }
}
