// JSON that both hosts can read: match serde_json's strings, finite numbers and depth limit.

/** In a `u` regex a surrogate pair is one code point, so this finds only unpaired ones. */
const LONE_SURROGATE = /\p{Cs}/u;
const MAX_NESTING = 127;

/** Parses JSON without replacing malformed Unicode, accepting infinite numbers or reading
 * more deeply nested arrays and objects than the native host. */
export function parseJson(text: string): unknown {
  const value: unknown = JSON.parse(text, (key, value: unknown) => {
    if (LONE_SURROGATE.test(key) || (typeof value === "string" && LONE_SURROGATE.test(value))) {
      throw new Error("a string has a lone surrogate");
    }
    if (typeof value === "number" && !Number.isFinite(value)) throw new Error("a number is out of range");
    return value;
  });
  const stack: [unknown, number][] = [[value, 0]];
  while (stack.length > 0) {
    const [item, depth] = stack.pop()!;
    if (item === null || typeof item !== "object") continue;
    if (depth + 1 > MAX_NESTING) throw new Error(`arrays and objects nest more than ${MAX_NESTING} deep`);
    for (const child of Object.values(item)) stack.push([child, depth + 1]);
  }
  return value;
}
