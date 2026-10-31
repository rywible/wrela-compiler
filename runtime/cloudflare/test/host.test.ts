// The host's parts on their own: HTTP's bytes against the ABI's vectors, the store on bun:sqlite,
// the three signature algorithms through WebCrypto, and what a response may say.

import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Out } from "../../browser/src/http.ts";
import { checkResponse, decodeRequest, decodeResponse, derToRaw, encodeRequest, encodeResponse, MAX_SCAN, random, Store, verify } from "../src/host.ts";

const hex = (s: string) => new Uint8Array((s.match(/../g) ?? []).map((b) => parseInt(b, 16)));
const vectors = JSON.parse(readFileSync(join(import.meta.dir, "../../abi/vectors.json"), "utf8")).http;

function store(): [Store, Database] {
  const db = new Database(":memory:");
  const s = new Store({
    sql: {
      exec(query: string, ...bindings: unknown[]) {
        const rows = db.query(query).all(...(bindings as never[])) as Record<string, unknown>[];
        return Object.assign(rows[Symbol.iterator](), { toArray: () => rows });
      },
    },
    transactionSync: <T>(fn: () => T): T => db.transaction(fn)(),
  });
  return [s, db];
}

const enc = (s: string) => new TextEncoder().encode(s);
const put = (k: Uint8Array, v: Uint8Array) => new Out().push(new Uint8Array([1])).chunk(k).chunk(v).bytes();
const del = (k: Uint8Array) => new Out().push(new Uint8Array([2])).chunk(k).bytes();
const scanBody = (from: Uint8Array, to: Uint8Array, limit: number, down: boolean) =>
  new Out().chunk(from).chunk(to).word(limit).push(new Uint8Array([down ? 1 : 0])).bytes();

function entries(answer: Uint8Array): [string, string][] {
  const view = new DataView(answer.buffer, answer.byteOffset);
  const n = view.getUint32(0, true);
  const out: [string, string][] = [];
  let at = 4;
  const chunk = () => {
    const len = view.getUint32(at, true);
    const s = new TextDecoder().decode(answer.subarray(at + 4, at + 4 + len));
    at += 4 + len;
    return s;
  };
  for (let i = 0; i < n; i++) out.push([chunk(), chunk()]);
  return out;
}

describe("HTTP's bytes", () => {
  test("a request and a response are the ABI's vectors' bytes", () => {
    const q = vectors.request;
    const request = decodeRequest(hex(q.bytes));
    expect(request).toEqual({ time: BigInt(q.time), method: q.method, path: q.path, query: q.query, headers: q.headers, body: hex(q.body) });
    expect(encodeRequest(request)).toEqual(hex(q.bytes));
    const s = vectors.response;
    const response = decodeResponse(hex(s.bytes));
    expect(response).toEqual({ status: s.status, headers: s.headers, body: hex(s.body) });
    expect(encodeResponse(response)).toEqual(hex(s.bytes));
    expect(() => decodeRequest(hex(q.bytes).subarray(0, 20))).toThrow();
    expect(() => decodeRequest(hex(s.bytes))).toThrow("not a request");
  });

  test("a server sets only the headers it may, in lower case, on one line", () => {
    const withHeaders = (headers: [string, string][]) => encodeResponse({ status: 200, headers, body: new Uint8Array(0) });
    expect(checkResponse(withHeaders([["set-cookie", "a=1"], ["x-wrela-page", "2"]])).status).toBe(200);
    expect(() => checkResponse(withHeaders([["access-control-allow-origin", "*"]]))).toThrow("can't set");
    expect(() => checkResponse(withHeaders([["Content-Type", "x"]]))).toThrow("lower case");
    expect(() => checkResponse(withHeaders([["location", "/a\r\nset-cookie: b=2"]]))).toThrow("line break");
  });
});

describe("the store", () => {
  test("reads, scans and writes as wrela-serve's does, an empty key and value too", () => {
    const [s] = store();
    s.commit([new Uint8Array([...put(enc("b/2"), enc("two")), ...put(enc("b/1"), enc("one")), ...put(enc("a"), enc("x"))]), put(enc(""), enc(""))]);
    expect(s.get(enc("b/1"))).toEqual(new Uint8Array([1, ...enc("one")]));
    expect(s.get(enc(""))).toEqual(new Uint8Array([1]));
    expect(s.get(enc("c"))).toEqual(new Uint8Array([0]));
    expect(entries(s.scan(scanBody(enc("b/"), enc("b0"), 10, false)))).toEqual([["b/1", "one"], ["b/2", "two"]]);
    expect(entries(s.scan(scanBody(enc(""), enc(""), 2, true)))).toEqual([["b/2", "two"], ["b/1", "one"]]);
    expect(entries(s.scan(scanBody(enc(""), enc(""), 10, false))).map(([k]) => k)).toEqual(["", "a", "b/1", "b/2"]);
    s.commit([del(enc("a"))]);
    expect(s.get(enc("a"))).toEqual(new Uint8Array([0]));
  });

  test("a malformed write commits nothing of its call", () => {
    const [s] = store();
    expect(() => s.commit([put(enc("k"), enc("v")), new Uint8Array([9, 0, 0, 0, 0])])).toThrow("kind 9");
    expect(s.get(enc("k"))).toEqual(new Uint8Array([0]));
  });

  test("a scan keeps its first entry past the byte limit, and stops before the next", () => {
    const [s, db] = store();
    db.query("INSERT INTO kv (key, value) VALUES (?, ?)").run(enc("a"), new Uint8Array(MAX_SCAN));
    s.commit([put(enc("b"), enc(""))]);
    expect(entries(s.scan(scanBody(enc(""), enc(""), 10, false))).map(([k]) => k)).toEqual(["a"]);
  });
});

describe("cryptography", () => {
  const body = (alg: number, key: Uint8Array, message: Uint8Array, signature: Uint8Array) =>
    new Out().push(new Uint8Array([alg])).chunk(key).chunk(message).chunk(signature).bytes();

  /** WebCrypto's `r‖s` as DER, as an authenticator gives it. */
  function der(raw: Uint8Array): Uint8Array {
    const int = (b: Uint8Array) => {
      let i = 0;
      while (i < b.length - 1 && b[i] === 0 && !(b[i + 1]! & 0x80)) i++;
      const v = b.subarray(i);
      return v[0]! & 0x80 ? [0x02, v.length + 1, 0, ...v] : [0x02, v.length, ...v];
    };
    const both = [...int(raw.subarray(0, 32)), ...int(raw.subarray(32))];
    return new Uint8Array([0x30, both.length, ...both]);
  }

  test("ES256 checks a DER signature over a SEC1 key, and refuses another message", async () => {
    const pair = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
    const key = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
    const raw = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, pair.privateKey, enc("the message")));
    const sig = der(raw);
    expect(derToRaw(sig)).toEqual(raw);
    expect(await verify(body(1, key, enc("the message"), sig))).toEqual(new Uint8Array([1]));
    expect(await verify(body(1, key, enc("another"), sig))).toEqual(new Uint8Array([0]));
    expect(await verify(body(1, key, enc("the message"), enc("junk")))).toEqual(new Uint8Array([0]));
    expect(verify(body(1, enc("not a key"), enc("m"), sig))).rejects.toThrow("isn't one of the algorithm's");
  });

  test("a DER signature reads only as strict DER", () => {
    const ok = new Uint8Array([0x30, 6, 0x02, 1, 5, 0x02, 1, 7]);
    expect(derToRaw(ok)?.[31]).toBe(5);
    expect(derToRaw(new Uint8Array([0x30, 7, 0x02, 2, 0, 5, 0x02, 1, 7]))).toBeNull(); // a needless 0
    expect(derToRaw(new Uint8Array([0x30, 6, 0x02, 1, 0x85, 0x02, 1, 7]))).toBeNull(); // negative
    expect(derToRaw(new Uint8Array([0x30, 7, 0x02, 1, 5, 0x02, 1, 7, 0]))).toBeNull(); // past its end
  });

  test("Ed25519 and RS256 check a signature and refuse another message", async () => {
    const ed = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
    const edKey = new Uint8Array(await crypto.subtle.exportKey("raw", ed.publicKey));
    const edSig = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, ed.privateKey, enc("hello")));
    expect(await verify(body(2, edKey, enc("hello"), edSig))).toEqual(new Uint8Array([1]));
    expect(await verify(body(2, edKey, enc("hellO"), edSig))).toEqual(new Uint8Array([0]));
    expect(verify(body(2, edKey.subarray(1), enc("hello"), edSig))).rejects.toThrow("32 bytes");
    const rsa = (await crypto.subtle.generateKey(
      { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
      true,
      ["sign", "verify"],
    )) as CryptoKeyPair;
    const jwk = await crypto.subtle.exportKey("jwk", rsa.publicKey);
    const b64 = (s: string) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
    // As std's `rsa_key` makes it from COSE, which may keep a leading zero.
    const rsaKey = new Out().chunk(new Uint8Array([0, ...b64(jwk.n!)])).chunk(b64(jwk.e!)).bytes();
    const rsaSig = new Uint8Array(await crypto.subtle.sign({ name: "RSASSA-PKCS1-v1_5" }, rsa.privateKey, enc("payload")));
    expect(await verify(body(3, rsaKey, enc("payload"), rsaSig))).toEqual(new Uint8Array([1]));
    expect(await verify(body(3, rsaKey, enc("payloaD"), rsaSig))).toEqual(new Uint8Array([0]));
  });

  test("randomness is the size asked and bounded", () => {
    const n = new Out().word(16).bytes();
    expect(random(n).length).toBe(16);
    expect(random(n)).not.toEqual(random(n));
    expect(() => random(new Out().word(1025).bytes())).toThrow("more than 1024");
  });
});
