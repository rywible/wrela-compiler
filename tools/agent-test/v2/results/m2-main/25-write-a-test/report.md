1. **Built:** yes, on attempt 1 of 8. I didn't run `fib_is_right`, because the only permitted commands were the compile and the lookups.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading:** nothing in the language or its documentation misled me. I made two small choices:
   - `fib` uses `wrapping_add`. `fib(47)` is the last value that fits in `u32`, but a plain `a + b` would trap while computing the unused `fib(48)` on the last loop pass. This keeps `fib_export(47)` from trapping.
   - `frame` has an empty body, since the task has no GPU work.

`pkg/main.wrela` has the iterative `fib` and the `@test fn fib_is_right()`, which asserts fib(0), fib(1), fib(2), fib(10) and fib(20). It also has `pub fn fib_export(n: u32) -> u32`, which returns `fib(n)`, and `pub fn frame(time: f32, width: u32, height: u32)`.