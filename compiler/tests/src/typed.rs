//! Random **well-typed** numeric programs, for the oracles that check the compiler against
//! itself (`compiler/tests/tests/suite/typed.rs`; the plan's stage A).
//!
//! A program is built type-first: [`Gen::expr`] picks only productions that give the type it
//! was asked for, so every program checks by construction. Every random choice goes through one
//! buffer ([`Choices`]), so a failing program shrinks without a shrinker that knows the grammar
//! (MacIver and Donaldson's choice sequence): choice 0 is always the simplest production, so a
//! buffer of zeros is the smallest program, and [`shrink`] makes the buffer smaller while the
//! failure stays.
//!
//! The function under test is `fn f(p: vec3) -> f32`, with up to three helpers and a constant
//! table before it. [`HARNESS`] is appended to every program: its exports evaluate `f`, its
//! gradient, its interval and its tape on the CPU, and its kernels evaluate `f` and its interval
//! on the GPU. No production allocates or reads a `Vec`, a `String` or a `Box`, since three of
//! the oracles derive `f` and a derived function can't touch heap memory (language.md §14.2).

use crate::Rng;

/// Every random choice a program is made from: drawn and recorded while generating, or replayed
/// from a buffer (each entry taken modulo the number of choices, 0 past its end).
#[derive(Clone, Debug)]
pub struct Choices {
    data: Vec<u32>,
    at: usize,
    rng: Option<Rng>,
}

impl Choices {
    /// Fresh choices from `seed`, recorded as they're drawn.
    pub fn random(seed: u64) -> Choices {
        Choices { data: Vec::new(), at: 0, rng: Some(Rng::new(seed)) }
    }

    /// The choices in `data`, replayed.
    pub fn replay(data: &[u32]) -> Choices {
        Choices { data: data.to_vec(), at: 0, rng: None }
    }

    /// A number in `0..n` (`n` at least 1).
    pub fn draw(&mut self, n: u32) -> u32 {
        assert!(n > 0, "a choice of nothing");
        match &mut self.rng {
            Some(rng) => {
                let v = rng.below(n as usize) as u32;
                self.data.push(v);
                v
            }
            None => {
                let v = self.data.get(self.at).map_or(0, |d| d % n);
                self.at += 1;
                v
            }
        }
    }

    /// The buffer: what was drawn, or what is replayed.
    pub fn data(&self) -> &[u32] {
        &self.data
    }
}

/// Which programs: **tame** ones, whose every operation is total and finite on the domain by
/// the guards the generator writes, or **wild** ones, with no guards (only built and run: their
/// values may be NaN). **Loops** programs are wild ones' shape over a `[f32; 64]` and integer
/// sums, for the optimizer's oracle alone (stage L).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Tame,
    Wild,
}

/// A switch for each production family, all on by default: one turned off names the bug it
/// avoids in a comment where it's turned off.
#[derive(Clone, Copy, Debug)]
pub struct Features {
    pub loops: bool,
    pub breaks: bool,
    pub tables: bool,
    pub helpers: bool,
    pub pow: bool,
    pub select: bool,
    pub branches: bool,
    pub vectors: bool,
    pub transcendentals: bool,
    /// The most nodes an expression tree may have, and how deep it may nest.
    pub nodes: u32,
    pub depth: u32,
}

impl Default for Features {
    fn default() -> Features {
        Features {
            loops: true,
            breaks: true,
            tables: true,
            helpers: true,
            pow: true,
            select: true,
            branches: true,
            vectors: true,
            transcendentals: true,
            nodes: 60,
            depth: 6,
        }
    }
}

/// A value's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    F32,
    Vec2,
    Vec3,
    Bool,
}

/// What's in scope: a name and its type, and whether anything read it.
#[derive(Clone, Debug)]
struct Var {
    name: String,
    ty: Ty,
    used: bool,
}

/// A helper function: its name and its parameters' types (it returns an `f32`).
#[derive(Clone, Debug)]
struct Helper {
    name: String,
    params: Vec<Ty>,
    used: bool,
}

const LITERALS: [&str; 8] = ["0.0", "1.0", "-1.0", "0.5", "2.0", "0.25", "3.0", "0.1"];

/// The generator: one program's choices, what's in scope, and what's been spent.
pub struct Gen<'a> {
    c: &'a mut Choices,
    mode: Mode,
    feat: Features,
    nodes: u32,
    scope: Vec<Var>,
    helpers: Vec<Helper>,
    /// The counter of the loop being written, which `f32(i)` may read.
    counter: Option<(String, bool)>,
    next: u32,
}

impl Gen<'_> {
    fn fresh(&mut self, stem: &str) -> String {
        self.next += 1;
        format!("{stem}{}", self.next - 1)
    }

    fn lit(&mut self) -> String {
        let text = if self.c.draw(2) == 0 {
            LITERALS[self.c.draw(LITERALS.len() as u32) as usize].to_string()
        } else {
            format!("{:.2}", f64::from(self.c.draw(801) as i32 - 400) / 100.0)
        };
        if text.starts_with('-') { format!("({text})") } else { text }
    }

    /// A name of type `ty` in scope, if there is one: chosen and marked used.
    fn var(&mut self, ty: Ty) -> Option<String> {
        let found: Vec<usize> = (0..self.scope.len()).filter(|&i| self.scope[i].ty == ty).collect();
        if found.is_empty() {
            return None;
        }
        let i = found[self.c.draw(found.len() as u32) as usize];
        self.scope[i].used = true;
        Some(self.scope[i].name.clone())
    }

    /// The simplest expression of `ty`.
    fn leaf(&mut self, ty: Ty) -> String {
        match ty {
            Ty::F32 => match self.c.draw(4) {
                0 => self.lit(),
                1 => self.var(Ty::F32).unwrap_or_else(|| self.lit()),
                2 => match self.var(Ty::Vec3) {
                    Some(v) => format!("{v}.{}", ["x", "y", "z"][self.c.draw(3) as usize]),
                    None => self.lit(),
                },
                _ => match &mut self.counter {
                    Some((name, used)) => {
                        *used = true;
                        format!("f32({name})")
                    }
                    None => self.lit(),
                },
            },
            Ty::Vec3 => match self.c.draw(2) {
                0 => format!("vec3({})", self.lit()),
                _ => self.var(Ty::Vec3).unwrap_or_else(|| "vec3(0.0)".into()),
            },
            Ty::Vec2 => match self.c.draw(2) {
                0 => format!("vec2({})", self.lit()),
                _ => match self.var(Ty::Vec3) {
                    Some(v) => format!("{v}.{}", ["xy", "zx", "yz"][self.c.draw(3) as usize]),
                    None => "vec2(0.0)".into(),
                },
            },
            Ty::Bool => {
                let (a, b) = (self.leaf(Ty::F32), self.leaf(Ty::F32));
                format!("({a} < {b})")
            }
        }
    }

    /// An expression of type `ty`, at most `depth` deep.
    fn expr(&mut self, ty: Ty, depth: u32) -> String {
        if depth == 0 || self.nodes >= self.feat.nodes {
            return self.leaf(ty);
        }
        self.nodes += 1;
        let d = depth - 1;
        match ty {
            Ty::F32 => self.float(d),
            Ty::Vec3 => self.vec3(d),
            Ty::Vec2 => self.vec2(d),
            Ty::Bool => self.boolean(d),
        }
    }

    fn float(&mut self, d: u32) -> String {
        let tame = self.mode == Mode::Tame;
        let f = &self.feat;
        // Each family, in order of simplicity: index 0 is the leaf.
        let families: Vec<u32> = [
            (0, true),
            (1, true),
            (2, true),
            (3, true),
            (4, f.transcendentals),
            (5, f.transcendentals),
            (6, true),
            (7, f.pow),
            (8, true),
            (9, f.vectors),
            (10, f.branches),
            (11, f.select),
            (12, f.helpers && !self.helpers.is_empty()),
            (13, f.tables && tame),
        ]
        .into_iter()
        .filter_map(|(k, on)| on.then_some(k))
        .collect();
        let family = families[self.c.draw(families.len() as u32) as usize];
        match family {
            0 => self.leaf(Ty::F32),
            1 => {
                let op = ["+", "-", "*"][self.c.draw(3) as usize];
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("({a} {op} {b})")
            }
            2 => format!("(-{})", self.expr(Ty::F32, d)),
            3 => {
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                if tame { format!("({a} / (abs({b}) + 0.5))") } else { format!("({a} / {b})") }
            }
            4 => {
                let g = ["sin", "cos", "tanh", "atan", "abs", "floor", "fract"]
                    [self.c.draw(7) as usize];
                format!("{g}({})", self.expr(Ty::F32, d))
            }
            5 => {
                let a = self.expr(Ty::F32, d);
                match (self.c.draw(3), tame) {
                    (0, true) => format!("sqrt(abs({a}))"),
                    (1, true) => format!("exp(clamp({a}, -4.0, 4.0))"),
                    (_, true) => format!("log(abs({a}) + 0.5)"),
                    (0, false) => format!("sqrt({a})"),
                    (1, false) => format!("exp({a})"),
                    (_, false) => format!("log({a})"),
                }
            }
            6 => {
                let g = ["min", "max", "atan2", "step"][self.c.draw(4) as usize];
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("{g}({a}, {b})")
            }
            7 => {
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                if tame {
                    format!("pow(abs({a}) + 0.1, clamp({b}, -2.0, 2.0))")
                } else {
                    format!("pow({a}, {b})")
                }
            }
            8 => {
                let k = self.c.draw(3);
                let (a, b, c) =
                    (self.expr(Ty::F32, d), self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                match (k, tame) {
                    (0, true) => format!("clamp({a}, min({b}, {c}), max({b}, {c}))"),
                    (0, false) => format!("clamp({a}, {b}, {c})"),
                    (1, _) => format!("mix({a}, {b}, {c})"),
                    (_, true) => format!("smoothstep({a}, {a} + abs({b}) + 0.1, {c})"),
                    (_, false) => format!("smoothstep({a}, {b}, {c})"),
                }
            }
            9 => {
                let ty = if self.c.draw(2) == 0 { Ty::Vec3 } else { Ty::Vec2 };
                match self.c.draw(3) {
                    0 => format!("length({})", self.expr(ty, d)),
                    1 => {
                        let (u, v) = (self.expr(ty, d), self.expr(ty, d));
                        format!("dot({u}, {v})")
                    }
                    _ => {
                        let (u, v) = (self.expr(ty, d), self.expr(ty, d));
                        format!("distance({u}, {v})")
                    }
                }
            }
            10 => {
                let c = self.expr(Ty::Bool, d);
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("(if {c} {{ {a} }} else {{ {b} }})")
            }
            11 => {
                let c = self.expr(Ty::Bool, d);
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("select(if_false: {a}, if_true: {b}, cond: {c})")
            }
            12 => {
                let h = self.c.draw(self.helpers.len() as u32) as usize;
                self.helpers[h].used = true;
                let params = self.helpers[h].params.clone();
                let args: Vec<String> = params.iter().map(|&t| self.expr(t, d)).collect();
                format!("{}({})", self.helpers[h].name, args.join(", "))
            }
            _ => format!("T[u32(clamp({}, 0.0, 7.0))]", self.expr(Ty::F32, d)),
        }
    }

    fn vec3(&mut self, d: u32) -> String {
        let tame = self.mode == Mode::Tame;
        let n = if self.feat.vectors { 9 } else { 1 };
        match self.c.draw(n) {
            0 => self.leaf(Ty::Vec3),
            1 => {
                let (a, b, c) =
                    (self.expr(Ty::F32, d), self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("vec3({a}, {b}, {c})")
            }
            2 => {
                let op = ["+", "-", "*"][self.c.draw(3) as usize];
                let (u, v) = (self.expr(Ty::Vec3, d), self.expr(Ty::Vec3, d));
                format!("({u} {op} {v})")
            }
            3 => {
                let (u, s) = (self.expr(Ty::Vec3, d), self.expr(Ty::F32, d));
                format!("({u} * {s})")
            }
            4 => format!("(-{})", self.expr(Ty::Vec3, d)),
            5 => match self.c.draw(4) {
                0 => format!("abs({})", self.expr(Ty::Vec3, d)),
                1 | 2 => {
                    let g = if self.c.draw(2) == 0 { "min" } else { "max" };
                    let (u, v) = (self.expr(Ty::Vec3, d), self.expr(Ty::Vec3, d));
                    format!("{g}({u}, {v})")
                }
                _ => {
                    let (u, v, t) =
                        (self.expr(Ty::Vec3, d), self.expr(Ty::Vec3, d), self.expr(Ty::F32, d));
                    format!("mix({u}, {v}, {t})")
                }
            },
            6 => {
                let (u, v) = (self.expr(Ty::Vec3, d), self.expr(Ty::Vec3, d));
                format!("cross({u}, {v})")
            }
            7 => {
                if tame {
                    let (a, b, c) =
                        (self.expr(Ty::F32, d), self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                    format!(
                        "normalize(vec3(tanh({a}), tanh({b}), tanh({c})) + vec3(0.0, 0.0, 2.5))"
                    )
                } else {
                    format!("normalize({})", self.expr(Ty::Vec3, d))
                }
            }
            _ => {
                let s = ["yzx", "zxy", "xzy", "zyx", "xxy", "zzz"][self.c.draw(6) as usize];
                format!("{}.{s}", self.paren(Ty::Vec3, d))
            }
        }
    }

    /// An expression of `ty` that a field access may follow: a name, or one in parentheses.
    fn paren(&mut self, ty: Ty, d: u32) -> String {
        let e = self.expr(ty, d);
        if e.chars().all(|c| c.is_alphanumeric() || c == '_') { e } else { format!("({e})") }
    }

    fn vec2(&mut self, d: u32) -> String {
        match self.c.draw(4) {
            0 => self.leaf(Ty::Vec2),
            1 => {
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("vec2({a}, {b})")
            }
            2 => {
                let op = ["+", "-", "*"][self.c.draw(3) as usize];
                let (u, v) = (self.expr(Ty::Vec2, d), self.expr(Ty::Vec2, d));
                format!("({u} {op} {v})")
            }
            _ => {
                let s = ["xy", "yz", "zx"][self.c.draw(3) as usize];
                format!("{}.{s}", self.paren(Ty::Vec3, d))
            }
        }
    }

    fn boolean(&mut self, d: u32) -> String {
        match self.c.draw(6) {
            0 => self.leaf(Ty::Bool),
            k @ 1..=3 => {
                let op = ["<", ">", "<="][k as usize - 1];
                let (a, b) = (self.expr(Ty::F32, d), self.expr(Ty::F32, d));
                format!("({a} {op} {b})")
            }
            4 => {
                let op = if self.c.draw(2) == 0 { "&&" } else { "||" };
                let (a, b) = (self.expr(Ty::Bool, d), self.expr(Ty::Bool, d));
                format!("({a} {op} {b})")
            }
            _ => format!("!{}", self.paren(Ty::Bool, d)),
        }
    }

    /// A fresh expression tree's budget.
    fn tree(&mut self, ty: Ty) -> String {
        self.nodes = 0;
        let depth = self.feat.depth;
        self.expr(ty, depth)
    }

    /// The body of `f`: zero to four statements, then its value. Each local not otherwise read
    /// is added in, so no statement is dead.
    fn body(&mut self) -> String {
        let mut out = String::new();
        let n = self.c.draw(5);
        for _ in 0..n {
            let kinds = if self.feat.loops { 3 } else { 2 };
            match self.c.draw(kinds) {
                0 => {
                    let ty = [Ty::F32, Ty::Vec3, Ty::Vec2, Ty::Bool][self.c.draw(4) as usize];
                    let e = self.tree(ty);
                    let name = self.fresh("x");
                    out += &format!("    let {name} = {e}\n");
                    self.scope.push(Var { name, ty, used: false });
                }
                1 => {
                    let start = self.tree(Ty::F32);
                    let name = self.fresh("acc");
                    out += &format!("    var {name} = {start}\n");
                    let c = self.tree(Ty::Bool);
                    let e = self.tree(Ty::F32);
                    out += &format!("    if {c} {{\n        {name} = {e}\n    }}\n");
                    self.scope.push(Var { name, ty: Ty::F32, used: false });
                }
                _ => {
                    let start = self.tree(Ty::F32);
                    let acc = self.fresh("acc");
                    let count = 1 + self.c.draw(6);
                    let i = self.fresh("i");
                    out += &format!("    var {acc} = {start}\n");
                    self.scope.push(Var { name: acc.clone(), ty: Ty::F32, used: true });
                    self.counter = Some((i.clone(), false));
                    let leave = if self.feat.breaks && self.c.draw(2) == 1 {
                        let c = self.tree(Ty::Bool);
                        format!("        if {c} {{\n            break\n        }}\n")
                    } else {
                        String::new()
                    };
                    let g = self.tree(Ty::F32);
                    let (name, used) = self.counter.take().expect("the loop's counter");
                    let name = if used { name } else { "_".into() };
                    out += &format!(
                        "    for {name} in 0..{count} {{\n{leave}        {acc} = {acc} + {g}\n    }}\n"
                    );
                    if let Some(v) = self.scope.iter_mut().find(|v| v.name == acc) {
                        v.used = false;
                    }
                }
            }
        }
        let mut value = self.tree(Ty::F32);
        for v in self.scope.iter().filter(|v| !v.used && v.name != "p") {
            value = match v.ty {
                Ty::F32 => format!("{value} + {}", v.name),
                Ty::Vec3 => format!("{value} + {}.y", v.name),
                Ty::Vec2 => format!("{value} + {}.x", v.name),
                Ty::Bool => format!("{value} + (if {} {{ 1.0 }} else {{ 0.0 }})", v.name),
            };
        }
        out + "    " + &value + "\n"
    }
}

/// A program's text (without [`HARNESS`]) from `choices`.
pub fn program(choices: &mut Choices, mode: Mode, feat: Features) -> String {
    let mut g = Gen {
        c: choices,
        mode,
        feat,
        nodes: 0,
        scope: Vec::new(),
        helpers: Vec::new(),
        counter: None,
        next: 0,
    };
    let mut out = String::new();
    // The table's values are drawn first, so a program that reads it needs no other change;
    // it's written only where it's read.
    let table: Vec<String> = (0..8).map(|_| g.lit().replace(['(', ')'], "")).collect();
    let helpers = if feat.helpers { g.c.draw(4) } else { 0 };
    for k in 0..helpers {
        let params = match g.c.draw(3) {
            0 => vec![Ty::F32],
            1 => vec![Ty::F32, Ty::Vec3],
            _ => vec![Ty::Vec3],
        };
        let names = ["a", "b"];
        g.scope = params
            .iter()
            .zip(names)
            .map(|(&ty, name)| Var { name: name.into(), ty, used: false })
            .collect();
        let value = g.tree(Ty::F32);
        let unused: Vec<String> = g
            .scope
            .iter()
            .filter(|v| !v.used)
            .map(|v| if v.ty == Ty::F32 { v.name.clone() } else { format!("{}.x", v.name) })
            .collect();
        let value = unused.iter().fold(value, |e, u| format!("{e} + {u}"));
        let sig: Vec<String> = params
            .iter()
            .zip(names)
            .map(|(&t, n)| format!("{n}: {}", if t == Ty::F32 { "f32" } else { "vec3" }))
            .collect();
        let name = format!("h{k}");
        out += &format!("fn {name}({}) -> f32 {{\n    {value}\n}}\n\n", sig.join(", "));
        g.helpers.push(Helper { name, params, used: false });
    }
    g.scope = vec![Var { name: "p".into(), ty: Ty::Vec3, used: false }];
    let body = g.body();
    // `p` read somewhere, or the function is a constant: still a program, but say it uses `p`.
    let body = if g.scope[0].used { body } else { format!("    let _ = p\n{body}") };
    // A helper nothing calls is called from `f`, so none is unused (W0008).
    let mut body = body;
    for h in g.helpers.iter().filter(|h| !h.used) {
        let args: Vec<&str> =
            h.params.iter().map(|t| if *t == Ty::F32 { "p.x" } else { "p" }).collect();
        let last = body.trim_end().rfind('\n').map_or(0, |i| i + 1);
        let (head, value) = body.split_at(last);
        body = format!("{head}{} + {}({})\n", value.trim_end(), h.name, args.join(", "));
    }
    out += &format!("fn f(p: vec3) -> f32 {{\n{body}}}\n");
    if out.contains("T[u32(") {
        out = format!("const T: [f32; 8] = [{}]\n\n{out}", table.join(", "));
    }
    out
}

/// What every program ends with: the exports and kernels the oracles call.
pub const HARNESS: &str = "
// ---- the harness (compiler/tests/src/typed.rs) -------------------------------------------------
use std::derive::{Box3, gradient, interval}
use std::field::hash_u32
use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

pub fn value(x: f32, y: f32, z: f32) -> f32 {
    f(vec3(x, y, z))
}

pub fn grad(x: f32, y: f32, z: f32) -> vec3 {
    gradient(f, vec3(x, y, z))
}

pub fn range(lx: f32, ly: f32, lz: f32, hx: f32, hy: f32, hz: f32) -> vec2 {
    let r = interval(f, Box3 { lo: vec3(lx, ly, lz), hi: vec3(hx, hy, hz) })
    vec2(r.lo, r.hi)
}

pub struct Outs {
    values: GpuBuffer<vec4>,
    ranges: GpuBuffer<vec2>,
}

pub fn init() -> Outs {
    Outs { values: buffer(0), ranges: buffer(0) }
}

struct Seed: Copy + GpuData {
    seed: u32,
    count: u32,
}

fn unit(h: u32) -> f32 {
    f32(h >> 8) * (1.0 / 16777216.0)
}

/// Point `i` of `seed`'s points in [-2, 2]³.
fn point(seed: u32, i: u32) -> vec3 {
    let h0 = hash_u32(i ^ seed)
    let h1 = hash_u32(h0)
    let h2 = hash_u32(h1)
    vec3(unit(h0), unit(h1), unit(h2)) * 4.0 - vec3(2.0)
}

@compute(64)
fn values_k(s: Seed, out: mut Slots<vec4>, id: GlobalId) {
    if id.x < s.count {
        let p = point(s.seed, id.x)
        out[id] = vec4(p, f(p))
    }
}

@compute(64)
fn ranges_k(s: Seed, out: mut Slots<vec2>, id: GlobalId) {
    if id.x < s.count {
        let p = point(s.seed, id.x)
        let r = interval(f, Box3 { lo: p, hi: p })
        out[id] = vec2(r.lo, r.hi)
    }
}

/// `count` points of `seed` on the GPU: each point and `f` there, and `f`'s interval over it.
pub fn on_gpu(state: mut Outs, seed: u32, count: u32) {
    let s = Seed { seed, count }
    state.values = buffer(count)
    state.ranges = buffer(count)
    dispatch(values_k.bind(s: s, out: mut state.values), groups: (count + 63) / 64)
    dispatch(ranges_k.bind(s: s, out: mut state.ranges), groups: (count + 63) / 64)
}

pub fn frame(state: mut Outs, time: f32, width: u32, height: u32) {}
";

/// What a program built for the tape oracle (O4) has besides [`HARNESS`]: `f` through its tape.
/// It's apart because a tape can't hold everything `f` may do (E0709), and that mustn't stop
/// the other oracles.
pub const TAPE_HARNESS: &str = "
use std::derive::tape
use std::stage::interpret

pub fn taped(x: f32, y: f32, z: f32) -> f32 {
    interpret(tape(f), vec3(x, y, z))
}
";

/// A whole program: [`program`] and the [`HARNESS`].
pub fn source(choices: &mut Choices, mode: Mode, feat: Features) -> String {
    program(choices, mode, feat) + HARNESS
}

/// The program `seed` makes, and its choices.
pub fn generate(seed: u64, mode: Mode, feat: Features) -> (String, Vec<u32>) {
    let mut c = Choices::random(seed);
    let text = source(&mut c, mode, feat);
    (text, c.data().to_vec())
}

/// The program `data` replays.
pub fn replay(data: &[u32], mode: Mode, feat: Features) -> String {
    source(&mut Choices::replay(data), mode, feat)
}

/// Makes `data` smaller while `fails` holds of it: (1) the tail cut, halving; (2) blocks of 8,
/// 4, 2, 1 entries zeroed; (3) single entries lowered. A change is kept when `fails` still
/// holds; it stops when a whole pass changes nothing or after `tries` calls of `fails`.
pub fn shrink(data: &[u32], tries: u32, mut fails: impl FnMut(&[u32]) -> bool) -> Vec<u32> {
    let mut best = data.to_vec();
    let spent = std::cell::Cell::new(0);
    let mut attempt = |cand: &[u32], best: &mut Vec<u32>| -> bool {
        if spent.get() >= tries || cand == best.as_slice() {
            return false;
        }
        spent.set(spent.get() + 1);
        if fails(cand) {
            *best = cand.to_vec();
            true
        } else {
            false
        }
    };
    loop {
        let before = best.clone();
        // (1) The tail, cut by halves.
        let mut cut = best.len() / 2;
        while cut > 0 {
            let cand = best[..best.len() - cut].to_vec();
            if !attempt(&cand, &mut best) {
                cut /= 2;
            }
        }
        // (2) Blocks zeroed.
        for size in [8, 4, 2, 1] {
            let mut i = 0;
            while i < best.len() {
                let end = (i + size).min(best.len());
                if best[i..end].iter().any(|&x| x != 0) {
                    let mut cand = best.clone();
                    cand[i..end].iter_mut().for_each(|x| *x = 0);
                    attempt(&cand, &mut best);
                }
                i += size;
            }
        }
        // (3) Single entries lowered: halved, then less one.
        for i in 0..best.len() {
            for lower in [best[i] / 2, best[i].saturating_sub(1)] {
                if lower < best[i] {
                    let mut cand = best.clone();
                    cand[i] = lower;
                    attempt(&cand, &mut best);
                }
            }
        }
        if best == before || spent.get() >= tries {
            return best;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_text() {
        for mode in [Mode::Tame, Mode::Wild] {
            for seed in 0..50 {
                let (a, data) = generate(seed, mode, Features::default());
                let (b, _) = generate(seed, mode, Features::default());
                assert_eq!(a, b);
                assert_eq!(replay(&data, mode, Features::default()), a, "seed {seed} replays");
            }
        }
    }

    #[test]
    fn a_buffer_of_zeros_is_the_smallest_program() {
        let zeros = replay(&[], Mode::Tame, Features::default());
        for seed in 0..200 {
            let (text, _) = generate(seed, Mode::Tame, Features::default());
            assert!(zeros.len() <= text.len(), "seed {seed} is smaller:\n{text}");
        }
        assert!(zeros.contains("fn f(p: vec3) -> f32 {\n    let _ = p\n    0.0\n}"), "{zeros}");
    }

    #[test]
    fn shrinking_keeps_the_failure() {
        // A "failure": the program reads the table.
        let fails = |d: &[u32]| replay(d, Mode::Tame, Features::default()).contains("T[u32(");
        let seed = (0..1000)
            .find(|&s| generate(s, Mode::Tame, Features::default()).0.contains("T[u32("))
            .expect("a program that reads the table");
        let (text, data) = generate(seed, Mode::Tame, Features::default());
        let small = shrink(&data, 2000, fails);
        let shrunk = replay(&small, Mode::Tame, Features::default());
        assert!(shrunk.contains("T[u32("));
        assert!(shrunk.len() < text.len(), "{shrunk}");
    }

    /// 200 seeds in each mode check with no error.
    #[test]
    fn every_program_checks() {
        let root = std::env::temp_dir().join(format!("wrela-typed-check-{}", std::process::id()));
        let seeds: Vec<(u64, Mode)> =
            (0..200).flat_map(|s| [(s, Mode::Tame), (s, Mode::Wild)]).collect();
        let failures = crate::par_map(&seeds, |&(seed, mode)| {
            let (text, _) = generate(seed, mode, Features::default());
            let dir = root.join(format!("{mode:?}-{seed}"));
            std::fs::create_dir_all(&dir).expect("a directory");
            std::fs::write(dir.join("main.wrela"), &text).expect("write");
            let out = wrela_driver::check(&dir);
            let bad: Vec<_> = out
                .diagnostics
                .iter()
                .filter(|d| d.is_error() || d.code.as_str() == "I0001")
                .cloned()
                .collect();
            (!bad.is_empty()).then(|| {
                format!(
                    "{mode:?} seed {seed}:\n{text}\n{}",
                    wrela_diag::render::render_all(&out.sources, &bad)
                )
            })
        });
        let _ = std::fs::remove_dir_all(&root);
        let failures: Vec<String> = failures.into_iter().flatten().collect();
        assert!(failures.is_empty(), "{} don't check; the first:\n{}", failures.len(), failures[0]);
    }
}
