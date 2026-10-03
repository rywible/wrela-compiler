// The text of anything thrown.

/** `e`'s message if it's an Error, else `e` as a string. */
export const errorMessage = (e: unknown): string => (e instanceof Error ? e.message : String(e));
