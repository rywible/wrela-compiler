1. **Built on attempt 1.** The compiler printed no diagnostics, so no attempt failed to build.

2. **Failed attempts:** none, so no diagnostic codes to report.

3. **Misleading:** nothing in the language or its documentation misled me. I did not check the program's output, because the only command allowed was the build. By hand, `scaled(s)` gives `(2s+1)s+1`.

The program is `pkg/main.wrela`:

```wrela
fn apply_twice(f: fn(f32) -> f32, x: f32) -> f32 {
    f(f(x))
}

pub fn scaled(s: f32) -> f32 {
    apply_twice(|x| x * s + 1.0, 2.0)
}

pub fn frame(time: f32, width: u32, height: u32) {}
```

The spec covers every piece I used:
- §6.7 says a function-typed parameter is borrowed, can take a closure, and is called like any function.
- §6.7 also says a closure that captures only values, like the `f32` `s`, is an ordinary value.
- `frame` has an empty body because the task has no GPU work.