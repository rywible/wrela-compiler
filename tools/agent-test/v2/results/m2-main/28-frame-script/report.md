**(1) It built on attempt 1.** The package is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/28-frame-script/pkg/main.wrela`.

- **Not run:** I didn't run the frame test, because the brief allows only the `attempt` build and the three lookup commands. The build compiled it, but whether `counts_two_space_presses` passes is untested.
- **State:** `pub struct Game { pub presses: u32 }`, made by `pub fn init() -> Game`.
- **`frame`:** it is `frame(state: mut Game, time, width, height)`. It reads `events()`. It adds 1 to `presses` for each `Event::KeyDown { key: Key::Space, repeat: false, .. }`, so a held, repeating key isn't counted.
- **Script:** `const SPACES: Script = Script::frames().press(2, Key::Space).press(5, Key::Space)`.
- **Test:** `@test(frames: 10, input: SPACES)` takes `game: Game` and asserts `game.presses == 2`.

**(2) Diagnostics:** there were none, so nothing changed.

**(3) Misleading points:** nothing was wrong, but two things were missing from the spec.
- §13.2 and §17.2 show `Script`'s `press`, but the files I was allowed to read don't list the `Key` variants or the `Event` shapes. I had to run `wrela doc std::input::Event` and `wrela doc std::input::Script` to get them. I guessed `Key::Space` from the DOM-code naming (`Key::A`, `Key::P`), and it compiled.
- The brief says `wrela primer` can look things up, but I didn't need it.