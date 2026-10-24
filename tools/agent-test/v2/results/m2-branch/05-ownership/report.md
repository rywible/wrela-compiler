1. **Built on attempt 1.** It compiled with no diagnostics. I didn't run `run()`; I traced it by hand, and it returns 5.
   - The original is bumped twice, so it has 2. The clone is taken then, so the copy has 2.
   - The original is bumped once more, so it has 3.
   - `archive(take copy)` returns 2, and 3 + 2 = 5.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Anything that misled me:** nothing did. Two points were stated outright in the spec:
   - `Log: Clone` makes the struct non-`Copy` while still allowing `.clone()` (§3.2, §6.1).
   - Declaring `Copy` implies `Clone`, so `Clone` alone is the right opt-in here.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/05-ownership/pkg/main.wrela`. `frame` has an empty body, since the task has no GPU work.