1. **Built:** yes, on attempt 1 of 8.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading parts of the language or docs:** nothing misled me.
   - The spec doesn't list a `reflect` built-in, so I wrote the reflection by hand: `d - n * (2.0 * dot(d, n))`, with `n = vec3(y: 1.0)`.
   - I checked the program by reading it, not by running it. The compiler only confirmed that it builds.
   - A zero input vector would normalize to NaN, as it would in any language.

The program is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/17-reflect/pkg/main.wrela`. It exports `reflect_y` and an empty `frame`.