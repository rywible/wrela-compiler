(1) **It built on attempt 1.** The program is at `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/06-field/pkg/main.wrela`.

`blob()` returns the sphere (`radius: 0.5`) smoothly united with the round cone (`k: 0.1`), then translated by `vec3(y: -0.5)`. The cone runs from `vec3()` to `vec3(y: 1.0)`, with `r_top: 0.3` and `r_bottom: 0.1`. `distance_at(x, y, z)` returns `blob().distance(vec3(x, y, z))`. `frame` has an empty body.

I haven't checked the distances it returns. The one thing I'm not sure of is which end of the cone `r_top` belongs to. The spec's `leg_segment` example puts `r_top` on the first point `a` and `r_bottom` on the second, so I gave the 0.3 radius to the origin end, as the task says.

(2) No attempt failed to build, so there are no diagnostics to report.

(3) Nothing misled me, but two points were unclear:
- **`round_cone` radii:** The spec never says which end `r_top` and `r_bottom` belong to. I inferred it from `leg_segment` in §3.1, where `r_top` goes with the first point.
- **Sphere argument:** The spec shows both `sphere(0.5)` and `sphere(radius: 0.5)`. I used the named form.