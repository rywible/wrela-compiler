import { expect, spyOn, test } from "bun:test";
import { HOST_FUNCTIONS, IMPORT_CLOCK, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT } from "../src/abi.gen.ts";
import { clockImport, printed } from "../src/imports.ts";

test("thread imports initialize and read strict UTF-8 without TextDecoder", async () => {
  const bundle = await Bun.build({ entrypoints: [new URL("../src/imports.ts", import.meta.url).pathname], target: "browser", format: "esm" });
  expect(bundle.success).toBe(true);
  const path = `data:text/javascript;base64,${Buffer.from(await bundle.outputs[0]!.text()).toString("base64")}`;
  const decoder = Object.getOwnPropertyDescriptor(globalThis, "TextDecoder")!;
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    Reflect.deleteProperty(globalThis, "TextDecoder");
    // A fresh bundle models AudioWorkletGlobalScope, where TextDecoder is absent. Bun caches
    // file imports even with a URL query, which would hide a previously constructed decoder.
    const fresh: typeof import("../src/imports.ts") = await import(path);
    const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    const imports = fresh.refusingImports(memory, "the audio thread", Error);
    const host = imports[IMPORT_MODULE]!;
    expect(host[IMPORT_MEMORY]).toBe(memory);
    (host[IMPORT_CLOCK] as (ptr: number) => void)(8);
    expect(Number.isFinite(new DataView(memory.buffer).getFloat64(8, true))).toBe(true);
    expect(() => (host[IMPORT_PHASE] as (ptr: number, len: number) => void)(0, 0)).not.toThrow();
    expect(() => (host[HOST_FUNCTIONS[0]![0]] as () => void)()).toThrow("the audio thread can't call the host");
    const bytes = new Uint8Array(memory.buffer);
    const line = "\uFEFFvoice é🦀\0%";
    const encoded = new TextEncoder().encode(line);
    bytes.set(encoded, 16);
    (host[IMPORT_PRINT] as (ptr: number, len: number) => void)(16, encoded.length);
    (host[IMPORT_PRINT] as (ptr: number, len: number) => void)(16, 0);
    expect(log.mock.calls.map(([line]) => line)).toEqual([`wrela: ${line}`, "wrela: "]);
    (host[IMPORT_PHASE] as (ptr: number, len: number) => void)(16, encoded.length);
    for (const invalid of [[0xc3], [0xc0, 0x80], [0xed, 0xa0, 0x80], [0xf4, 0x90, 0x80, 0x80], [0x80]]) {
      bytes.set(invalid, 16);
      expect(() => (host[IMPORT_PRINT] as (ptr: number, len: number) => void)(16, invalid.length)).toThrow("print: the text isn't UTF-8");
      expect(() => (host[IMPORT_PHASE] as (ptr: number, len: number) => void)(16, invalid.length)).toThrow("phase: the text isn't UTF-8");
    }
    expect(() => (host[IMPORT_PRINT] as (ptr: number, len: number) => void)(65_535, 2)).toThrow("print(65535, 2) reaches past the end of the program's memory");
  } finally {
    Object.defineProperty(globalThis, "TextDecoder", decoder);
    log.mockRestore();
  }
});

for (const shared of [false, true]) {
  test(`the clock writes to ${shared ? "shared" : "unshared"} memory before and after growth`, () => {
    const memory = new WebAssembly.Memory({ initial: 1, maximum: 2, shared });
    const clock = clockImport(memory);
    const now = spyOn(performance, "now").mockReturnValue(1234);
    try {
      const expected = (performance.timeOrigin + 1234) / 1000;
      clock(8);
      clock(16);
      expect(new DataView(memory.buffer).getFloat64(8, true)).toBe(expected);
      expect(new DataView(memory.buffer).getFloat64(16, true)).toBe(expected);
      expect(() => clock(65_532)).toThrow(RangeError);
      memory.grow(1);
      clock(65_532);
      expect(new DataView(memory.buffer).getFloat64(65_532, true)).toBe(expected);
      expect(() => clock(131_068)).toThrow(RangeError);
    } finally {
      now.mockRestore();
    }
  });
}

test("consecutive printed lines decode independently, rejecting malformed text and keeping a BOM", () => {
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const bytes = new Uint8Array(memory.buffer);
  bytes.set([0xc3], 8); // A partial UTF-8 sequence must not carry into the next print.
  bytes.set(new TextEncoder().encode("\uFEFFé"), 16);
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    expect(() => printed(memory, 8, 1)).toThrow("print: the text isn't UTF-8");
    expect(printed(memory, 16, 5)).toBe("\uFEFFé");
    expect(printed(memory, 16, 0)).toBe("");
    expect(log.mock.calls.map(([line]) => line)).toEqual(["wrela: \uFEFFé", "wrela: "]);
    expect(() => printed(memory, 65_535, 2)).toThrow("print(65535, 2) reaches past the end of the program's memory");
  } finally {
    log.mockRestore();
  }
});
