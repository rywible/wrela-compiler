1. **Built:** yes, on attempt 1. The program is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/22-arena/pkg/main.wrela`.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading parts:** nothing in the language or its documentation misled me.
   - The spec doesn't show `Arena`'s API, so I looked it up with `wrela doc Arena`. That gave `Arena::new()`, `insert` (returns a `Handle`) and `remove(h)`, and it worked as shown.
   - I only compiled it and did not run the exports. I'm confident it is correct from reading the code.

Both exports take no state, since the task doesn't give the program's state. Each one builds the arena itself. It inserts five `Unit`s with hp 1, 2, 3, 4 and 5, then removes the handle returned for the third one (hp 3). `alive()` returns the arena's `len()`, which is 4. `hp_sum()` sums hp over the remaining units, which is 1 + 2 + 4 + 5 = 12.0. `frame` is empty.