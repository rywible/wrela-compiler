1. **It built on attempt 1.** The compiler printed `attempt 1: it builds` with no diagnostics. I didn't run `@test fn fib_is_right()`, because I can only compile with the `attempt` command. I checked by hand that its asserts match the Fibonacci values.

   The program is in `pkg/main.wrela`:
   - `fn fib(n: u32) -> u32` is iterative. It returns 0 for `n == 0`. Otherwise it starts with `a = 0`, `b = 1` and loops `1..n`, so it never computes a value past `fib(n)`. Every `n` up to 47 fits a `u32` without overflow.
   - `pub fn fib_export(n: u32) -> u32` returns `fib(n)`.
   - `@test fn fib_is_right()` asserts five values: `fib(0) == 0`, `fib(1) == 1`, `fib(2) == 1`, `fib(10) == 55` and `fib(20) == 6765`.
   - `pub fn frame(time: f32, width: u32, height: u32) {}` has an empty body, as the brief allows.

2. **No attempt failed to build,** so there are no diagnostics to report.

3. **Nothing in the language or its documentation misled me.** One thing is worth knowing: §11.3 says integer overflow traps on the CPU. A simple `a + b` loop that runs one step too far would trap at `n = 47`, so I wrote the loop to stop at `fib(n)`.