// The browser's `http/fetch` (language.md §19.5): a request to the page's own origin, cookies
// sent, no redirect followed, only the headers a client may set and see; and HTTP's bytes against
// the ABI's vectors.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { decodeRequest, decodeResponse, encodeRequest, encodeResponse, httpFetch } from "../src/http.ts";

const hex = (s: string) => new Uint8Array((s.match(/../g) ?? []).map((b) => parseInt(b, 16)));
const vectors = JSON.parse(readFileSync(join(import.meta.dir, "../../abi/vectors.json"), "utf8")).http;
const ORIGIN = "https://wrela.dev";

const ask = (method: string, path: string, query = "", headers: [string, string][] = [], body = new Uint8Array(0)) =>
  encodeRequest({ time: 0n, method, path, query, headers, body });

describe("http/fetch", () => {
  test("HTTP's bytes are the ABI's vectors'", () => {
    const q = decodeRequest(hex(vectors.request.bytes));
    expect(q.path).toBe(vectors.request.path);
    expect(encodeRequest(q)).toEqual(hex(vectors.request.bytes));
    expect(encodeResponse(decodeResponse(hex(vectors.response.bytes)))).toEqual(hex(vectors.response.bytes));
  });

  test("it asks the page's origin with its cookies, and sees only what a client may", async () => {
    let seen: [URL, RequestInit] | null = null;
    const send = (async (url: URL, init: RequestInit) => {
      seen = [url, init];
      return new Response(new Uint8Array([4, 5]), {
        status: 201,
        headers: { "content-type": "application/octet-stream", "set-cookie": "a=1", "x-wrela-page": "2", server: "x" },
      });
    }) as unknown as typeof fetch;
    const answer = decodeResponse(await httpFetch(ask("POST", "/api/run", "id=3", [["x-wrela-csrf", "7f"]], new Uint8Array([1])), ORIGIN, send));
    const [url, init] = seen!;
    expect(url.href).toBe("https://wrela.dev/api/run?id=3");
    expect(init.credentials).toBe("same-origin");
    expect(init.redirect).toBe("error");
    expect(new Headers(init.headers).get("x-wrela-csrf")).toBe("7f");
    expect(answer.status).toBe(201);
    expect(answer.headers).toEqual([["content-type", "application/octet-stream"], ["x-wrela-page", "2"]]);
    expect(answer.body).toEqual(new Uint8Array([4, 5]));
  });

  test("it refuses another origin, a header a client can't set, and a method no host sends", async () => {
    const send = (async () => new Response(null)) as unknown as typeof fetch;
    expect(httpFetch(ask("GET", "//evil.example/x"), ORIGIN, send)).rejects.toThrow("absolute");
    expect(httpFetch(ask("GET", "relative"), ORIGIN, send)).rejects.toThrow("absolute");
    expect(httpFetch(ask("GET", "/x", "", [["cookie", "a=1"]]), ORIGIN, send)).rejects.toThrow("can't set");
    expect(httpFetch(ask("TRACE", "/x"), ORIGIN, send)).rejects.toThrow("isn't a method");
  });

  test("a public read is a GET without cookies, which the HTTP cache may answer", async () => {
    let seen: RequestInit | null = null;
    const send = (async (_url: URL, init: RequestInit) => {
      seen = init;
      return new Response("board", { headers: { "cache-control": "public, max-age=60" } });
    }) as unknown as typeof fetch;
    const answer = decodeResponse(await httpFetch(ask("GET", "/api/board"), ORIGIN, send, true));
    expect(seen!.credentials).toBe("omit");
    expect(seen!.cache).toBe("default");
    expect(seen!.redirect).toBe("error");
    expect(answer.headers).toEqual([["cache-control", "public, max-age=60"]]);
    expect(httpFetch(ask("POST", "/api/board"), ORIGIN, send, true)).rejects.toThrow("a `GET`");
    // An ordinary fetch keeps its cookies and caches nothing.
    await httpFetch(ask("GET", "/api/board"), ORIGIN, send);
    expect(seen!.credentials).toBe("same-origin");
    expect(seen!.cache).toBe("no-store");
  });
});
