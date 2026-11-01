# Changelog

Each release's changes a program or a tool can see. The pull request of each release holds the
detail; `docs/release.md` says how a release is cut.

## 0.18.0

- **A server runs as named instances** (language.md §19.5, `std::server`): one `WrelaServer`
  class holds every instance, `getByName("<kind>/<name>")`, each with its own store; `server` is
  the rooms' store. Every API request now reaches a fresh **edge instance** in the Worker first,
  with no store, which routes it with `dispatch(conn, Route)` (`Instance(name)`, `Edge`,
  `Refuse(status)`) or answers it itself. `me()` is an instance's name (empty at the edge).
  `call(name, bytes)` / `reply(conn, bytes)`: at most once, the reply after the callee's writes
  commit. `send(name, bytes)` / `ack(conn)`: at least once, from an outbox written in the call's
  transaction, retried with backoff capped at an hour forever (one log line past an hour, with
  no request data), and dropped only once the receiver's acknowledging call commits.
  `call_build(artifact, bytes)` calls an archived build's verifier through the Worker Loader with
  no bindings and no network. A program that serves HTTP must now `dispatch` at its edge (no
  compatibility: there were no accounts yet). `wrela-serve`'s rooms answer `me()` with `server`
  and refuse the rest.
- **The verifier artifact:** `wrela build` writes `server/verifier/` beside a server build:
  `main.js` (a `WorkerEntrypoint` whose `verify(bytes)` calls the build's edge instance),
  `cloudflare.js`, `game.wasm` and `verifier.json` (`schema` `wrela-verify-1`,
  `compatibility_date`, `main`, `modules`). `server/cloudflare.json` adds the Worker Loader
  binding `WRELA_LOADER`; a release that uses `call_build` binds an R2 bucket `WRELA_BUILDS`.
- **`std::crypto::hmac(key, message)`**: HMAC-SHA256 (WebCrypto on Cloudflare, the `hmac` crate
  on `wrela-serve`), and `same(a, b)`, a comparison in constant time.
- **`std::http::fetch_public(path, headers)`**: a client's `GET` to its own origin without
  cookies, which the browser's HTTP cache may answer (`http/fetch-public`).
- **A build's payloads at content addresses** (language.md §15.2): a build may ship
  `addresses.json` (`{"format": "wrela-addresses-1", "files": {path: "/b/..."}}`). The browser
  host loads the WASM modules, the shaders, `game.lines` and the program's fetched files from
  their addresses; an address outside `/b/` fails the load, and no map is as before.
- `std::host::flush_commands`, a std intrinsic, lets the host answer a request in the call that
  makes it (`me()`).

## 0.17.0

- **A program is opened at an address with a fragment** (language.md §17.1):
  `std::host::open("capybara/#attract")` opens the build at `capybara/`, and the opened
  program's `std::host::address` is `/capybara/#attract`, so one build can start in another way
  without a second build. Its page and files are fetched and cached without the fragment.
  runtime/browser: `programUrl` (`surface/open`, `prefetch`) allows a fragment, except one that
  asks for test mode (`#test`), and still refuses credentials and queries, now an empty query
  (`/?`) too; `prefetch` fetches a URL without its fragment.

## 0.16.0

- **A program shows another playing behind it** (language.md §17.1): `std::host::underlay(id,
  on)` shows the program in a surface this one opened behind this one's canvas, which is then
  composited with its alpha (premultiplied) wherever its screen pass clears to `vec4(0.0)`. The
  program behind is muted, hears no input and keeps its own frame rate; `on` false hides it, as
  `show` and `close` do. The native host declines it. runtime/browser: the host service
  `surface/underlay`, the parent's signals `underlay` and `underlay-end` to the program behind,
  and the canvas reconfigured `alphaMode: "premultiplied"` while one is underlaid.

## 0.15.0

- **A program reloads its page** (language.md §17.1): `std::host::reload()` loads the page
  again from its address, so the program starts again from its bootstrap with the page's
  storage kept. Only the program the page itself runs may: the browser host declines one in a
  surface another program opened, and the native host declines it. runtime/browser: the host
  service `nav/reload`.

## 0.14.0

- **A server build's rooms run on Cloudflare** (language.md §19.6): `server/cloudflare.js` now
  also has `WrelaRoom`, a Durable Object a room, and `rooms` for the site's Worker, and
  `server/cloudflare.json` binds it (`WRELA_ROOM`, its migration, `/room/*` before the assets).
  A peer's connection is a WebSocket, held through the Hibernation API, so an idle room leaves
  memory with its peers connected. Each binary message is one frame. The room keeps
  `wrela-serve`'s contract: its journal in its own SQLite, read back when it starts; its store
  writes committed in the server's object before its records and sends; poisoned messages,
  quarantine and three failures at the host's limits. `rooms` refuses an upgrade from another
  build, another origin or a client past its rate limits before the room's object runs.
- **`server/cloudflare.json` sets no CPU limit**, so it deploys on Workers Free; a release on
  Paid may add `limits.cpu_ms`.
- **The browser host opens a room over a WebSocket** when `net.json` names a `wss:` server (`ws:`
  only to a loopback address); an `https:` server is still WebTransport.

## 0.13.0

- **A server build runs on Cloudflare** (language.md §19.5): `wrela build` writes its host beside
  `server/game.wasm`, `server/cloudflare.js` (a Durable Object, `WrelaServer`, and `api` for the
  site's Worker) and `server/cloudflare.json` (the Wrangler settings it needs). One SQLite-backed
  object holds the store, with §19.4's contract: reads on the next call, a call's writes in one
  transaction before its responses, nothing of a failed call. Its signatures and randomness are
  WebCrypto's.
- **A server answers HTTP** (`std::http`): a request comes as a short connection, whose message
  `request` reads (method, path, query, the headers a server may see, body, and the time of day),
  and `respond` answers it once; `session_cookie` makes a `__Host-` cookie. The host passes only an
  allowlist of headers, never an address, and refuses a request that changes something from
  another origin before the program runs, and a client past its rate limits (`WRELA_LIMITS`, Workers
  Rate Limiting keyed on the client's address). A public `GET` answer is served from the Cache API.
- **A client asks its page's origin** with `std::http::fetch`, its cookies sent (the browser host).
- **A server asks the time of day** (`std::time::wall_clock`, read with `unix_ms`), on
  `wrela-serve` and on Cloudflare.

## 0.12.0

- **The browser host builds the first frame's pipelines first** (language.md §18.9): no
  pipeline that no command asked for starts until the first frame's commands have all run, and
  then none starts while a command waits for one. 0.11.1 started six at load, the largest first,
  so a pipeline the first frame needed compiled behind those it didn't. rywible/wrela's capybara
  (17 pipelines, headless Chrome on Linux, cold, median of 3): its first frame on the screen at
  2.71 s after the page opened, from 3.56 s; warm, 0.30 s, from 0.28 s (its pipelines from the
  cache now start when the frame asks, not at load).
- **Test mode says when each frame was on its way to the screen**: `frames.json`'s
  `submitted_ms`, when its commands had all run and were submitted (after any that waited for a
  pipeline), and `load.json`'s `first_submitted_ms`. `show=1`'s summary times the first frame and
  `playable` by them; `began_ms` is still when each frame began.
- **`std::tick::post(tag, a:, b:)` puts a value the frames know into the ticks** (§16.2): the
  host stamps it into the next tick's records, as an input event, so a tick log keeps it and a
  replay holds; the step reads it in `Ticked`'s new `posts` (`Posted { tag, a, b }`). A replaying
  host drops what the program posts. runtime/abi: the record kind `Posted` (16), and the host
  service `tick/post`. A `Ticked` literal names `posts` too.

## 0.11.1

- **An associated constant made with `&`, `|`, `^`, `<<` or `>>` folds** (language.md §3.4): a
  look's mask, `const SUBSTANCE: u32 = FORM | TONE`, decides its branches as a literal would. 0.11.0
  left it a call, so an `if` on it built both branches, and a look whose mask was written so drew
  its four-target shaders into a one-target pass (E0603).

## 0.11.0

- **A fragment shader may return its look's associated type** (language.md §18.1: `-> Y::Out`),
  checked for each instantiation, so one shader writes one target for one type and four for
  another. 0.10.3 refused it before instantiation (E0602).
- **An `if` on an associated constant builds only its taken branch** (§3.4), where its condition
  folds: the constant, or a comparison (of integers or `f32`s), `!`, `&&`, `||`, `&`, `|` or `^` of
  such and literals, written in the condition. The branch the instantiation doesn't take
  instantiates nothing, so it may draw into a pass the instantiation's shaders couldn't. 0.10.3 lowered both branches, and a
  draw in the other one was E0603 though it was never taken. The shaders are those 0.10.3 wrote
  but for their locals' names and order, and where a constant `if` reached WGSL before, it's gone.
- **`--set` points a `pub type` of the program's `main.wrela` at another type** (§13: `--set
  LOOK=styles::cel::Cel`), as it sets a `pub const`: so one program builds in each look.

## 0.10.3

- **An associated constant folds an enum's variants** (language.md §3.4), where the enum's
  variants hold nothing: `const FOG: bool = TIER != Tier::Low` and `match TIER { Tier::Low =>
  false, _ => true }` fold, so a program's `const TIER: Tier`, set with `wrela build --set
  TIER=Low`, builds none of what its tier turns off. In 0.10.2 they stayed calls, and the branch
  and its pipelines with them. An arm with a guard, or one that binds, still doesn't fold.

## 0.10.2

- **The browser host builds six pipelines at a time** (language.md §18.9), tier 0's first and
  the largest first, then tier 1's; one a command waits for starts at once, beside them. 0.10.1
  asked for all of tier 0's at once (tier 1's once they'd settled): the GPU process compiles a few
  at a time whatever's asked, so a small pipeline a frame needed waited behind every other. On an
  M4 Air (rywible/wrela's C5, three starts each), the Last Green's warm start went from 6.5 s to
  5.4 s and the vale's cold one from 8.0 s to 6.7 s.
- **Test mode's `starts.json`**: when each pipeline was built, and each time a command waited for
  one (its frame, the pipeline, when it began waiting), in milliseconds since the epoch as
  `frames.json`'s `began_ms`: what a start waited for.

## 0.10.1

- **An associated constant whose initializer folds (§18.1's list) is folded where it's read**
  (language.md §3.4), however long the function that reads it and in a relaxed-SIMD build too: a
  branch on one that's `false` builds none of what only it reaches, its pipelines included. In
  0.10.0 it was a call, folded only once inlined: the CPU's inliner stops at its budget, and keeps
  strict code a call in relaxable code, so there the branch, and every pipeline behind it, stayed.
  One read in a debug build (whose arithmetic checks for NaN as it runs) or a lifted one (whose
  literals are the table's), one only a role's build may read (its call is E0708 elsewhere), and
  one of `f64` arithmetic (the folder's floats are `f32`s) stay calls, as before.

## 0.10.0

- **Associated constants** (language.md §3.4): a trait declares `const K: u32`, with an optional
  default; an impl gives `const K: u32 = 4`; an inherent impl may hold one. `S::K` under a bound is
  a constant of each instantiation, and may be an entry point's `const` argument (§18.1): each
  instantiation is a pipeline of its own. E0420 for a constant of a type it can't have, or one
  called as a function; E0600 for an initializer that does IO, prints, panics or names itself;
  E0603 for one a `const` argument can't fold.
- **`const` parameters are folded into their pipeline's WGSL** (§18.1): no WGSL `override` is left,
  so a switch that's off leaves no code in its shader. The manifest is version 12, without
  `constants`; both hosts still open a version-11 build (0.9's), setting its pipelines'
  `constants`, so `wrela same` compares a build against one 0.9 made.
- **Passes into up to eight colour targets** (§18): `begin_pass5` to `begin_pass8`, their depth
  forms, `Pass5`–`Pass8`, `DepthPass5`–`DepthPass8` and `Targets5`–`Targets8`, within WebGPU's 32
  attachment bytes a sample (E0602 names each target's bytes and the total). The ABI takes eight
  colour attachments (WebGPU's default `maxColorAttachments`). `wrela pipelines --json` gives each
  render pipeline's `attachment_bytes`.
- **A texture as a field's data** (§20.3): `std::gpu::Texture3dHandle<F>`, a 3D texture's handle as
  `Copy + GpuData` data, bound beside any value that holds one in a struct's fields where an entry
  point takes it (E0602 for one in a tuple, an array or an enum), and bound to a GPU local once
  (E0702); and `std::field::Sampled3d`, a `Surface` whose distances are a texture's texels.
- **A struct update over a return-position trait type** no longer makes malformed IR: a type lowers
  as the type it reveals.
- **The constant cache's key** holds the constant's WASM, its fuel, the build's options, the bytes of
  each file it embeds and the toolchain's version; the cache is `build/consts/<version>`, and
  another version's is never read. `wrela build --report consts` lists each computed constant:
  bytes, fuel, seconds, computed or cached.
- **`wrela build --set NAME=VALUE`** (§3.5) gives a `pub const` of the program's `main.wrela` another
  value (a scalar, a `Text`, or a variant of a fieldless enum); `std::tool::toolchain`'s
  `BuildOptions::set` too; `wrela pipelines` and `wrela same` take it. The manifest records the
  sets. E0224 for a constant a build can't set (one only `pub(package)`, or of a type a build
  can't set, an alias's included). A `match` on a constant that holds a variant builds
  only the arm it takes.
