1. **Built:** yes, on attempt 1.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading:** nothing misled me. I checked the program against the spec but did not run it, since the brief allows only the compile command. The relevant sections are §6.7 (a closure that captures only a `Copy` value, like `s`, passes to a `fn(f32) -> f32` parameter) and §15.1 (a stateless program exports `frame(time, width, height)`).

The program is in `pkg/main.wrela`:

```wrela
fn apply_twice(f: fn(f32) -> f32, x: f32) -> f32 {
    f(f(x))
}

pub fn scaled(s: f32) -> f32 {
    apply_twice(|x| x * s + 1.0, 2.0)
}

pub fn frame(time: f32, width: u32, height: u32) {}
```

`scaled(s)` starts at 2 and applies `x * s + 1` twice, so it returns `(2s + 1) * s + 1`. For `s = 3` that is 22.