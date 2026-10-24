1. **Built on attempt 1.** The compiler printed `attempt 1: it builds`. I didn't run any tests, so I haven't checked the behavior beyond the build. By my reading it returns 1.0.

2. **No failed attempts,** so there are no diagnostics to report.

3. **Nothing in the language or its documentation misled me.** One thing I assumed but didn't check: §6.3 shows `+=` through a mutable projection only for a struct field (`g.root.pos.y += 1m`). I applied the same rule to a bare `mut f32` binding (`e += 1.0`), and the compiler accepted it.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/11-projection/pkg/main.wrela`:

```wrela
struct World {
    values: [f32; 8],
}

fn element(w: mut World, i: u32) -> mut f32 {
    mut w.values[i]
}

pub fn bump_fourth() -> f32 {
    var w = World { values: [0.0; 8] }
    mut e = element(mut w, 3)
    e += 1.0
    w.values[3]
}

pub fn frame(time: f32, width: u32, height: u32) {}
```

`w.values[3]` is read after the last use of `e`, so the mutable loan has ended by then.