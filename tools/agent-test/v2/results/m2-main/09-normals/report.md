1. **Built:** yes, on attempt 1 of 8.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading parts of the language or docs:** nothing misled me.
   - I wrote `fn blob() -> Surface` with `sphere(radius: …)`, `.translate(vec3(…))` and `.smooth_union(small, k: 0.1)`. I took these from §20.3, §22 and the `hello-field` example, and they compiled as written.
   - The normal is `normalize(blob().gradient(vec3(x, y, z)))`, the derived gradient normalized.
   - I added an empty `frame`, as the brief requires.
   - I didn't run `normal_at` on any point. The build compiling is the only check, so I haven't confirmed the normals are correct.

The program is at `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/09-normals/pkg/main.wrela`.