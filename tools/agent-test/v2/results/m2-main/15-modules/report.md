1. **Built on attempt 1.**

   `geometry.wrela` has `pub fn rotate(v: vec2, angle: f32) -> vec2`. It returns `vec2(v.x*cos(a) - v.y*sin(a), v.x*sin(a) + v.y*cos(a))`, which is counter-clockwise about the origin. `main.wrela` has `use geometry::rotate` and `pub fn rotated_x(angle: f32) -> f32`, which returns `rotate(vec2(1.0, 0.0), angle).x`. It also has an empty `pub fn frame(time: f32, width: u32, height: u32)`.

   I didn't run any tests of the values. I'm confident `rotated_x` is right because it reduces to `cos(angle)`.

2. No attempt failed, so there were no diagnostics to report.

3. Nothing in the language or its documentation misled me. One small inconsistency: §3.6 shows `use shapes::blob::blob`, and the `hello-field` example writes `use shape::creature`. Both forms are the same single-name import without braces, so it didn't affect this task.