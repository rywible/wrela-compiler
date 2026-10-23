import { expect, test } from "bun:test";
import { Live, type LiveEvent } from "../src/live.ts";
import type { Program } from "../src/program.ts";

const base = "http://127.0.0.1:8000/game/";

async function fromServer(events: LiveEvent[]): Promise<{ live: Live; requests: { url: string; init: RequestInit | undefined }[]; restore: () => void }> {
  const saved = globalThis.fetch;
  const requests: { url: string; init: RequestInit | undefined }[] = [];
  const stop = new AbortController();
  globalThis.fetch = (async (url: URL, init?: RequestInit) => {
    requests.push({ url: url.href, init });
    if (init?.method === "POST") return new Response(null, { status: 204 });
    stop.abort();
    return new Response(JSON.stringify(events));
  }) as typeof fetch;
  const live = new Live(base);
  try { await live.start(stop.signal); }
  catch (e) { globalThis.fetch = saved; throw e; }
  return { live, requests, restore: () => { globalThis.fetch = saved; } };
}

test("live updates stay at the local server and only load its builds", async () => {
  const { live, requests, restore } = await fromServer([
    { seq: 1, kind: "literals", values: [[2, 9]] },
    { seq: 2, kind: "build", base: "/next/" },
  ]);
  const values: [number, number][] = [];
  try {
    expect(live.take({ setLiteral: (i: number, v: number) => { values.push([i, v]); } } as Program))
      .toBe("http://127.0.0.1:8000/next/");
    expect(values).toEqual([[2, 9]]);
    expect(requests[0]!.url).toBe("http://127.0.0.1:8000/live/next?after=0");
    expect(requests[0]!.init).toMatchObject({ cache: "no-store", redirect: "error" });
    live.drawn(4);
    expect(requests[1]).toMatchObject({
      url: "http://127.0.0.1:8000/live/shown",
      init: { method: "POST", body: '{"seq":1,"frame":4}', redirect: "manual" },
    });
  } finally { restore(); }
});

test("live updates refuse builds on another origin or scheme", async () => {
  for (const target of ["https://attacker.example/build/", "blob:http://127.0.0.1:8000/identifier", "http://user:pass@127.0.0.1:8000/build/"]) {
    const { live, restore } = await fromServer([{ seq: 1, kind: "build", base: target }]);
    try { expect(() => live.take({} as Program)).toThrow("page's origin"); }
    finally { restore(); }
  }
});
