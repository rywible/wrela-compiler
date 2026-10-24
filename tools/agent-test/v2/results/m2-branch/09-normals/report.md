1. **Built:** yes, on attempt 1. The compiler printed no diagnostics. I couldn't run `normal_at`, since the brief allows no way to run the program. The result is unchecked beyond compiling.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading or unclear points:** none that affected the build.
   - **Gradient check:** I relied on §14.1 and §22's `normalize(field.gradient(p))` example, but I didn't check `gradient(p)` at a point where it's undefined (such as the sphere's centre).
   - **Sphere argument:** §20.3 lists `sphere` as a primitive without giving its signature. I took the named form `sphere(radius: 0.5)` from the `hello-field` example, and it compiled.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/09-normals/pkg/main.wrela`. It builds the two-sphere field (radius 0.5 at the origin, radius 0.3 at (0.6, 0, 0)) with `smooth_union(k: 0.1)`. `normal_at(x, y, z)` returns the normalized gradient the compiler derives at that point. `frame` has an empty body.