1. **Built:** yes, on attempt 1. The compiler printed no diagnostics.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading documentation:** nothing misled me. I didn't run the program, so the build is the only check I have. By reading the code, `where_negative` should return 2 and `none_negative` should return 99.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/14-option/pkg/main.wrela`. `first_negative` loops over `0..xs.len()` and returns `Some(i)` at the first element below `0.0`, or `None` otherwise. Each export builds its array, matches on the result, and turns `None` into 99. `frame` has an empty body, since the task has no GPU work.