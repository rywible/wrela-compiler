# Changelog

Each release's changes a program or a tool can see. The pull request of each release holds the
detail; `docs/release.md` says how a release is cut.

## 0.34.0

- **`wrela pixel <package> --frame <n> --at <x>,<y>`** says why a pixel has its colour (language.md
  §23.9): every draw of the frame that changed it, the draw that decided it, and its fragment
  shader run again for that pixel in an interpreter of the GPU code the build wrote, with each
  line's values and the first value that wasn't finite. `--kernel <label> --id <i>` does the
  same for one invocation of a kernel.
- **`wrela inspect <package> <module>::<fn>`** reads what a surface is made of (§23.10): its
  pieces, certified by intervals (`floating`, `neck`, `speck`), the parts that decide it nowhere
  (`dead-part`), parts thinner than a size (`thin`), the literals that move it nowhere
  (`dead-literal`, with `--lift`), and pictures from three sides with no GPU (`--views`).
- **`wrela sliders <package> <module>::<fn> --free ...`** finds the independent ways a surface's
  freed literals move it, each named by what it moves and the literals that make it, lists dead
  and redundant literals, and moves along one (`--move`, `--write`, `--sheet`) (§23.10).
- **`wrela fuzz <package>`** looks for the input that traps a program's ticks (or, with
  `--room`, its server's rooms), guided by the code each input runs; each finding is made as
  small as it can be and saved as a tick log and a script, and `--replay` runs saved ones
  (§16.2).
- **Reverse-mode derivatives** (§14.1): `std::derive::reverse(f, at)` gives a function of an
  array's value and its derivative by each number from one recorded evaluation (`Trace`), and
  `std::lift::reverse` and `reverse_sum` do the same for lifted literals, however many. `wrela
  solve` takes its gradient by reverse mode from 65 free literals, where it is faster.
- **Outlines are measures** (§20, §23.6): `std::field::coverage`, `outline_miss` and
  `image_miss` say how far a silhouette from the side, the front or the top (`View`) is from a
  blueprint's outline or an image, and a spec can hold a subject to it.
- **A lift entry names one file of a package, `<package>/<file>`** (§23.1), for `wrela build
  --lift`, `wrela run --lift` and a tool's `BuildOptions.lift` alike: that file's literals alone
  are lifted. The form was taken before and said nowhere but the spec; a file the package
  doesn't have is now an error that names it, where it lifted nothing.
- **`wrela solve` lifts only the files `--free` names** (§23.6): a literal of another file, a
  blueprint's outline or a spec's target, is a reference the solve neither moves nor derives
  through.
- **`wrela solve` holds a step to its literals' sizes** (shortened whole, so it keeps its
  direction) and searches along it by Armijo's rule; a solve the program traps under says the literals it was trying; a spec
  whose every check already holds is left as it is (`"held_before": true`); and rounding gives
  a literal no decimals that buy nothing and never leaves the function larger than the solve
  found it (§23.6).
- **`@deterministic` kernels** (§18.14): a `@compute` kernel marked `@deterministic` gives the
  same bits on every GPU and on the CPU. It computes with integers and moves floats only as
  bits (anything else is E0713), its indexes past the end are clamped, and `k.run(input,
  params, mut out)` is its CPU form, made from the same code.
- **`std::collections::Columns<T>`** keeps each field of `T` in its own array (§6.10): `c[i].x`
  reads and writes one field, `c[i]` read whole gathers a copy, and a loop over one field reads
  only that field. Lending `c[i]` to a `mut` parameter, binding it `mut` or assigning it whole is
  E0339.
- **More loops run four at a time** (§11.2): loops over `Vec`s, with their bounds checks part of
  the four-wide test, and loops whose bodies branch on comparisons of floats. A check of a
  loop's counter against its own bound, or the length of the `Vec` it walks, is gone.
- **`wrela build --report layout`** lists, for each loop over a container or array of structs,
  the bytes of the struct it reads and writes, the most skipped first (§6.10).
- **GPU fixes the compiler's own tests found** (§11.3): `tanh` no longer gives NaN for large
  arguments, `atan2` of two zeros gives the CPU's bits on every GPU (y where x is +0, π with y's sign
  where x is -0), and an unsigned division whose
  dividend is a constant near 2^32 is right on Apple's GPUs (Metal divided it wrongly).

## 0.33.0

- **`wrela solve --spec --write` writes only a spec that holds** (language.md §23.6). Where a
  check still misses when the solve ends, it writes nothing, says which checks miss and exits
  with status 1 (its JSON: `"written": false`, and `"refused"` saying why). It wrote wherever
  the solve stopped. `--minimize` and `--fit` have no checks to hold a solve to, and write where
  it ended, as before.
- **`--free <file>:<name>`**: the literals of the value the file binds `<name>` to (`let reach =
  0.62`, a `var`'s value, a `const`'s), beside `--free <file>:<lines>`, and with them
  (`solved.wrela:reach,drive,40-44`). A name stays put when the file's lines move. A name the
  file doesn't bind, binds more than once, or binds to a value with no lifted literal is an
  error. The report's literals carry their `name`.
- **A warning for each freed literal the solve couldn't feel**: `warning: <file>:<line>: <the
  function> never changed with the literal ...` when its derivative by the literal was 0 at
  every step (JSON: the literal's `"changes": false`, and `"warnings"`).
  `std::lift::Minimize::changes(i)` says it of free literal `i`.
- **E0700 says when it's heap memory** (§14.2). A derived function that reads or writes a
  `Vec`, a `String` or a `Box` was told it "records GPU work"; it's now told it reads or writes
  heap memory, where a derivation can't follow a float, and what to do: keep what depends on the
  input in arrays and values, or make the heap value in a function that takes nothing that
  depends on the input and returns a value. The error is at the program's line, not in std's
  code (`derived here` and `in std's code here` point at the rest). Work with other threads (an
  atomic, a task) has a message of its own.
- **A value that depends on the input put in heap memory is E0700, not a derivative of 0.** A
  gradient runs a function as it is when its result carries no derivative, as far as the
  function's own code shows. A float read back out of a `Vec` shows none, so a function that
  kept what depended on the input in a `Vec` and returned it (`fn posed(k: Key) -> Vec<Rigid>`),
  read by another, was run as it was: `std::lift::gradient` gave 0 for every literal on that
  path, and `wrela solve` took 0 steps and said nothing. The store is now an error where it is
  written (`w[d] = ...`, `v.push(x)`), for gradients, intervals and tapes. Code that did this
  compiled before and computed a wrong derivative; it now needs its input-dependent values in
  arrays and values.

## 0.32.0

- **Least-squares fits** (language.md §23): `wrela solve <package> --fit <module>::<function>`
  makes the sum of the squares of the numbers a function gives (`fn() -> [f32; M]`: how far each
  of `M` things misses) as small as it gets, by Levenberg–Marquardt (`std::lift::Minimize::fit`).
  A fit that `--minimize` took hundreds of BFGS steps over takes a dozen or so.
- **`std::lift::jacobian(f, which)`**: `gradient` of a function that gives more than one number.
  It gives `f`'s value, and for each literal of `which` how every part of the value changes with
  it, from one derived evaluation.
- **`wrela solve` rounds all the literals at once** where that costs no more than 5% of the
  solved value, and only otherwise one at a time with the rest solved again. A solve of many
  literals spent most of its time in those solves.

## 0.31.2

- E0712 (language.md §18.13) no longer counts a loop's counter math when the whole loop is under
  a branch the command chooses (`if g.learned != 0 { .. }` round a net walking its weights,
  `w[FIRST + r * INPUTS + c]`): a counter that is a variable of that block alone, set there
  before anything reads it, is the loop's structure there as it is outside, whatever led to the
  block. A variable from outside the block that the branch sets is still the command's to say,
  and counted. Still counted, wrongly: such a loop under a branch that is itself inside a loop
  (the outer loop's counter is held to depend on the branch). `wrela pipelines` lists what it
  did.

## 0.31.1

- A borrow struct that is a temporary (a call's result, or written out where it's used) reads
  through its projection fields as a named one does: `ctx_of(world).world.count`, and a group
  (language.md §18.6) given to `bind` in place (`paint.bind(waves: waves_of(ocean), ...)`). It
  was I0001, the CPU module's IR malformed, and the way round it a `let`. A field, a component or
  a swizzle read this way loads only itself; lending such a field whole (`count_of(ctx_of(w).world)`)
  still copies what it borrows, where a named struct's doesn't.

## 0.31.0

- **Calls a table could answer** (language.md §18.13): `wrela pipelines` lists, under each
  pipeline, the costly functions its entry points call with at most three numbers that vary
  between invocations, all else the same for every invocation (the sky along a direction, a
  noise at a point): how many times an invocation calls each, how many numbers vary, and their
  work. `--json` gives them as `few`. A call given only the invocation's own place isn't listed,
  nor one doing no more work than a table's read. Calls are counted as the work count walks the
  code (each loop pass that runs, each call of the functions they're called through), with what's
  known of their arguments. The query alone looks for them; a build doesn't.
- **Work** (§18.11): `ir::work::CallRuns` counts how each call in a function runs in one call of
  it.

## 0.30.0

- **Tighter ranges over noise** (language.md §14.2): the interval of `std::field::value_noise`
  over a box is exact (up to 2⁻¹⁶ of rounding) where the box crosses at most one lattice plane on
  each axis, from the noise at its corners and planes, not derived from its body. Over a tenth of
  a cell it was three to five times too wide; a displaced stone's interval over 12.5 cm blocks is
  now half as wide, and 20% fewer of those blocks look like they might hold its surface. Taped
  fields (`NOISE3`) get it too.

## 0.29.0

- **Breaking: every surface states where it is** (language.md §20.3). `Surface::bound()` has no
  default: an impl without one is E0401. A surface that has none says so, `UNBOUNDED`, with its
  reason (closures, half-spaces, `Sampled3d`, taped fields and `ToBound` do). std's `Tube`,
  `UnionAll`, `SmoothUnionAll`, `SmoothSubtract`, `SmoothIntersect`, `Mirror` and a rotated
  surface (`Turn`) now state real bounds, so culling and LOD can skip them; `PointMap::bound` is
  required too.
- `std::field::check_bound` (and `bound_broken`): a search of the boxes round a stated bound for
  one whose every distance is below what it allows. Debug builds run it where a surface is stepped
  by (`to_bound`), beside `check_lipschitz`.

## 0.28.1

- E0712 no longer counts what's computed only from constants and loops' counters, whatever it
  costs (`(f32(i) + 1.0) / f32(steps)` in `engine::air`'s march): nothing the command gives, so
  nothing to pass in. A counter is only that while every store to it, the index it's stored at,
  the branches it's under and its loop's exits are too: a uniform-bounded loop, a uniform-picked
  element or a uniform-chosen constant still count.

## 0.28.0

- **GPU code computes constants as it's built** (language.md §18.13): a value computed only from
  constants, and what a loop leaves when all it reads is constant, is a constant in the WGSL, no
  longer computed by every invocation (`normalize(CUTS[i].xyz)` in the opening's rock).
- **E0712: a shipped build's GPU code that computes, in every invocation, more than 256 work of
  a value the same for all of them** (computed only from the uniform block, a buffer or a texture read at the same
  place, and constants). Compute it once on the CPU, or in a `@compute(1)` kernel, and pass it
  in. Kernels of one invocation a workgroup aren't checked. A loop's values count once a pass;
  a loop's own counter arithmetic isn't counted.
- `std::field`'s `Fbm::range` is its closed form, `2 − 2^(1 − octaves)`, not a loop: a displaced
  surface's bounds no longer loop over the octaves in every invocation.
- Breaking: shipped builds of the opening hit E0712 until its sites are hoisted (hero, weather,
  paint, isle, yonder, skiff, aboard); stay on 0.27.0 until then.

## 0.27.0

- **Shaders compile faster** (language.md §18.11): a loop that calls a big function is no longer
  written out a pass at a time, and back-to-back calls of a big function whose arguments don't
  need each other's results (a normal's taps) merge into one call in a loop. The opening's far
  pass compiles about 6x faster cold (22.6 s to under 4 s on SwiftShader), its noise cook 10x.
- **A pipeline's weight is its WGSL's**, after the compiler's inlining and these rewrites, so
  `wrela pipelines` and the 300,000 cap (E0707) count what a browser compiles. A function's
  `@budget(weight: n)` still counts its code as written.

## 0.26.0

- **Stand-ins** (language.md §18.12): `@standin(cheap)` on a `@compute` kernel or a `@fragment`
  shader names a cheap entry point of the same stage and parameters. The compiler builds its
  pipeline beside the real one, with the same bindings, and the manifest names it (`standin`).
  The browser builds stand-ins first and runs a command on its pipeline's stand-in until that
  pipeline is built, so a start no longer waits for its slowest shaders. A stand-in that can't
  stand in is E0711. Test mode waits for each pipeline as before unless `standins=1`; its
  `starts.json` lists the frames that ran on a stand-in (`stood_in`). `wrela pipelines` shows
  each pipeline's stand-in.

## 0.25.0

- **Work and weight** (language.md §18.11): the compiler counts each GPU function's most work in
  one call (about one 32-bit operation on one invocation a unit, the costlier side of each branch
  it can't decide, each pass of a loop to its sure exit) and its weight (its statements with every
  call written out, what a browser's shader compiler works through). Both are the same on every
  machine. `wrela pipelines` shows each pipeline's (`work`, `weight` in `--json`).
- **`@budget(work: n, weight: m)`** holds a GPU function to them: over either, or with work that
  has no bound, is E0710 at the attribute, naming the calls that do the most.
- **A shipped build's pipeline weighs at most 300,000** (E0707), beside its 256 KiB of WGSL.

## 0.24.0

- **A program tells its rooms from its instance `server`** (language.md §19.6):
  `std::net::room_name()` (`@server`) is the room's own name, as `std::net::room()` answers it,
  or none where the host names no room. On Cloudflare a room and the named instance `server`,
  whose store the rooms share, both answer `std::server::me()` with `server`, so a program that
  calls the instance `server` (to read what its rooms wrote) runs its own code there, not a
  room's.

## 0.23.0

- **A page knows it's out of date** (language.md §19.3, §19.6): on Cloudflare, a room's connection
  from a client of another build (a page loaded before a deploy) is answered by the Worker with a
  WebSocket closed at once with code 4409, where it was refused (403), which a browser can't tell
  from a lost connection. The browser host gives the program that closing with the tag
  `std::net::OUTDATED`, so it stops reopening rooms no build of its own runs, and can load the
  page again (`std::host::reload`) when that suits the player. No room's object is billed for it.

## 0.22.0

- **A server reads its own secrets** (language.md §19.5): `std::server::secret(name)`
  (`@server`) answers one of the host's secrets at once, as its value's UTF-8, to any instance, the edge
  and a room included. A host gives only those its configuration names as the program's: on
  Cloudflare the Worker variable `WRELA_SECRETS` (names, comma-separated), each a Worker secret;
  on `wrela-serve` the environment variables its `WRELA_SECRETS` lists. Another, or one not set,
  fails. So a room can check what an account signed without the key crossing a request.
- **A server host answers the time of day at once** (`time/unix`), as it does a secret, and
  `std::time::unix_now()` (`@server`) reads it in the call that asks.

## 0.21.0

- **A program draws its own focus ring** (`std::semantics`): `Node.ringed` says the program
  draws that node's keyboard focus itself, so the browser host draws no outline over it
  (`wrela-ringed`, flag 128 of the tree's bytes). A page's own controls then show one ring, the
  program's, and none at all when a pointer opened a dialog that took the focus.

## 0.20.0

- **A room's relay** (language.md §19.3, §19.6): `std::net::relay(conn, via)` (`@client`) opens
  a lossy, unordered channel to the other peers of the room that connection `via` is open to,
  which doesn't pass through the room: each message (at most `MAX_RELAY`, 1024 bytes) reaches
  each other peer at most once, or not at all, as the sender's tag then its bytes. Only a
  connection the room tagged may join. On Cloudflare it goes through Realtime SFU: the room's
  object holds the app (`WRELA_SFU_APP`) and its secret token (`WRELA_SFU_TOKEN`) and speaks to
  the SFU for its peers over text messages on their WebSockets, and the page's WebRTC, on the
  main thread, hands each data channel to the render worker. A peer's text message no longer
  closes its connection. A tag has one relay (the same peer joining on a new connection ends the
  old one), a connection may join 4 times a minute, and one whose WebRTC isn't up in 30 seconds
  gives its place up. `wrela-serve` has no relay yet.

## 0.19.0

- **Textures baked when a program is built** (language.md §20.2): `std::png::decode` reads a
  PNG (every colour type and bit depth, Adam7, `tRNS`) to an `Image` of RGBA8 pixels, rows top
  to bottom, with `texel` and `sample` (bilinear, repeating, at glTF's texture coordinates), so a
  constant decodes an embedded texture (`decode(embed("skin.png"))`) and bakes it into the
  fields and parameters the program draws with, and nothing decodes an image while it runs. A
  malformed file is `None`. No gamma is applied: an ordinary file's bytes are sRGB. A 1024 × 1024
  RGBA texture decodes in under a second of the build, within a constant's default fuel.
- **`std::zlib`**: `inflate` (a zlib stream, its header and Adler-32 sum checked), `inflate_raw`
  (raw deflate), each `None` for a malformed stream or one past `most` bytes, and `adler32`.
- **A software adapter, on purpose:** `WRELA_ALLOW_SOFTWARE=1` lets the native host open a GPU
  on the CPU (lavapipe, SwiftShader), and gives a Chrome run that a frame test, `wrela profile`
  or a tool's browser starts test mode's new `software=1`, which admits WebGPU's fallback
  adapter. For a machine with no GPU (a cloud VM an agent works on); off by default, and its
  timings are a CPU's (a profile's `adapter` says `Cpu`).
- **`std::transform::inverse` and `determinant`** of any `mat4`.
- **`std::net::room()`** (language.md §19.2): a room's own name, as its peers open it
  (`/room/<name>`), so a room can keep what must outlive its build's journal in the store under
  it. Cloudflare's room object keeps the name it was opened by; `wrela-serve` names each room it
  starts (`net/room`).

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
