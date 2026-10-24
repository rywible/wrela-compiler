**(1) Did it build?** Yes, on attempt 1 of 8, with no diagnostics. I didn't run any of the exports, so I haven't seen `bump_fourth` return 1.0. I think it does from reading the code.

The program is in `pkg/main.wrela`:

```wrela
pub struct World {
    pub values: [f32; 8],
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

`bump_fourth` makes a world of zeros and borrows element 3 through `element`. It adds 1.0 through that borrow, then reads element 3 after the borrow's last use, so the borrow checker has no conflict.

**(2) Failed attempts:** none.

**(3) Anything that misled me:** nothing did. §6.4 gives a `-> mut T` function with an example (`mut world.grazers[h]`), and §6.3 gives the `mut x = place` binding. I copied both forms.

**One loose end:** the spec doesn't say outright that `e += 1.0` works on a `mut` binding of a scalar. It compiled, but the spec only shows field assignment (`g.root.pos.y += 1m`).