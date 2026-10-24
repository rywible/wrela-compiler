1. **Built:** yes, on attempt 1 of 8.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading parts:** nothing in the language or its documentation misled me.

The program is `pkg/main.wrela`. I haven't run it, so the shading is untested beyond the build. Each frame it draws one full-screen triangle (`cover`). The fragment shader (`shade`) does the rest:

- **Camera and field:** the camera is at (0, 0, -3) and looks along +z. The field is `sphere(radius: 1.0)`.
- **Tracing:** it steps along the ray with `field.distance`, at most 64 steps. It stops when the distance is under 0.001 or the ray passes 10 units.
- **Hits:** it shades them with `normalize(field.gradient(p)) * 0.5 + 0.5`, using the derived gradient.
- **Misses:** they get dark grey, `vec4(0.1, 0.1, 0.1, 1.0)`.

I took the field of view (a 1.6 focal length) and the 10-unit far limit from the `hello-field` example, since the brief doesn't give them.