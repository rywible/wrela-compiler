# Plan: a Cloudflare host for server builds (HTTP in 0.13.0, WebSockets next)

Ryan decided on 2026-10-31 to run every Wrela server on Cloudflare Workers and Durable Objects
instead of Fly.io: Cloudflare's protection (floods, firewall and rate rules, bot checks, no
server address to attack), one vendor with wrela.dev, and a server build is already WASM, which
a Durable Object runs. Cloudflare's proxy can't carry WebTransport, and a server build runs today
only on the native `wrela-serve`, only over WebTransport (spec/language.md §19.3). This plan
gives a server build a second host. Plain HTTP requests come first, under the site's own origin,
for sign-in and the boards. WebSockets come next, for live play.

The first user is the console's accounts and boards server in rywible/wrela
(rywible/wrela#345: `platform/server.wrela`, `console/server/serve.wrela`). It uses `std::store`,
`std::crypto` (random; ES256, Ed25519 and RS256 verify), `std::webauthn` and the `std::net`
server events. It keeps its WebAuthn challenges in memory per connection and its own clock in the
store; on this host both move (§1, §4).

Scopes, each its own pull request and release (AGENTS.md rule 1):

1. **HTTP** (this plan's §1–§9; toolchain 0.13.0). The plan itself lands with it.
2. **WebSockets for rooms** (§10; the release after).

Constraints for both: **no IP address is ever stored or logged**, by the host or by anything it
gives a program. rywible/wrela is not edited here; what it must change is listed per release
(§11). Ryan merges.

## 0. Where the pieces live

| Piece | Where |
|---|---|
| The wire formats (a request's bytes, a response's, the services' names) | `runtime/abi/src/http.rs`, and their TypeScript (`runtime/browser/src/abi.gen.ts`, generated) |
| The program's side | `compiler/std/http.wrela` (new); `std::time::wall_clock` |
| The host | `runtime/cloudflare` (new, TypeScript, built with Bun like `runtime/browser`). `src/host.ts` runs a server build's instance (imports, events, requests, the store over a SQL interface, crypto over WebCrypto); `src/cloudflare.ts` is the Durable Object and the Worker's router. `dist/cloudflare.js` is checked in and embedded in the compiler |
| The build | `wrela build` writes `server/cloudflare.js` (the host) and `server/cloudflare.json` (the Wrangler members it needs) beside `server/game.wasm` |
| The browser's side | `runtime/browser/src/http.ts`: HTTP's bytes, which the Cloudflare host imports too, and the `http/fetch` service (`platform.ts` answers it) |
| The native host | `wrela_host::services` answers `time/unix` too, so a program runs the same on `wrela-serve` |
| The spec | §15's hosts table, §19.5 (new: a server over HTTP), §19.4 (the time of day) |

## 1. The HTTP surface for a program

**An HTTP request is a short connection.** It reuses the events a server already reads
(§17.2), so a program that serves rooms keeps its shape, and no event kind is added (0.12.0, in
#45, takes event kind 16):

1. `Event::Connection { conn, open: true, tag: 0 }`: a request came. Its connection number is
   new: a host never uses one twice while the instance runs.
2. `Event::Message { conn, .. }`: the request, whose payload `std::http::request(conn, bytes)`
   reads. A payload that isn't a request reads as `None`.
3. The program answers once, with `std::http::respond(conn, response)`: a command, never
   answered, carried out once the call's writes are committed, as `send` is (§19.4).
4. `Event::Connection { conn, open: false }` on a later call: the response went, the client went
   away, or the request timed out (§5). The program frees what it kept for `conn`.

`send` and `close` on a request's connection, and a second `respond`, fail the call as the
program's (it misused the connection: 500). `tag` and `keep`/`compact` fail the call on this
host for now: an HTTP instance has no journal (§10 gives rooms theirs).

**A request** (`std::http::Request`):

| Field | What |
|---|---|
| `conn` | its connection |
| `method` | `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE` or `OPTIONS`; the Worker refuses others (405) |
| `path` | the URL's path, as sent, percent-encoding kept (`/api/me`) |
| `query` | what follows `?`, without it (empty if none) |
| `headers` | the headers a server may see (below), names lower-case, in name order with a name's values joined (as the Fetch `Headers` iteration gives them) |
| `body` | its bytes |
| `time` | the host's wall clock when it took the request, milliseconds since the Unix epoch (§4) |

`Request::header(name)` gives a header's value, and `Request::cookie(name)` a cookie's
(`Cookie: a=1; b=2`, RFC 6265 §4.2; the first of a name wins).

**The headers a server may see**, an allowlist kept in `runtime/abi` (the host drops every other
header before the Durable Object is called): `accept`, `accept-language`, `content-type`,
`cookie`, `if-none-match`, `origin`, `sec-fetch-dest`, `sec-fetch-mode`, `sec-fetch-site`,
`sec-fetch-user`, and any `x-wrela-*` (a program's own, such as a CSRF token). Never an
address: not `cf-connecting-ip`, `cf-connecting-ipv6`, `x-forwarded-for`, `x-real-ip`,
`true-client-ip`, nor `cf-ipcountry`, `cf-ray` or `user-agent` (open question 1).

**A response** (`std::http::Response`): a final status (200–599), headers, a body (none for
204, 205 and 304: the Workers runtime refuses one, as it refuses a status below 200).

- The headers a server may set, also an allowlist: `cache-control`, `content-type`,
  `content-disposition`, `etag`, `last-modified`, `location`, `retry-after`, `set-cookie`
  (several), `vary`, `www-authenticate`, and `x-wrela-*`. Another fails the call, as a malformed
  command does.
- The host adds `cache-control: no-store` when the program sets none, and
  `x-content-type-options: nosniff`, `referrer-policy: same-origin`, and
  `content-security-policy: default-src 'none'` (no response of the API is a page).
- A response is one host request: at most 1 MiB less its headers (`std::host::request`'s
  bound). A larger body (the accounts' export with its replays) is paged by the program.
- `std::http::session_cookie(name, value, max_age)` writes a first-party session cookie:
  `__Host-<name>=<value>; Path=/; Max-Age=<s>; Secure; HttpOnly; SameSite=Strict` (RFC 6265bis
  §4.1.3.2: `__Host-` requires Secure, `Path=/` and no Domain). A `max_age` of 0 clears it.

**The byte layouts** are std's and the host's, written once in `runtime/abi/src/http.rs` with
test vectors that both sides check: little-endian `u32` lengths, `WHQ1` before a request and
`WHR1` before a response so a stray message never reads as one.

## 2. Where the store lives: in the server's own Durable Object

**Decision: one SQLite-backed Durable Object, "the server object", is both the store and the
place the program's HTTP instance runs.** D1 is not used.

Why:

- **Strong consistency, without extra machinery.** A Durable Object is single-threaded, and its
  storage is "transactional and strongly consistent", private to the object
  ([SQLite storage API](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/)).
  Every read and write the program makes goes through one object in one order, which is what
  `std::store` promises on `wrela-serve` (one SQLite file, §19.4). Ryan favours strong
  consistency.
- **The call's writes commit together.** `ctx.storage.transactionSync(fn)` runs a synchronous
  callback in one transaction and rolls it back if the callback throws (same page). A call's
  `store/write` commands become one `transactionSync`, then its responses go, exactly as
  `wrela_host::rooms` commits before it sends.
- **Reads are synchronous and local.** `ctx.storage.sql.exec` runs in the object's own thread,
  "effectively zero-latency because compute runs in the same process as the data"
  ([placement](https://developers.cloudflare.com/workers/configuration/placement/)). A `get`
  or `scan` is answered before the call returns, for the next call, with no network hop.
- **The same table.** `CREATE TABLE kv (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT
  ROWID`, the same SQL as `wrela_host::services`, so a scan's order and bounds are the same.
- **D1 would add a hop and not more consistency.** D1 is itself "backed by a single Durable
  Object" and processes queries one at a time
  ([D1 limits](https://developers.cloudflare.com/d1/platform/limits/)). Its read replicas are
  sequentially consistent only through the Sessions API
  ([read replication](https://developers.cloudflare.com/d1/best-practices/read-replication/)),
  and every query is an asynchronous call from the Worker. Its one advantage, reads near the
  reader, matters little for an accounts API whose every write goes to one primary anyway.

Bounds: an object holds up to 10 GB
([what are Durable Objects](https://developers.cloudflare.com/durable-objects/concepts/what-are-durable-objects/)),
and one object handles about 500–1,000 simple requests a second
([rules of Durable Objects](https://developers.cloudflare.com/durable-objects/best-practices/rules-of-durable-objects/)).
That is ample for sign-in and boards. A server that outgrows one object shards its store, which
is a later design (open question 4). Point-in-time recovery (30 days) comes with SQLite-backed
objects (same page as the storage API), which is the store's backup.

When rooms come (§10), each room is its own object, and its store requests reach the server
object over RPC. A request is answered on a later call anyway, so the hop fits the contract.

## 3. `std::crypto` on WebCrypto

**Decision: WebCrypto (`crypto.subtle`), not cryptography compiled into the host.** The Workers
runtime implements ECDSA, Ed25519 and RSASSA-PKCS1-v1_5 sign and verify
([Web Crypto](https://developers.cloudflare.com/workers/runtime-apis/web-crypto/)), natively, which
is "significantly faster than performing them purely in JavaScript" (same page). It keeps
§19.4's rule that the host checks signatures with vetted libraries and std does none of its own,
and it adds nothing to the bundle.

How each request maps:

- `crypto/random`: `crypto.getRandomValues`, at most `MAX_RANDOM` (1 KiB).
- ES256: the key is imported as `raw` (the SEC1 point) on `P-256`. The DER signature is turned
  into the 64-byte `r‖s` form WebCrypto takes; a malformed one, or one whose numbers don't fit,
  is "not the key's" (false), as on `wrela-serve`.
- Ed25519: `raw` 32-byte key.
- RS256: the modulus and exponent become a JWK (`kty: RSA`) for `RSASSA-PKCS1-v1_5` with
  SHA-256.
- A key the import refuses fails the request, as a key that isn't the algorithm's does natively.

WebCrypto is asynchronous. The host awaits a call's verifications before the next call, so a
`verify` is still answered on the next call.

Known difference: `wrela-serve`'s Ed25519 is `ed25519-dalek`'s `verify_strict`, which also
refuses small-order keys; WebCrypto doesn't promise that. A passkey's key is the
authenticator's, and the accounts server only checks signatures under keys it registered, so the
difference doesn't reach it. It is recorded in the spec as a host difference.

## 4. The time of day

A server has had no time of day (§19.3: `serve`'s `time` counts from the server's start), so the
accounts server keeps a clock in its store that stands still while it's down. Two ways in now:

- **Each request carries it** (`Request::time`): the host's `Date.now()` when it took the request.
  No round trip.
- **`std::time::wall_clock()`** (`@server`): a request (`time/unix`), answered on the next call
  with the milliseconds since the Unix epoch, which `std::time::unix_ms(answer)` reads. Both
  hosts answer it: `wrela-serve` from the system clock, this host from `Date.now()`.

It is `nondet`, as every request's answer is. It can go back (NTP), so a program that needs
order keeps its own monotonic counter. Cloudflare's `Date.now()` advances only between I/O
(a Spectre mitigation), which is fine at a request's grain.

## 5. How long work runs

On `wrela-serve`, a call has a 50 ms deadline (epoch interruption), and a room is called again at
once after a call whose requests were answered. The accounts server verifies a run 400 ticks a
call (about 5 ms each on `wrela-serve`, about 40 ms in all).

On this host:

- **Calls come as they do natively.** After a call, the host calls again while its requests were
  answered or events wait, giving the event loop a turn between calls so other requests come in.
- **When no HTTP request waits on the instance,** the host sets the object's alarm for now and
  goes on from `alarm()`, which runs at most 250 ms of calls, then sets the alarm again if work
  is left. An alarm is retried if its object fails, and "remains active ... for up to 15 minutes"
  of wall time ([limits](https://developers.cloudflare.com/workers/platform/limits/)). So a queue
  of runs goes on with no client waiting, and after an eviction the alarm brings the object back:
  `serve_init`, then `serve`. A program that keeps its queue in the store (§1) resumes it there.
- **No per-call deadline.** JavaScript can't interrupt a WASM call it is running. The bound is the
  Worker's CPU limit per invocation: Workers Paid defaults to 30 s and allows up to 5 minutes
  (`limits.cpu_ms`); Workers Free allows 10 ms
  ([limits](https://developers.cloudflare.com/workers/platform/limits/)), which can't verify a
  run. A call past the limit fails the invocation (error 1102) and resets the object; the
  requests in flight fail and the store keeps only committed writes. Release config sets
  `limits.cpu_ms` to 5000 (open question 2).
- **Memory.** The instance's cap is 64 MiB, as `wrela-serve`'s. JavaScript can't refuse a
  `memory.grow` inside a call, so the host checks after each call and drops an instance past the
  cap (a host failure). An isolate's hard limit is 128 MB
  ([limits](https://developers.cloudflare.com/workers/platform/limits/)).

**A failed call** (the program's trap, a host limit) drops the instance. Every request open on it
gets 500 (the program's trap) or 503 (the host's limits), with an empty body. Their state lived
in the instance, and the next request starts a fresh `serve_init`. Its writes are not committed
and its responses don't go. The failure's reason goes to the Worker's log (`console.error`),
which holds no request data and no address. Requests that the failure didn't touch aren't
retried one at a time as `wrela-serve` retries a room's events (open question 3).

**A request that gets no response** within 30 s gets 504, and its connection closes for the
program.

## 6. The browser side

`std::http::fetch(method, path, headers, body)` (`@client`) is a host service (`http/fetch`,
§17.1's application services):

- `path` is absolute, on the page's own origin (`/api/me`, with a query if it likes). A path that
  doesn't start with a single `/` panics.
- The browser host sends it with `fetch`: `credentials: "same-origin"`, so the session cookie
  goes and comes back; `redirect: "error"`; `cache: "no-store"`. The headers are those a
  program may set on a request: `accept`, `content-type` and `x-wrela-*`.
- The answer is the response, which `std::http::response(answer)` reads: its status, its headers
  (`content-type`, `etag`, `retry-after`, `x-wrela-*`; never `set-cookie`, which the browser keeps
  to itself), and its body. A response that never came (the network) fails the request.
- The native client host fails it: it has no origin.

## 7. Same-origin routing

The site's Worker serves the API too, so the session is a first-party cookie and Cloudflare's
per-path rules (rate limits, WAF) apply to `/api/*`. `server/cloudflare.js` exports:

- `WrelaServer`: the Durable Object class.
- `api(request, env, options?)`: a `Response` for a request under the API's prefix (default
  `/api/`), or `null` for one that isn't. The site's `fetch` calls it first.

`api` does what it can before the object is billed:

1. The path must be under the prefix (else `null`) and the method one of §1's (else 405).
2. **Origin.** For any method but `GET`, `HEAD` and `OPTIONS`, the `Origin` header must be one of
   `env.WRELA_ORIGINS` (a comma-separated list, for example `https://wrela.dev`), and
   `Sec-Fetch-Site`, if sent, must be `same-origin`; else 403. A browser always sends Origin on
   such a request (Fetch, "append a request `Origin` header"), so a cross-site form or script is
   refused. The program can check again (its CSRF token).
3. **Size.** A body over 1 MiB is 413, by `Content-Length` and as it's read.
4. **Rate limits** (asked for by the accounts server: without a connection, a program can't limit
   one client, and a global limit lets one attacker lock everyone out of sign-in).
   `WRELA_LIMITS` lists `{ binding, prefix, methods?, period? }`. Each names a
   [Workers Rate Limiting](https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/)
   binding (its own limit, a period of 10 or 60 s), keyed on `cf-connecting-ip`. A client past one
   gets 429 with `retry-after`. The address is the binding's key only. The counts are approximate and
   per Cloudflare location (same page), so the program keeps its own limits in the store.
5. **Public reads from the cache.** A `GET` answered 200 with `cache-control: public` and a lifetime,
   no `set-cookie` and no `vary`, is put in the
   [Cache API](https://developers.cloudflare.com/workers/runtime-apis/cache/) by its URL, and a later
   `GET` of it is answered from there without the object. It is per data center and doesn't read
   through. Workers Caching would, but it caches the whole Worker's responses, which is the site's
   decision, not the host's.
6. It builds the request's bytes from the allowlisted headers, so no address leaves the Worker,
   and calls the server object over RPC (`env.WRELA_SERVER.getByName("server").request(bytes)`).
   RPC carries only those bytes.

Nothing is logged: the host has no `console.log` of a request, a path or a header.

## 8. From a build to a deployable Worker

`wrela build` of a program with a server role writes, beside `server/game.wasm`:

- `server/cloudflare.js`: the host, one ES module that imports `./game.wasm`. Wrangler bundles
  a `.wasm` import as a `CompiledWasm` module (the `rules` key in
  [Wrangler's configuration](https://developers.cloudflare.com/workers/wrangler/configuration/)).
- `server/cloudflare.json`: the Wrangler members the host needs, for the release tool to merge
  into its configuration:

```json
{
  "durable_objects": { "bindings": [{ "name": "WRELA_SERVER", "class_name": "WrelaServer" }] },
  "migrations": [{ "tag": "wrela-server-1", "new_sqlite_classes": ["WrelaServer"] }],
  "rules": [{ "type": "CompiledWasm", "globs": ["**/*.wasm"], "fallthrough": true }],
  "limits": { "cpu_ms": 5000 },
  "vars": { "WRELA_ORIGINS": "" },
  "assets": { "run_worker_first": ["/api/*"] }
}
```

`WRELA_ORIGINS` is the release tool's to fill, and `run_worker_first` joins the Worker's own list. New
namespaces must be SQLite-backed
([changelog, 2026-07-09](https://developers.cloudflare.com/changelog/post/2026-07-09-restrict-new-kv-backed-namespaces/)).
A migration tag is kept forever: the class is never renamed, or a new migration is needed.

What rywible/wrela's `studio/release` does with it (its `site.wrela` writes each Worker's script
and configuration): copy the console build's `server/` to `<out>/<worker>.server/`, outside the
assets directory, since the server's module isn't for the public. Begin the console's
`<worker>.worker.js` with `import { WrelaServer, api } from "./<worker>.server/cloudflare.js";
export { WrelaServer };`, and call `api(request, env)` first in its `fetch`. Merge
`cloudflare.json` into `<worker>.wrangler.json`, filling `WRELA_ORIGINS` with the console's
origin. `studio/deploy` runs `wrangler deploy` as it does now. The Worker has no secrets of its
own, and the deploy's `CLOUDFLARE_API_TOKEN` is enough: a Durable Object has no permissions of its
own, only the Worker's that implements it
([Durable Objects roles](https://developers.cloudflare.com/workers/authorization/durable-objects/)).

Size: a Worker may be 64 MiB uncompressed and must start in 1 s
([limits](https://developers.cloudflare.com/workers/platform/limits/)). The host is tens of KB;
the server module is the program's.

## 9. A local run, and how the host is tested

- **Local run:** `npx wrangler@<pinned> dev` on the release's configuration runs the real Workers
  runtime (workerd) with a local SQLite store; the console's page and its API share
  `http://localhost:8787`. A `__Host-` cookie needs Secure, which browsers accept on
  `http://localhost` as a potentially trustworthy origin. rywible/wrela's
  `docs/toolchain.md` says how.
- **The host's own tests** (`runtime/cloudflare/test`, Bun): the instance against hand-written
  WebAssembly (each import, a trap, the memory cap, request ids), the store over `bun:sqlite` (the
  same synchronous `exec` shape as `ctx.storage.sql`), WebCrypto's three algorithms on keys it
  makes (an ES256 signature turned to DER as an authenticator gives it, an RSA modulus with a
  leading zero as COSE may give it), strict DER, and the alarm going on with work no request
  waits on. HTTP's bytes are checked against `runtime/abi/vectors.json` by both TypeScript hosts.
- **End to end** (the compiler's suite, `http::`): it builds a program that counts visits in the
  store, sets a cookie, asks for the time and random bytes and checks a signature, and drives the
  built `server/cloudflare.js` in Bun through `api` and the Durable Object, with its storage on
  `bun:sqlite` (`runtime/cloudflare/test/e2e.test.ts`). It checks the responses, the cookie, the
  commit before the response, the headers that reach the program (no address), the router's
  refusals, concurrent requests, and that a trap or a misused response gets 500 and the next
  request a fresh instance.
- Not in the toolchain's checks: `wrangler dev` itself (it needs Node and a download), so the
  first deploy is the check of the glue that only workerd runs: `cloudflare:workers`, RPC and
  alarms. That is open question 5.

## 10. Then WebSockets for rooms (the next scope)

Not built in 0.13.0; its own pull request and release.

- **One room, one Durable Object**, named by the room (`getByName("room:" + name)`), in its own
  class (`WrelaRoom`), each with its journal in its own SQLite table: records, each with its
  CRC-32C as on `wrela-serve`, and the snapshot of a compaction in one transaction with the
  journal's truncation.
- **The same call contract as `wrela-serve`** (`wrela_host::rooms`): a call's records are kept
  (`transactionSync`) and its store writes committed (RPC to the server object, awaited) before
  its sends go. Poisoned messages, quarantine, three failures and the room's time follow §19.2.
  One WebSocket binary message is one frame, so the varint framing is dropped; the bounds of
  §19.3 (64 KiB frames, 64 bytes less up, 1 MiB waiting for the room) are kept.
- **The Hibernation API** (`ctx.acceptWebSocket`): idle sockets cost nothing while the object
  is out of memory, and "WebSocket messages routed through a Worker do not count as requests"
  ([pricing](https://developers.cloudflare.com/workers/platform/pricing/)). Each socket's
  attachment (`serializeAttachment`, at most 16 KiB) holds its connection number and tag, so a
  woken object rebuilds the room from its journal and tells the program of its connections, as
  `wrela-serve` does after a restart. At most 32,768 sockets an object
  ([state API](https://developers.cloudflare.com/durable-objects/api/state/)).
- **Checks in the Worker before an object is billed:** the path `/room/<name>?pair=<16 hex>`, the
  pair equal to the server build's (`wrela.pair`, read at build time into `cloudflare.json`'s
  host), the Upgrade header, and the Origin against `WRELA_ORIGINS`.
- **The browser:** `std::net::open` uses a WebSocket where `net.json` says the server is one
  (`"transport": "websocket"`), and WebTransport otherwise.

## 11. What rywible/wrela must change, per release

**0.13.0 (HTTP).** Release files: `server/cloudflare.js` and `server/cloudflare.json` from the
console's build, placed and merged as §8 says. Deploy commands: none new (`wrangler deploy`). The
first deploy creates the `WrelaServer` namespace through the migration. Secrets: none new (a Durable Object takes its Worker's permissions, so the deploy's token does). Cloudflare plan: **Workers Paid** ($5 a month).
Free's 10 ms of CPU an invocation can't verify a run, and Paid allows `cpu_ms`. The accounts
engine moves challenges and rate counts to the store, uses `Request::time` for its clock, reads
the session from the `__Host-` cookie, and checks Origin and its CSRF header. The console calls
`std::http::fetch`. The pin moves to 0.13.0.

**WebSockets release.** Its own list when it's cut.

## 12. Later, listed but not built

- **DataChannels** (Realtime SFU) for unreliable movement, after WebSockets.
- **Signing in `std::crypto`**, keys from Worker secrets (a token the server issues and other
  servers check).
- **Servers that admit other programs' pairs** (a list of pairs a server takes, so a game's own
  client can reach the console's server directly).
- **An MMO's gateway:** zones as objects, a gateway Worker holding client sockets and forwarding
  over one RPC session.
- **HTTP on `wrela-serve`**, so a local run needs no Wrangler.

## 13. Open questions

1. **`user-agent`.** Left out of what a server sees, since it fingerprints and the privacy policy
   lists what's kept. Does the accounts server want it, say to name a session's device ("Chrome
   on a Mac")? If so, the host could pass a coarse family instead.
2. **`limits.cpu_ms`.** 5000 is a guess: enough for a call that verifies a slow run, and a cap on
   what a runaway call costs. It should be measured on the first deploy.
3. **Isolating a failing request.** Today a trap fails every request open on the instance.
   `wrela-serve` retries a room's events one at a time to find the poisoned one. Is that worth
   doing for HTTP, where one attacker's request could otherwise fail others' concurrent
   requests?
4. **Past one object.** Sharding the store, if a server ever needs more than about 1,000 requests a
   second or 10 GB.
5. **workerd in the checks.** The glue that only workerd runs (RPC, alarms, `transactionSync`'s
   real behaviour) is tested against fakes. Should a check run `wrangler dev` where Node is on the
   machine?
6. **The prefix.** `/api/` is the default. Does the console want another?
