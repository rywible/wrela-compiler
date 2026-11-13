//! The oracles over typed programs (the plan's stage A): each builds a program [`super`] made
//! and checks the compiler against itself on it. The suite (`suite/typed.rs`) runs them over a
//! fixed set of seeds; the soak binary (`typed-soak`) over as many as it's asked.
//!
//! | | Oracle | What it requires |
//! |---|---|---|
//! | O1 | builds | the program builds, with no error, and runs at 16 points without a trap |
//! | O2 | the interval encloses | over 8 boxes, 16 points each: `lo <= value <= hi` |
//! | O3 | the gradient agrees | with central differences where the function is smooth |
//! | O4 | the tape agrees | `interpret(tape(f), p)` within 3e-6 of `f(p)` (§14.4) |
//! | O5 | the optimizer keeps the bits | a `no_opt` build's values, bit for bit |
//! | O7 | the interpreter agrees | the GPU code interpreted (`wrela_ir::interp`) gives the GPU's value, within O6's bound |
//! | O8 | reverse mode agrees | `std::derive::reverse`'s gradient within `1e-5 (1 + |g|)` of `gradient`'s |
//! | O6 | the targets agree | the GPU's value inside its interval (within 8 ulps: Metal's fast math reassociates), and near the CPU's |

use super::{Features, HARNESS, Mode, REVERSE_HARNESS, TAPE_HARNESS, program};
use crate::{Rng, u32s};
use std::path::{Path, PathBuf};
use wrela_host::{CpuBuild, CpuHost, Host, Value};

/// An oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Oracle {
    O1,
    O2,
    O3,
    O4,
    O5,
    O6,
    O7,
    O8,
    O9,
}

impl Oracle {
    pub const ALL: [Oracle; 9] = [
        Oracle::O1,
        Oracle::O2,
        Oracle::O3,
        Oracle::O4,
        Oracle::O5,
        Oracle::O6,
        Oracle::O7,
        Oracle::O8,
        Oracle::O9,
    ];

    pub fn parse(s: &str) -> Option<Oracle> {
        Oracle::ALL.into_iter().find(|o| o.name().eq_ignore_ascii_case(s))
    }

    pub fn name(self) -> &'static str {
        match self {
            Oracle::O1 => "O1",
            Oracle::O2 => "O2",
            Oracle::O3 => "O3",
            Oracle::O4 => "O4",
            Oracle::O5 => "O5",
            Oracle::O6 => "O6",
            Oracle::O7 => "O7",
            Oracle::O8 => "O8",
            Oracle::O9 => "O9",
        }
    }

    /// The productions its programs use: all, but O4's have no `pow`, since a tape can't hold a
    /// power whose exponent depends on the point (§14.4; with `pow`, most programs had one, and
    /// O4 saw almost nothing).
    pub fn features(self) -> Features {
        match self {
            Oracle::O4 => Features { pow: false, ..Features::default() },
            _ => Features::default(),
        }
    }

    /// The programs it's run on: O1 sees both modes; the rest need finite values.
    pub fn modes(self) -> &'static [Mode] {
        match self {
            Oracle::O1 => &[Mode::Tame, Mode::Wild],
            _ => &[Mode::Tame],
        }
    }
}

/// Set by the soak once it has as many failures as it stops after: a failure found after that
/// isn't shrunk (`check_seed_shrinking`), since the soak has done its job.
pub static ENOUGH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// O9's refusals over a run, by reason (the walk's message, its numbers taken out): what the
/// walk can't prove, which the soak reports.
pub static REFUSALS: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());

fn refused(why: &str) {
    set_apart(&class(why));
}

/// Counts a program (or a point) set apart for `why`, as written.
fn set_apart(why: &str) {
    let key = why.to_string();
    let mut r = REFUSALS.lock().expect("the refusals' lock");
    match r.iter_mut().find(|(k, _)| *k == key) {
        Some((_, n)) => *n += 1,
        None => r.push((key, 1)),
    }
}

std::thread_local! {
    /// O8's worst difference on this thread's programs, relative to the gradient's size.
    pub static WORST_O8: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// What tape limits O4 lets pass: E0709's messages for what §14.4 says a tape can't hold, each
/// a limit of tapes, not a bug.
pub const TAPE_ALLOWED: &[&str] =
    &["a tape can't hold a power whose exponent depends on the point"];

/// The panic of a tape past its room (`std::stage`): a limit of tapes O4 lets pass.
pub const TAPE_FULL: &str = "past its room for 4096 and 2048";

/// What a run of an oracle over one program found: how many points it checked, and how many of
/// them counted (O3: where the function is smooth enough to compare).
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub points: u32,
    pub counted: u32,
}

/// Why an oracle failed on a program: what, and at the first failing point.
#[derive(Clone, Debug)]
pub struct Failure {
    pub what: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.what)
    }
}

/// `a <= b`, false where either is NaN.
fn within<T: PartialOrd>(a: T, b: T) -> bool {
    a <= b
}

fn fail<T>(what: impl Into<String>) -> Result<T, Failure> {
    Err(Failure { what: what.into() })
}

/// The program `data` makes, with the harness.
pub fn text(data: &[u32], mode: Mode, feat: Features) -> String {
    program(&mut super::Choices::replay(data), mode, feat) + HARNESS
}

/// Runs the regression file at `path`: its first lines say the oracle (`// oracle: O3`) and the
/// seed of its points (`// seed: 7`), and the rest is the program with its harness.
pub fn run_file(path: &Path, scratch: &Path) -> Result<Counts, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let oracle = crate::header(&src, "oracle")
        .next()
        .and_then(Oracle::parse)
        .ok_or_else(|| format!("{}: no `// oracle:` line", path.display()))?;
    let seed = crate::header(&src, "seed").next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let name = path.file_stem().map_or("regress".into(), |s| s.to_string_lossy().into_owned());
    let dir = scratch.join(format!("regress-{name}"));
    let ran = run(oracle, &src, seed, &dir).map_err(|f| format!("{}: {f}", path.display()));
    let _ = std::fs::remove_dir_all(&dir);
    ran
}

/// Builds `text` as a package in `dir` (made afresh), with `no_opt` if asked: the build's
/// directory, or the errors rendered.
pub fn build(dir: &Path, text: &str, no_opt: bool) -> Result<PathBuf, Failure> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("make the program's directory");
    std::fs::write(dir.join("wrela.toml"), "[package]\nname = \"typed\"\n").expect("write");
    std::fs::write(dir.join("main.wrela"), text).expect("write main.wrela");
    let out = wrela_driver::build_for_tests_with(dir, false, no_opt);
    let bad: Vec<_> = out
        .diagnostics
        .iter()
        .filter(|d| d.is_error() || d.code.as_str() == "I0001")
        .cloned()
        .collect();
    if !bad.is_empty() {
        return fail(wrela_diag::render::render_all(&out.sources, &bad));
    }
    let build = dir.join("build");
    out.write_to(&build).expect("write the build");
    Ok(build)
}

fn start(build: &Path) -> Result<CpuHost, Failure> {
    CpuBuild::load(build)
        .and_then(|b| b.start_with(1))
        .or_else(|e| fail(format!("the build doesn't load: {e}")))
}

/// A point in [-2, 2]³.
fn point(rng: &mut Rng) -> [f32; 3] {
    [0, 1, 2].map(|_| (rng.unit() * 4.0 - 2.0) as f32)
}

fn args(p: [f32; 3]) -> [Value; 3] {
    p.map(Value::F32)
}

fn value(host: &mut CpuHost, p: [f32; 3]) -> Result<f32, Failure> {
    match host.call_export("value", &args(p)) {
        Ok(v) => match v.as_slice() {
            [Value::F32(x)] => Ok(*x),
            other => fail(format!("value returned {other:?}")),
        },
        Err(e) => fail(format!("trapped at {p:?}: {e}")),
    }
}

/// `range`'s arguments for the box from `lo` to `hi`.
fn ranged(lo: [f32; 3], hi: [f32; 3]) -> Vec<Value> {
    lo.into_iter().chain(hi).map(Value::F32).collect()
}

fn floats(host: &mut CpuHost, name: &str, args: &[Value]) -> Result<Vec<f32>, Failure> {
    match host.call_export(name, args) {
        Ok(v) => v
            .iter()
            .map(|x| match x {
                Value::F32(x) => Ok(*x),
                other => fail(format!("{name} returned {other:?}")),
            })
            .collect(),
        Err(e) => fail(format!("{name} trapped at {args:?}: {e}")),
    }
}

/// Runs `oracle` on the program `src` (with [`HARNESS`]), its files under `dir`, its points
/// from `seed`.
pub fn run(oracle: Oracle, src: &str, seed: u64, dir: &Path) -> Result<Counts, Failure> {
    let mut rng = Rng::new(seed ^ 0x0a11_ce5e_ed00_0000);
    match oracle {
        Oracle::O1 => {
            let mut host = start(&build(dir, src, false)?)?;
            for _ in 0..16 {
                value(&mut host, point(&mut rng))?;
            }
            Ok(Counts { points: 16, counted: 16 })
        }
        Oracle::O2 => o2(&mut start(&build(dir, src, false)?)?, &mut rng),
        Oracle::O3 => o3(&mut start(&build(dir, src, false)?)?, &mut rng),
        Oracle::O4 => {
            let build = match build(dir, &(src.to_string() + TAPE_HARNESS), false) {
                Err(f) if f.what.contains("E0709") => {
                    if TAPE_ALLOWED.iter().any(|a| f.what.contains(a)) {
                        set_apart(
                            "O4: a tape can't hold a power whose exponent depends on the point",
                        );
                        return Ok(Counts::default());
                    }
                    return Err(f);
                }
                other => other?,
            };
            let mut host = start(&build)?;
            for _ in 0..16 {
                let p = point(&mut rng);
                let v = value(&mut host, p)?;
                let t = match floats(&mut host, "taped", &args(p)) {
                    // A tape has room for 4,096 operations and 2,048 constants (§14.4): a field
                    // that needs more is a limit of tapes, which the panic says, not a bug.
                    Err(f) if f.what.contains(TAPE_FULL) => {
                        set_apart("O4: a tape past its room (4,096 operations, 2,048 constants)");
                        return Ok(Counts::default());
                    }
                    other => other?[0],
                };
                let near = 3e-6 * v.abs().max(1.0);
                if within((t - v).abs(), near) {
                    continue;
                }
                // The tape rounds each operation as compiled code does, in another order: where
                // that order matters (a `floor` of a value on an integer, a steep `pow`), what
                // rounding inside `f` can do at the point is the point interval's width (§14.2,
                // as O6 allows the GPU). Past it, the tape is wrong.
                let r = floats(&mut host, "range", &ranged(p, p))?;
                let room = 2.0 * f64::from(r[1] - r[0]);
                if !within(f64::from((t - v).abs()), f64::from(near) + room) {
                    return fail(format!(
                        "at {p:?}: the tape gives {t:e}, f gives {v:e}, apart by more than the \
                         point interval's width ({:e})",
                        r[1] - r[0]
                    ));
                }
                set_apart(
                    "O4 (a point counted, agreeing): apart by more than 3e-6, within the point interval's width",
                );
            }
            Ok(Counts { points: 16, counted: 16 })
        }
        Oracle::O5 => {
            let mut opt = start(&build(dir, src, false)?)?;
            let mut plain = start(&build(&dir.join("no_opt"), src, true)?)?;
            for _ in 0..16 {
                let p = point(&mut rng);
                let (a, b) = (value(&mut opt, p), value(&mut plain, p));
                match (a, b) {
                    (Ok(a), Ok(b)) if a.to_bits() == b.to_bits() => {}
                    (Err(_), Err(_)) => {}
                    (a, b) => {
                        return fail(format!(
                            "at {p:?}: optimized {:?}, not optimized {:?}",
                            a.map(f32::to_bits).map_err(|f| f.what),
                            b.map(f32::to_bits).map_err(|f| f.what)
                        ));
                    }
                }
            }
            Ok(Counts { points: 16, counted: 16 })
        }
        Oracle::O6 => o6(&build(dir, src, false)?, seed),
        Oracle::O7 => o7(dir, &build(dir, src, false)?, seed),
        Oracle::O9 => {
            build(dir, src, false)?;
            o9(dir, seed)
        }
        Oracle::O8 => {
            let mut host = start(&build(dir, &(src.to_string() + REVERSE_HARNESS), false)?)?;
            let mut worst = 0.0f64;
            for _ in 0..16 {
                let p = point(&mut rng);
                let f = floats(&mut host, "grad", &args(p))?
                    .into_iter()
                    .map(f64::from)
                    .collect::<Vec<_>>();
                let r = floats(&mut host, "rgrad", &args(p))?
                    .into_iter()
                    .map(f64::from)
                    .collect::<Vec<_>>();
                let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
                let diff: Vec<f64> = f.iter().zip(&r).map(|(a, b)| a - b).collect();
                let d = norm(&diff) / (1.0 + norm(&f));
                if !within(d, 1e-5) {
                    return fail(format!(
                        "at {p:?}: forward mode gives {f:?}, reverse mode {r:?} ({d:e} apart)"
                    ));
                }
                worst = worst.max(d);
            }
            WORST_O8.with(|w| w.set(w.get().max(worst)));
            Ok(Counts { points: 16, counted: 16 })
        }
    }
}

/// O2: for 8 boxes (a centre in [-2, 2]³, half sizes log-uniform from 1e-4 to 2) and 16 points
/// in each (its corners and 8 inside), the interval holds the value.
fn o2(host: &mut CpuHost, rng: &mut Rng) -> Result<Counts, Failure> {
    for _ in 0..8 {
        let c = point(rng);
        let h = [0, 1, 2].map(|_| 10f64.powf(rng.unit() * 4.3 - 4.0) as f32);
        let lo = [0, 1, 2].map(|i| c[i] - h[i]);
        let hi = [0, 1, 2].map(|i| c[i] + h[i]);
        let r = floats(host, "range", &ranged(lo, hi))?;
        for k in 0..16 {
            let p = if k < 8 {
                [0, 1, 2].map(|i| if k & (1 << i) == 0 { lo[i] } else { hi[i] })
            } else {
                [0, 1, 2].map(|i| (lo[i] + (hi[i] - lo[i]) * rng.unit() as f32).clamp(lo[i], hi[i]))
            };
            let v = value(host, p)?;
            if !(within(r[0], v) && within(v, r[1])) {
                return fail(format!(
                    "the interval over {lo:?}..{hi:?} is [{:e}, {:e}], but f({p:?}) = {v:e}",
                    r[0], r[1]
                ));
            }
        }
    }
    Ok(Counts { points: 128, counted: 128 })
}

/// O3: at 16 points, central differences `D(h)` and `D(h/2)`, `h` = 1e-3. A point counts where
/// they agree within 1e-3 relative in each component (the function is smooth there); at a point
/// that counts, the gradient is within 2e-3 relative of `D(h/2)`. A point also counts only
/// where the differences stand well above the values' own rounding (a large value's ulp over
/// the step) and the point's value is continuous with its neighbours'.
fn o3(host: &mut CpuHost, rng: &mut Rng) -> Result<Counts, Failure> {
    let mut counted = 0;
    for _ in 0..16 {
        let p = point(rng);
        let g = floats(host, "grad", &args(p))?;
        let here = f64::from(value(host, p)?);
        let mut d = [[0.0f64; 3]; 2];
        // Each difference's own rounding: an ulp of the largest value it subtracts, over its
        // step, and whether the point's value lies between its neighbours' (f is continuous
        // there: a branch that flips at the point itself, as a table read at a rounded index
        // can, makes its neighbours' difference another branch's slope).
        // And whether the one-sided differences agree at the smaller step: a corner within a
        // step of the point (a clamp's, a min's) makes both central differences the mean of
        // its two slopes, which agree with each other and with neither side's gradient.
        let (mut noise, mut continuous, mut cornered) = ([0.0f64; 3], true, [0.0f64; 3]);
        for (s, h) in [1e-3f32, 5e-4].into_iter().enumerate() {
            for axis in 0..3 {
                let (mut a, mut b) = (p, p);
                a[axis] += h;
                b[axis] -= h;
                let step = f64::from(a[axis]) - f64::from(b[axis]);
                let (fa, fb) = (f64::from(value(host, a)?), f64::from(value(host, b)?));
                d[s][axis] = (fa - fb) / step;
                let ulp = f64::from(f32::EPSILON) * fa.abs().max(fb.abs());
                noise[axis] = noise[axis].max(4.0 * ulp / step);
                let spread = (fa - fb).abs() + 4.0 * ulp;
                continuous &= (here - 0.5 * (fa + fb)).abs() <= spread + 1e-3 * (1.0 + here.abs());
                let (up, down) = (
                    f64::from(a[axis]) - f64::from(p[axis]),
                    f64::from(p[axis]) - f64::from(b[axis]),
                );
                cornered[axis] = ((fa - here) / up - (here - fb) / down).abs();
            }
        }
        let smooth = continuous
            && (0..3).all(|i| {
                (d[0][i] - d[1][i]).abs() <= 1e-3 * (1.0 + d[0][i].abs())
                    && noise[i] <= 1e-3 * (1.0 + d[1][i].abs())
                    && cornered[i] <= 2e-3 * (1.0 + d[1][i].abs())
            });
        if !smooth {
            continue;
        }
        counted += 1;
        for i in 0..3 {
            let (g, fd) = (f64::from(g[i]), d[1][i]);
            if !within((g - fd).abs(), 2e-3 * (1.0 + fd.abs())) {
                return fail(format!(
                    "at {p:?}: the gradient's component {i} is {g:e}, central differences {fd:e}"
                ));
            }
        }
    }
    Ok(Counts { points: 16, counted })
}

/// O6: at 64 points on the GPU, the GPU's value lies in the GPU's interval over the point, and
/// the CPU's and the GPU's values differ by at most the widths of the two point intervals.
fn o6(build: &Path, seed: u64) -> Result<Counts, Failure> {
    const N: u32 = 64;
    let mut gpu = Host::load(build).or_else(|e| fail(format!("the GPU host: {e}")))?;
    let at = [Value::I32(seed as u32 as i32)];
    gpu.call_export("on_gpu", &at).or_else(|e| fail(format!("on_gpu: {e}")))?;
    let made = gpu.buffers();
    let [.., values, ranges] = made[..] else { return fail("on_gpu made no buffers") };
    let values = u32s(&gpu.read_buffer(values).or_else(|e| fail(format!("read: {e}")))?);
    let ranges = u32s(&gpu.read_buffer(ranges).or_else(|e| fail(format!("read: {e}")))?);
    let mut cpu = start(build)?;
    let mut reassociated = 0;
    for i in 0..N as usize {
        let p = [0, 1, 2].map(|k| f32::from_bits(values[4 * i + k]));
        let gv = f32::from_bits(values[4 * i + 3]);
        let (glo, ghi) = (f32::from_bits(ranges[2 * i]), f32::from_bits(ranges[2 * i + 1]));
        let cv = value(&mut cpu, p)?;
        let r = floats(&mut cpu, "range", &ranged(p, p))?;
        if !(within(glo, gv) && within(gv, ghi)) {
            // The native host's GPU on Metal compiles with Metal's fast math (wgpu 30 leaves
            // `MTLCompileOptions` at its default), which reassociates sums: a value a few ulps
            // outside the interval derived op by op. That's reported (not counted), not failed;
            // a miss larger than 8 ulps of the value, or of 1 under 1 (a reassociated sum
            // cancels: its error is its operands'), fails: a NaN, a wrong branch.
            let miss = if gv < glo { glo - gv } else { gv - ghi };
            let ulp = f32::EPSILON * gv.abs().max(1.0);
            if !within(miss, 8.0 * ulp) {
                return fail(format!(
                    "at {p:?}: the GPU's value {gv:e} is outside [{glo:e}, {ghi:e}]"
                ));
            }
            reassociated += 1;
        }
        let room = f64::from(r[1] - r[0]) + f64::from(ghi - glo);
        if !within((f64::from(gv) - f64::from(cv)).abs(), room) {
            return fail(format!(
                "at {p:?}: the GPU gives {gv:e} and the CPU {cv:e}, more apart than the point \
                 intervals' widths ({room:e})"
            ));
        }
    }
    Ok(Counts { points: N, counted: N - reassociated })
}

/// O7: the kernel that writes `f(p)` (`values_k`), taken from the build as its WGSL was written
/// (`wrela_driver::gpu_modules`) and interpreted at each of the 64 points with the points
/// buffer the GPU read, gives the GPU's value within O6's bound: the two point intervals'
/// widths, and Metal's reassociation (8 ulps of the value, or of 1 under 1).
fn o7(dir: &Path, build: &Path, seed: u64) -> Result<Counts, Failure> {
    use wrela_ir::interp::{Interp, V, concrete};
    const N: u32 = 64;
    let modules = // A test's build, as the other oracles' are: a shipped build's E0712 isn't this oracle's
    // concern.
    wrela_driver::gpu_modules(dir, wrela_driver::BuildKind { testing: true, ..Default::default() })
        .map_err(|out| Failure {
            what: wrela_diag::render::render_all(&out.sources, &out.diagnostics),
        })?
        .gpu;
    let Some(kernel) = modules.iter().find(|g| g.entries.iter().any(|e| e == "values_k")) else {
        return fail("no pipeline runs `values_k`");
    };
    let m = &kernel.module;
    let mut gpu = Host::load(build).or_else(|e| fail(format!("the GPU host: {e}")))?;
    let at = [Value::I32(seed as u32 as i32)];
    gpu.call_export("on_gpu", &at).or_else(|e| fail(format!("on_gpu: {e}")))?;
    let made = gpu.buffers();
    let [.., points, values, ranges] = made[..] else { return fail("on_gpu made no buffers") };
    let read = |gpu: &mut Host, b| gpu.read_buffer(b).or_else(|e| fail(format!("read: {e}")));
    let points = read(&mut gpu, points)?;
    let values = u32s(&read(&mut gpu, values)?);
    let ranges = u32s(&read(&mut gpu, ranges)?);
    let Some(input) = m.resources.iter().find(|r| r.kind == wrela_ir::ResourceKind::StorageRead)
    else {
        return fail("`values_k` reads no buffer");
    };
    let mut cpu = start(build)?;
    for i in 0..N as usize {
        let mut given = concrete::Given::default();
        given.bytes.insert(input.binding, points.clone());
        given.builtins = vec![
            (wrela_ir::BuiltinInput::GlobalInvocationId, concrete::uvec3(i as u32, 0, 0)),
            (wrela_ir::BuiltinInput::NumWorkgroups, concrete::uvec3(1, 1, 1)),
        ];
        let out = Interp::new(m, &mut given, &mut ())
            .run_entry(0)
            .or_else(|e| fail(format!("the interpreter: {e}")))?;
        let Some(w) = out.writes.iter().find(|w| w.index == i as u32) else {
            return fail(format!("the interpreted kernel wrote nothing at {i}"));
        };
        let iv = match &w.value {
            V::P(c) => concrete::floats_of(&c[3]).first().copied(),
            _ => None,
        }
        .ok_or_else(|| Failure { what: format!("the kernel wrote {:?}", w.value) })?;
        let p = [0, 1, 2].map(|k| f32::from_bits(values[4 * i + k]));
        let gv = f32::from_bits(values[4 * i + 3]);
        let (glo, ghi) = (f32::from_bits(ranges[2 * i]), f32::from_bits(ranges[2 * i + 1]));
        let r = floats(&mut cpu, "range", &ranged(p, p))?;
        let slack = 8.0 * f64::from(f32::EPSILON) * f64::from(gv.abs().max(1.0));
        let room = f64::from(r[1] - r[0]) + f64::from(ghi - glo) + slack;
        let same = iv.to_bits() == gv.to_bits() || (iv.is_nan() && gv.is_nan());
        if !same && !within((f64::from(iv) - f64::from(gv)).abs(), room) {
            return fail(format!(
                "at {p:?}: the interpreter gives {iv:e} and the GPU {gv:e} (apart by more \
                 than {room:e})"
            ));
        }
    }
    Ok(Counts { points: N, counted: N })
}

/// O9: the error walk (`wrela_ir::interp::error`) is sound. The kernel that writes `f(p)`
/// (`values_k`, `f` inlined into it) is taken from the build's GPU module; up to three of its
/// float values, at random, are sites, each with a random bound `δ`; the walk's bound `E` on
/// `f`'s change over a small box of points; then, at 256 points of the box, the kernel
/// interpreted with each site's value moved by a random amount within its `δ` writes an `f`
/// that differs from the unmoved run's by at most `E`, and the two runs' f32 rounding (16 ulps
/// of the larger of 1 and the value: the walk's rules are over the reals). A program the walk
/// refuses (no proof) counts its points as not counted.
fn o9(dir: &Path, seed: u64) -> Result<Counts, Failure> {
    use std::collections::HashMap;
    use wrela_ir::interp::error::{A, R, walk_kernel};
    use wrela_ir::interp::{Interp, Observer, V, concrete};
    use wrela_ir::{Const, FuncId, TypeId, ValueId};
    const N: u32 = 256;
    let modules = // A test's build, as the other oracles' are: a shipped build's E0712 isn't this oracle's
    // concern.
    wrela_driver::gpu_modules(dir, wrela_driver::BuildKind { testing: true, ..Default::default() })
        .map_err(|out| Failure {
            what: wrela_diag::render::render_all(&out.sources, &out.diagnostics),
        })?
        .gpu;
    let Some(kernel) = modules.iter().find(|g| g.entries.iter().any(|e| e == "values_k")) else {
        return fail("no pipeline runs `values_k`");
    };
    let m = &kernel.module;
    let Some(input) =
        m.resources.iter().position(|r| r.kind == wrela_ir::ResourceKind::StorageRead)
    else {
        return fail("`values_k` reads no buffer");
    };
    let binding = m.resources[input].binding;
    let entry = m.entry_points[0].function;
    let mut rng = wrela_grammar::rng::Rng::new(seed ^ 0x09);
    let c = point(&mut rng).map(f64::from);
    let half = 0.05 + 0.2 * rng.unit();
    let mut boxes =
        c.iter().map(|&x| A::F(R { lo: x - half, hi: x + half, err: 0.0 })).collect::<Vec<_>>();
    boxes.push(A::F(R::exact(0.0)));
    let given = |input: usize| {
        let mut r = HashMap::new();
        r.insert(wrela_ir::ResourceId(input as u32), A::P(boxes.clone()));
        r
    };
    let none = HashMap::new();
    let debug = std::env::var_os("O9_DEBUG").is_some();
    let (_, seen) = match walk_kernel(m, entry, given(input), &none) {
        Ok(w) => w,
        Err(e) => {
            if debug {
                eprintln!("refused: {e}");
            }
            refused(&e.to_string());
            return Ok(Counts { points: N, counted: 0 });
        }
    };
    let mut floats: Vec<(FuncId, ValueId)> =
        seen.values.iter().filter(|(_, (a, _))| matches!(a, A::F(_))).map(|(k, _)| *k).collect();
    floats.sort_by_key(|(f, v)| (f.0, v.0));
    let mut sites = HashMap::new();
    for _ in 0..3 {
        if floats.is_empty() {
            break;
        }
        let k = floats.swap_remove(rng.below(floats.len()));
        let size = match &seen.values[&k].0 {
            A::F(r) => r.size(),
            _ => 0.0,
        };
        sites.insert(k, (1e-3 + 1e-2 * rng.unit()) * (1.0 + size.min(1e3)));
    }
    let bound = match walk_kernel(m, entry, given(input), &sites) {
        Ok((Some(A::P(w)), _)) if w.len() == 4 => match &w[3] {
            A::F(r) => r.err,
            _ => return fail("the kernel wrote a value that isn't a float"),
        },
        Ok(other) => return fail(format!("the walk wrote {:?}", other.0)),
        Err(e) => {
            if debug {
                eprintln!("refused with sites: {e}");
            }
            refused(&format!("with its sites: {e}"));
            return Ok(Counts { points: N, counted: 0 });
        }
    };
    /// Moves each site's value by a random amount within its bound.
    struct Move<'s> {
        sites: &'s HashMap<(FuncId, ValueId), f64>,
        rng: wrela_grammar::rng::Rng,
    }
    impl Observer for Move<'_> {
        fn value(&mut self, f: FuncId, v: ValueId, _ty: TypeId, val: &mut V) {
            if let Some(&d) = self.sites.get(&(f, v)) {
                let u = (2.0 * self.rng.unit() - 1.0) * d;
                nudge(val, u);
            }
        }
    }
    fn nudge(v: &mut V, u: f64) {
        match v {
            V::S(Const::F32(x)) => *x = (f64::from(*x) + u) as f32,
            V::P(ps) => ps.iter_mut().for_each(|p| nudge(p, u)),
            _ => {}
        }
    }
    /// The kernel interpreted at point `p` (invocation 0): the `f` it writes.
    fn run<O: Observer>(m: &wrela_ir::Module, binding: u32, p: &[f32], obs: &mut O) -> Option<f32> {
        let mut given = concrete::Given::default();
        let bytes: Vec<u8> = [p[0], p[1], p[2], 0.0].iter().flat_map(|x| x.to_le_bytes()).collect();
        given.bytes.insert(binding, bytes);
        given.builtins = vec![
            (wrela_ir::BuiltinInput::GlobalInvocationId, concrete::uvec3(0, 0, 0)),
            (wrela_ir::BuiltinInput::NumWorkgroups, concrete::uvec3(1, 1, 1)),
        ];
        let out = Interp::new(m, &mut given, obs).run_entry(0).ok()?;
        let w = out.writes.iter().find(|w| w.index == 0)?;
        match &w.value {
            V::P(c) => concrete::floats_of(&c[3]).first().copied(),
            _ => None,
        }
    }
    let mut mover = Move { sites: &sites, rng: wrela_grammar::rng::Rng::new(seed ^ 0x99) };
    let mut counted = 0;
    for _ in 0..N {
        let p: Vec<f32> = c.iter().map(|&x| (x + half * (2.0 * rng.unit() - 1.0)) as f32).collect();
        let (Some(a), Some(b)) = (run(m, binding, &p, &mut ()), run(m, binding, &p, &mut mover))
        else {
            continue;
        };
        if !(a.is_finite() && b.is_finite()) {
            continue;
        }
        counted += 1;
        let slack = 16.0 * f64::from(f32::EPSILON) * f64::from(a.abs().max(b.abs()).max(1.0));
        let moved = (f64::from(b) - f64::from(a)).abs();
        if !within(moved, bound + slack) {
            return fail(format!(
                "at {p:?}: moving {} sites ({sites:?}) moved f by {moved:e}, past the walk's \
                 bound {bound:e}",
                sites.len()
            ));
        }
    }
    Ok(Counts { points: N, counted })
}

/// The seed of program `i` of a run from `base`: what the suite and the soak both use.
pub fn program_seed(base: u64, i: u64) -> u64 {
    wrela_grammar::rng::mix(base, i)
}

/// Runs `oracle` on program `seed` (`mode`), and on a failure shrinks it and writes the shrunk
/// program to `out/<oracle>-<seed>.wrela`: the counts, or a message that names the seed, the
/// file and what failed.
/// What kind of failure `why` is: a diagnostic's code (`E0712`), or the oracle's own message
/// with its numbers, points and value lists taken out. Shrinking keeps a change only while the
/// failure stays of the same kind: a shrunk program that fails some other way is a repro of
/// another failure, worse than none.
pub fn class(why: &str) -> String {
    if let Some(i) = why.find("error[")
        && let Some(j) = why[i..].find(']')
    {
        return why[i..i + j + 1].to_string();
    }
    let mut out = String::new();
    let mut depth = 0;
    let mut prev = ' ';
    for c in why.chars() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth -= 1,
            _ if depth > 0 => {}
            // A digit in a name (`atan2`, `E0712`) stays; a number goes.
            c if c.is_ascii_digit() && prev.is_ascii_alphabetic() => out.push(c),
            c if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' => {}
            c => out.push(c),
        }
        if !(c.is_ascii_digit() && prev.is_ascii_alphabetic()) {
            prev = c;
        }
    }
    let line = out.lines().next().unwrap_or("").to_string();
    // "e" left of a number in scientific notation: gone with the digits around it.
    line.split_whitespace().filter(|w| *w != "e").collect::<Vec<_>>().join(" ")
}

pub fn check_seed(
    oracle: Oracle,
    mode: Mode,
    feat: Features,
    seed: u64,
    scratch: &Path,
    out: &Path,
) -> Result<Counts, String> {
    check_seed_shrinking(oracle, mode, feat, seed, scratch, out, true)
}

/// [`check_seed`], shrinking a failure only if `shrink`; unshrunk, the failure is reported as
/// it is and no file is written.
pub fn check_seed_shrinking(
    oracle: Oracle,
    mode: Mode,
    feat: Features,
    seed: u64,
    scratch: &Path,
    out: &Path,
    shrink: bool,
) -> Result<Counts, String> {
    let (_, data) = super::generate(seed, mode, feat);
    let dir = scratch.join(format!("{}-{mode:?}-{seed:x}", oracle.name()));
    let first = match run(oracle, &text(&data, mode, feat), seed, &dir) {
        Ok(counts) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Ok(counts);
        }
        Err(f) => f,
    };
    let kind = class(&first.what);
    let head = first.what.lines().next().unwrap_or("").to_string();
    if !shrink || ENOUGH.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("{} fails on {mode:?} seed {seed} [{kind}]: {head}", oracle.name()));
    }
    eprintln!("{} fails on {mode:?} seed {seed} [{kind}] before shrinking: {head}", oracle.name());
    let fails = |d: &[u32]| {
        run(oracle, &text(d, mode, feat), seed, &dir).err().is_some_and(|f| class(&f.what) == kind)
    };
    let small = super::shrink(&data, 2000, fails);
    let why = run(oracle, &text(&small, mode, feat), seed, &dir).err().unwrap_or(first);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(out).expect("make the failures' directory");
    let path = out.join(format!("{}-{seed}.wrela", oracle.name()));
    let header = format!("// oracle: {}\n// mode: {mode:?}\n// seed: {seed}\n", oracle.name());
    std::fs::write(&path, header + &text(&small, mode, feat)).expect("write the failure");
    Err(format!("{} fails on {mode:?} seed {seed} ({}): {why}", oracle.name(), path.display()))
}
