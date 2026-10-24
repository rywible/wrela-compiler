1. **Built:** yes, on attempt 1 of 8.
2. **Failed attempts:** none, so there are no diagnostics to report.
3. **Misleading:** nothing in the language or its documentation misled me. I did not run the exports, so the build is my only check. By reading the code, `run()` should return 12.0, which is (1 + 2 + 3) × 2.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/23-borrow-struct/pkg/main.wrela`. It defines `borrow struct Ctx { xs: borrow Vec<f32>, k: f32 }` and `scaled_sum(c: Ctx) -> f32`. It exports `run()` and an empty `frame`.