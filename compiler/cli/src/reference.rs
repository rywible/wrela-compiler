//! `wrela reference replica <parts> -o <dir> [--engine <dir>] [--harmonics k]
//! [--sections-every m] [--seam m]`: a replica of a reference mesh as a wrela package, a loft per
//! part (its engine dependency the `engine/` above the parts file, or `--engine`); compressed
//! with `--harmonics` and `--sections-every` (`compress`), and every seam `--seam` wide. `wrela reference deviation <parts> <package> [--points n] [--json]`:
//! how far a package's surface is from the mesh's.
//!
//! A parts file (TOML) names its reference (`reference = "<manifest>"`, a mesh's manifest in
//! `references/`, whose mesh is the `.obj` or `.stl` of the same stem; `spacing`, `angles`,
//! `r_max`, `overlap`, `taper` and `seam` at its top are defaults for its parts), its landmarks
//! (points in the creature's frame, metres: `[landmarks]` `"elbow left" = [x, y, z]`) and its
//! parts, in order (`[[part]]`):
//!
//! ```toml
//! [[part]]
//! name = "torso"           # also its bone's name
//! from = "chest"           # the axis: a landmark, or [x, y, z]
//! to = "rump"
//! up = [0.0, 1.0, 0.0]     # angle 0's direction (default +y, or +z for an axis near +y)
//! parent = ""              # the part whose bone is its bone's parent ("" for the root)
//! spacing = 0.01           # between stations, at most (m)
//! angles = 48              # radii per station
//! r_max = 0.25             # the longest radius (m): a ray that runs on is held there
//! seam = "hard"            # or a smooth seam's width, in metres
//! overlap = 0.02           # the axis runs on this far past each end (m), then is trimmed
//! taper = 0.0              # how far it sinks toward a cap inside another part (m): `taper`
//! sections = 0             # a sweep's control sections (0: a loft); see `sweeps`
//! through = []             # a sweep's path's points between `from` and `to` (landmarks)
//! bumps = 4                # a sweep's bumps on each section
//! ends = [0.0, 0.0]        # a sweep's domes, how far past each end (m)
//! ```
//!
//! Each station's radius in each direction is where a ray from the axis leaves the mesh (its
//! largest piece, placed by the manifest's registration: where the winding number falls to 0,
//! so past surfaces inside it where a sculpt crosses itself), at most `r_max`; a station whose
//! point is outside the mesh has none (radii 0). So each part lies inside the mesh, but for
//! interpolation between stations and angles, and where no part reaches shows as a hole.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

use wrela_host::{CpuBuild, CpuHost, Value as HostValue};

use crate::mesh::{self, Registration, Solid, V3};
use crate::sweeps;

pub fn run(args: &[String]) -> ExitCode {
    let result = match args.first().map(String::as_str) {
        Some("replica") => replica_command(&args[1..]),
        Some("deviation") => deviation_command(&args[1..]),
        _ => return crate::usage(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn replica_command(args: &[String]) -> Result<ExitCode, String> {
    let (mut parts, mut out, mut engine) = (None, None, None);
    let (mut harmonics, mut every, mut seam) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--harmonics" => match it.next().and_then(|k| k.parse().ok()) {
                Some(k) => harmonics = Some(k),
                None => return Ok(crate::usage()),
            },
            "--sections-every" => match it.next().and_then(|m| m.parse::<f64>().ok()) {
                Some(m) if m > 0.0 => every = Some(m),
                _ => return Ok(crate::usage()),
            },
            "--seam" => match it.next().and_then(|m| m.parse::<f64>().ok()) {
                Some(m) if m >= 0.0 => seam = Some(m),
                _ => return Ok(crate::usage()),
            },
            "-o" | "--out" => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return Ok(crate::usage()),
            },
            "--engine" => match it.next() {
                Some(e) => engine = Some(PathBuf::from(e)),
                None => return Ok(crate::usage()),
            },
            _ if parts.is_none() && !a.starts_with('-') => parts = Some(PathBuf::from(a)),
            _ => return Ok(crate::unknown(a)),
        }
    }
    let (Some(parts), Some(out)) = (parts, out) else { return Ok(crate::usage()) };
    let mut file = PartsFile::read(&parts)?;
    if let Some(k) = seam {
        // Every part's seam, the parts file's overridden: 0 is hard.
        for p in &mut file.parts {
            p.seam = (k > 0.0).then_some(k);
        }
    }
    let solid = file.solid()?;
    let mut measured = Vec::new();
    for p in &file.parts {
        if p.sections > 0 {
            measured.push(Measure::Sweep(measure_sweep(&solid, p)?));
            continue;
        }
        let (t, start, end) = trimmed(&solid, p)
            .ok_or_else(|| format!("part `{}`: its axis isn't inside the mesh", p.name))?;
        measured.push(Measure::Loft(sample(&solid, &t, (start, end))));
    }
    let mut sampled = fit_sweeps(&file.parts, measured);
    taper(&mut sampled);
    // Compressed: each part's table from a few harmonics at a few sections.
    let mut numbers = Vec::new();
    if harmonics.is_some() || every.is_some() {
        let c =
            Compression { harmonics: harmonics.unwrap_or(usize::MAX), every: every.unwrap_or(0.0) };
        let (compact, floored) = compress(&sampled, c);
        for (s, (t, n)) in sampled.iter_mut().zip(compact) {
            *s = t;
            numbers.push(n);
        }
        println!("compressed: {floored} measured radii held up, where no part reached them");
    }
    let engine = match engine {
        Some(e) => e,
        None => find_engine(file.path.parent().unwrap_or(Path::new(".")))?,
    };
    write_package(&file, &sampled, &out, &engine)?;
    for s in &sampled {
        let t = match &s.shape {
            Shape::Loft(t) => t,
            Shape::Sweep(sw) => {
                let f = &sw.fitted;
                println!(
                    "{}: a sweep through {} points, {} sections of {} bumps, {:.3} m: {:.0}% of its own surface's {} radii within 2 mm, {:.1} mm rms; {} held up",
                    s.part.name,
                    sw.points.len(),
                    f.sections.len(),
                    f.bumps,
                    sweeps::Path::new(&sw.points, s.part.up).length(),
                    100.0 * f.within_2mm,
                    sw.own,
                    f.rms * 1000.0,
                    sw.floored
                );
                continue;
            }
        };
        let (start, end) = t.trim;
        let trim = if start + end > 0.0 {
            format!(
                ", trimmed {:.0} mm at its start and {:.0} at its end (outside the mesh)",
                start * 1e3,
                end * 1e3
            )
        } else {
            String::new()
        };
        println!(
            "{}: {} stations of {} radii, {:.3} m{trim}{}{}",
            s.part.name,
            t.stations,
            s.part.angles,
            mesh::length(mesh::sub(s.part.to, s.part.from)),
            if t.outside.is_empty() {
                String::new()
            } else {
                format!(", stations {:?} of 0 to {} outside the mesh", t.outside, t.stations - 1)
            },
            if t.held > 0 { format!(", {} radii held at r_max", t.held) } else { String::new() },
        );
    }
    if !numbers.is_empty() {
        let radii = |s: &Sampled| s.loft().map_or(0, |t| t.radii.len());
        let dense: usize = sampled.iter().map(radii).sum();
        for (s, n) in sampled.iter().zip(&numbers) {
            println!("{}: {n} numbers (of {})", s.part.name, radii(s));
        }
        println!("compressed: {} numbers, of {dense}", numbers.iter().sum::<usize>());
    }
    println!("wrote {}", out.display());
    Ok(ExitCode::SUCCESS)
}

// ---- compression ------------------------------------------------------------------------------

/// How a table is compressed: `harmonics` around each section (`usize::MAX`: all the table's),
/// and control sections every `every` metres along the axis (0: every station's), smooth
/// between.
#[derive(Clone, Copy, Debug)]
pub struct Compression {
    pub harmonics: usize,
    pub every: f64,
}

/// The parts' tables compressed, each with how many numbers its compact form has, and how many
/// measured radii are held up. Each part is fitted by least squares (`fit_compact`). A part
/// short of its measured surface opened cracks you could see through where it meets its
/// neighbours at a thin form or a fold (150 rays from five views, at 8 harmonics). But held up
/// to every measured radius, a section can't follow a narrow run of long radii (rays that ran
/// along a leg before they left the sculpt) without swinging out elsewhere (35 mm past the
/// sculpt), and the leg holds that run anyway. So a measured radius is held up where no other
/// part reaches the point 0.5 mm inside it, or halfway from it to the next station's or the next
/// angle's: at once where its own part reaches it (so a fit again can't lose it); and where no
/// part does, its part is fitted again. In the first rounds, only the radii its part came within
/// 1 mm of, then 2, 4, 8, then any: so a neighbour that reaches a long run's end is held up
/// before the run's part is. The rounds go on until every point is reached.
pub fn compress(sampled: &[Sampled], c: Compression) -> (Vec<(Sampled, usize)>, usize) {
    let mut floors: Vec<Vec<bool>> =
        sampled.iter().map(|s| vec![false; s.loft().map_or(0, |t| t.radii.len())]).collect();
    let mut out = on_threads(sampled.len(), |k| fit_compact(&sampled[k], c, &[]));
    let mut within = 10;
    for _ in 0..FLOOR_ROUNDS {
        let lofts: Vec<Option<Loft>> = out.iter().map(|(s, _)| Loft::of(s)).collect();
        let mut refit = vec![false; sampled.len()];
        let mut further = false;
        for (k, s) in sampled.iter().enumerate() {
            let (Some(loft), Some(fit)) = (Loft::of(s), &lofts[k]) else { continue };
            let (t, a) = (loft.t, s.part.angles);
            // Each measured radius's point, and the points halfway to the next station's and
            // the next angle's (at the table's radius there): where no other part reaches one,
            // the radii on either side of it are held up, unless their own part reaches it.
            for n in 0..t.radii.len() {
                let (i, j) = (n / a, n % a);
                let mut ends = vec![(n, 0.0, 0.0)];
                if i + 1 < t.stations {
                    ends.push((n + a, 0.5, 0.0));
                }
                ends.push((i * a + (j + 1) % a, 0.0, 0.5));
                for &(m, di, dj) in &ends {
                    let pair = [n, m];
                    if pair.iter().any(|&q| t.held_at[q]) || pair.iter().all(|&q| floors[k][q]) {
                        continue;
                    }
                    let r = 0.5 * f64::from(t.radii[n] + t.radii[m]) - 5.0;
                    if r <= 0.0 {
                        continue;
                    }
                    let p = loft.point(i as f64 + di, j as f64 + dj, r / 10_000.0);
                    let inside = |(l, other): (usize, &Option<Loft>)| {
                        l != k && other.as_ref().is_some_and(|other| other.inside(p))
                    };
                    if lofts.iter().enumerate().any(inside) {
                        continue;
                    }
                    for q in pair {
                        if floors[k][q] {
                            continue;
                        }
                        let short = t.radii[q] - fit.t.radii[q];
                        if short < 5 {
                            floors[k][q] = true;
                        } else if short <= within {
                            floors[k][q] = true;
                            refit[k] = true;
                        } else {
                            further = true;
                        }
                    }
                }
            }
        }
        if !refit.contains(&true) && !further {
            break;
        }
        let again: Vec<usize> = (0..sampled.len()).filter(|&k| refit[k]).collect();
        let fits =
            on_threads(again.len(), |i| fit_compact(&sampled[again[i]], c, &floors[again[i]]));
        for (k, fit) in again.into_iter().zip(fits) {
            out[k] = fit;
        }
        within = if within >= 80 { i32::MAX } else { 2 * within };
    }
    (out, floors.iter().map(|f| f.iter().filter(|&&x| x).count()).sum())
}

/// How many rounds `compress` holds up radii the compressed parts don't reach, at most.
const FLOOR_ROUNDS: usize = 64;

/// `f(i)` for each `i` below `n`, each on a thread of its own: in order, and the same as one
/// after another (the parts' fits don't depend on each other).
fn on_threads<U: Send>(n: usize, f: impl Fn(usize) -> U + Sync) -> Vec<U> {
    let f = &f;
    std::thread::scope(|s| {
        let threads: Vec<_> = (0..n).map(|i| s.spawn(move || f(i))).collect();
        threads
            .into_iter()
            .map(|t| t.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// A loft's table as the engine's loft reads it (engine/loft.wrela), to test points against.
struct Loft<'a> {
    a: V3,
    w: V3,
    u: V3,
    v: V3,
    len: f64,
    angles: usize,
    t: &'a Lofted,
}

impl<'a> Loft<'a> {
    /// `s`'s table, if it's a loft.
    fn of(s: &'a Sampled) -> Option<Loft<'a>> {
        let t = s.loft()?;
        let d = mesh::sub(s.part.to, s.part.from);
        let w = mesh::normalize(d);
        let (u, v, len) = (t.u, mesh::cross(w, t.u), mesh::length(d));
        Some(Loft { a: s.part.from, w, u, v, len, angles: s.part.angles, t })
    }

    /// The point `r` from the axis at station `i` and angle `j` (each may be between two).
    fn point(&self, i: f64, j: f64, r: f64) -> V3 {
        let o = mesh::add(self.a, mesh::scale(self.w, self.len * i / (self.t.stations - 1) as f64));
        let theta = std::f64::consts::TAU * j / self.angles as f64;
        let d = mesh::add(mesh::scale(self.u, theta.cos()), mesh::scale(self.v, theta.sin()));
        mesh::add(o, mesh::scale(d, r))
    }

    /// Whether `p` is inside: between the caps, and nearer the axis than the radius there (the
    /// table's, linear along the axis and around it).
    fn inside(&self, p: V3) -> bool {
        let q = mesh::sub(p, self.a);
        let along = mesh::dot(q, self.w);
        if along < 0.0 || along > self.len {
            return false;
        }
        let (x, y) = (mesh::dot(q, self.u), mesh::dot(q, self.v));
        let rho = (x * x + y * y).sqrt();
        let (stations, angles) = (self.t.stations, self.angles);
        let ts = along / self.len * (stations - 1) as f64;
        let i0 = (ts as usize).min(stations - 2);
        let f = (ts - i0 as f64).clamp(0.0, 1.0);
        let turns = (y.atan2(x) / std::f64::consts::TAU).rem_euclid(1.0);
        let js = turns * angles as f64;
        let j0 = (js as usize).min(angles - 1);
        let g = (js - j0 as f64).clamp(0.0, 1.0);
        let j1 = (j0 + 1) % angles;
        let r = |i: usize, j: usize| f64::from(self.t.radii[i * angles + j]) / 10_000.0;
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        let radius = lerp(lerp(r(i0, j0), r(i0, j1), g), lerp(r(i0 + 1, j0), r(i0 + 1, j1), g), f);
        rho < radius
    }
}

/// A part's table compressed, and how many numbers the compact form has. Each section is a
/// Fourier series of `harmonics` (a₀, then aₖ and bₖ), and each coefficient a Catmull–Rom curve
/// along the axis through control sections: the control sections' coefficients are the compact
/// form, fitted to the table's measured radii by least squares. A held radius (its ray runs into
/// another part) is free, as any value up to `r_max` is inside the sculpt. The radii `floor`
/// flags are pressed on, harder each round, until the fit isn't short of one, and the table,
/// expanded back to the stations and angles, held to [0, `r_max`] and to 0.1 mm, is at least
/// each of them. A sweep is left as it is, and counts no numbers.
pub fn fit_compact(s: &Sampled, c: Compression, floor: &[bool]) -> (Sampled, usize) {
    let Some(table) = s.loft() else { return (s.clone(), 0) };
    let floor = |n: usize| floor.get(n).copied().unwrap_or(false);
    let a = s.part.angles;
    let k = c.harmonics.min((a - 1) / 2);
    let coefficients = 2 * k + 1;
    let tau = std::f64::consts::TAU;
    let around: Vec<Vec<f64>> = (0..a).map(|j| fourier(tau * j as f64 / a as f64, k)).collect();
    let len = mesh::length(mesh::sub(s.part.to, s.part.from));
    let stations = table.stations;
    let controls = if c.every <= 0.0 {
        stations
    } else {
        ((len / c.every).ceil() as usize + 1).clamp(2, stations)
    };
    // Each station's weights on the control sections: its own, or a Catmull–Rom curve's.
    let along: Vec<Vec<(usize, f64)>> = (0..stations)
        .map(|i| {
            if controls == stations {
                return vec![(i, 1.0)];
            }
            let t = i as f64 / (stations - 1) as f64;
            catmull_rom_weights(t, controls)
                .into_iter()
                .enumerate()
                .filter(|w| w.1 != 0.0)
                .collect()
        })
        .collect();
    // Unknown `c * coefficients + q` is control section c's coefficient q; a radius depends on
    // the coefficients of the four control sections around it at most.
    let unknowns = controls * coefficients;
    let band = if controls == stations { coefficients } else { 4 * coefficients };
    let measured = table.held_at.iter().filter(|&&h| !h).count().max(1);
    // A little ridge on the harmonics, so a section measured over a narrow arc doesn't swing
    // wildly where it's free.
    let ridge = 1e-3 * measured as f64 / controls as f64;
    let row = |i: usize, j: usize| -> Vec<(usize, f64)> {
        let mut out = Vec::with_capacity(along[i].len() * coefficients);
        for &(c, w) in &along[i] {
            for (q, t) in around[j].iter().enumerate() {
                out.push((c * coefficients + q, w * t));
            }
        }
        out
    };
    let radii = &table.radii;
    let rows: Vec<Vec<(usize, f64)>> = (0..radii.len()).map(|n| row(n / a, n % a)).collect();
    let value = |x: &[f64], n: usize| -> f64 { rows[n].iter().map(|&(u, w)| w * x[u]).sum() };
    let (mut press, mut hold) = (vec![0.0f64; radii.len()], vec![0.0f64; radii.len()]);
    let mut x = vec![0.0; unknowns];
    for _ in 0..PRESS_ROUNDS {
        let mut m = Banded::new(unknowns, band);
        let mut rhs = vec![0.0; unknowns];
        for (n, r) in rows.iter().enumerate() {
            let w = if table.held_at[n] { 0.0 } else { 1.0 + press[n] + hold[n] };
            if w == 0.0 {
                continue;
            }
            let y = f64::from(radii[n]);
            // A row's unknowns rise, so those up to `u` are the row's first.
            for (e, &(u, wu)) in r.iter().enumerate() {
                rhs[u] += w * wu * y;
                for &(v, wv) in &r[..=e] {
                    m.add(u, v, w * wu * wv);
                }
            }
        }
        for u in 0..unknowns {
            m.add(u, u, if u % coefficients == 0 { 1e-9 } else { ridge.max(1e-9) });
        }
        x = m.solve(rhs);
        // Short of a floor by more than half its last digit: pressed on harder. Past a measured
        // radius by more than `CEILING`: held back harder, but less hard, and only so hard.
        let mut changed = false;
        for n in (0..radii.len()).filter(|&n| floor(n)) {
            if value(&x, n) < f64::from(radii[n]) - 0.5 {
                press[n] = (press[n] * 10.0).max(10.0);
                changed = true;
            }
        }
        for n in (0..radii.len()).filter(|&n| !table.held_at[n]) {
            if value(&x, n) > f64::from(radii[n] + CEILING) && hold[n] < HOLD_MOST {
                hold[n] = (hold[n] * 3.0).clamp(1.0, HOLD_MOST);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let top = s.part.r_max * 10_000.0;
    let fitted = (0..radii.len())
        .map(|n| {
            let r = value(&x, n).clamp(0.0, top).round() as i32;
            if floor(n) { r.max(radii[n]) } else { r }
        })
        .collect();
    let table = Lofted { radii: fitted, ..table.clone() };
    (Sampled { part: s.part.clone(), shape: Shape::Loft(table) }, controls * coefficients)
}

/// The Fourier series' terms at `theta`, to `k` harmonics: 1, then cos hθ and sin hθ for each.
fn fourier(theta: f64, k: usize) -> Vec<f64> {
    let mut t = vec![1.0];
    for h in 1..=k {
        t.push((h as f64 * theta).cos());
        t.push((h as f64 * theta).sin());
    }
    t
}

/// How far past a measured radius `fit_compact` lets its fit go before holding it back (0.1 mm),
/// and how hard it holds it back at most (a floor's press grows without end).
const CEILING: i32 = 30;
const HOLD_MOST: f64 = 1000.0;

/// How many rounds `fit_compact` presses on the floors it falls short of, at most: each presses
/// ten times harder.
const PRESS_ROUNDS: usize = 24;

/// A symmetric matrix that's zero more than `band` from its diagonal, by its lower half, and
/// solved by Cholesky's method in time linear in its size.
pub(crate) struct Banded {
    n: usize,
    band: usize,
    /// Row i's entries from column i − band to i.
    rows: Vec<Vec<f64>>,
}

impl Banded {
    pub(crate) fn new(n: usize, band: usize) -> Banded {
        Banded { n, band, rows: vec![vec![0.0; band + 1]; n] }
    }

    /// Adds `v` at (`i`, `j`), `j` ≤ `i` and within the band.
    pub(crate) fn add(&mut self, i: usize, j: usize, v: f64) {
        self.rows[i][self.band + j - i] += v;
    }

    pub(crate) fn at(&self, i: usize, j: usize) -> f64 {
        self.rows[i][self.band + j - i]
    }

    /// The `x` with M x = `b` (M positive definite).
    pub(crate) fn solve(mut self, mut b: Vec<f64>) -> Vec<f64> {
        let (n, band) = (self.n, self.band);
        // M = L Lᵀ, L in place of M's lower half.
        for i in 0..n {
            for j in i.saturating_sub(band)..=i {
                let mut sum = self.at(i, j);
                for k in i.saturating_sub(band).max(j.saturating_sub(band))..j {
                    sum -= self.at(i, k) * self.at(j, k);
                }
                let v = if i == j { sum.max(1e-300).sqrt() } else { sum / self.at(j, j) };
                self.rows[i][band + j - i] = v;
            }
        }
        for i in 0..n {
            let s: f64 = (i.saturating_sub(band)..i).map(|k| self.at(i, k) * b[k]).sum();
            b[i] = (b[i] - s) / self.at(i, i);
        }
        for i in (0..n).rev() {
            let s: f64 = (i + 1..(i + band + 1).min(n)).map(|k| self.at(k, i) * b[k]).sum();
            b[i] = (b[i] - s) / self.at(i, i);
        }
        b
    }
}

/// The weights of `controls` uniform Catmull–Rom control values at `t` in [0, 1]. Past the first
/// and last, a control is extrapolated (2 p₀ − p₁), so a straight line is reproduced exactly,
/// to its ends.
fn catmull_rom_weights(t: f64, controls: usize) -> Vec<f64> {
    let x = t * (controls - 1) as f64;
    let m = (x.floor() as usize).min(controls - 2);
    let f = x - m as f64;
    let (f2, f3) = (f * f, f * f * f);
    let w = [
        -0.5 * f3 + f2 - 0.5 * f,
        1.5 * f3 - 2.5 * f2 + 1.0,
        -1.5 * f3 + 2.0 * f2 + 0.5 * f,
        0.5 * f3 - 0.5 * f2,
    ];
    let mut out = vec![0.0; controls];
    let last = controls - 1;
    for (d, wd) in w.iter().enumerate() {
        match m as isize + d as isize - 1 {
            -1 => {
                out[0] += 2.0 * wd;
                out[1] -= wd;
            }
            i if i as usize > last => {
                out[last] += 2.0 * wd;
                out[last - 1] -= wd;
            }
            i => out[i as usize] += wd,
        }
    }
    out
}

/// The `x` that minimizes |B x − y|² + `ridge` |x'|² (B's rows `basis`; x' all of x but its
/// first), by its normal equations: a whisker of ridge keeps a control nothing samples at 0
/// rather than undefined.
pub(crate) fn least_squares(basis: &[Vec<f64>], y: &[f64], ridge: f64) -> Vec<f64> {
    let n = basis[0].len();
    let mut m = vec![vec![0.0; n + 1]; n];
    for (row, &yi) in basis.iter().zip(y) {
        for p in 0..n {
            if row[p] == 0.0 {
                continue;
            }
            for q in 0..n {
                m[p][q] += row[p] * row[q];
            }
            m[p][n] += row[p] * yi;
        }
    }
    for (p, r) in m.iter_mut().enumerate() {
        r[p] += if p == 0 { 1e-9 } else { ridge.max(1e-9) };
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..n {
        let pivot =
            (col..n).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs())).unwrap_or(col);
        m.swap(col, pivot);
        let (above, below) = m.split_at_mut(col + 1);
        let top = &above[col];
        for row in below {
            let f = row[col] / top[col];
            if f != 0.0 {
                for (x, t) in row[col..].iter_mut().zip(&top[col..]) {
                    *x -= f * t;
                }
            }
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|c| m[r][c] * x[c]).sum();
        x[r] = (m[r][n] - s) / m[r][r];
    }
    x
}

// ---- the parts file ---------------------------------------------------------------------------

/// A value in the TOML these files use: a string, a number, or a list of them on one line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Str(String),
    Num(f64),
    List(Vec<Value>),
}

/// A table: its header (`""` for the top), whether it's one of an array (`[[name]]`), and its
/// keys in order.
#[derive(Debug)]
pub(crate) struct Table {
    header: String,
    array: bool,
    entries: Vec<(String, Value)>,
}

impl Table {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// The subset of TOML that manifests and parts files use: comments, `[table]`, `[[array]]`,
/// and `key = value` on one line, a key bare or quoted, a value a string, a number or a list.
pub(crate) fn parse_toml(text: &str) -> Result<Vec<Table>, String> {
    let mut tables = vec![Table { header: String::new(), array: false, entries: Vec::new() }];
    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        let at = |e: String| format!("line {}: {e}", n + 1);
        if line.is_empty() {
            continue;
        }
        if let Some(h) = line.strip_prefix("[[") {
            let h =
                h.strip_suffix("]]").ok_or_else(|| at("a `[[table]]` header ends `]]`".into()))?;
            tables.push(Table { header: h.trim().to_string(), array: true, entries: Vec::new() });
            continue;
        }
        if let Some(h) = line.strip_prefix('[') {
            let h = h.strip_suffix(']').ok_or_else(|| at("a `[table]` header ends `]`".into()))?;
            tables.push(Table { header: h.trim().to_string(), array: false, entries: Vec::new() });
            continue;
        }
        let mut chars: Vec<char> = line.chars().collect();
        chars.push('\n');
        let mut i = 0;
        let key = if chars[0] == '"' {
            let (k, next) = read_string(&chars, 0).map_err(at)?;
            i = next;
            k
        } else {
            while chars[i] != '=' && chars[i] != '\n' {
                i += 1;
            }
            line[..i].trim().to_string()
        };
        while chars[i] == ' ' {
            i += 1;
        }
        if chars[i] != '=' {
            return Err(at(format!("`{key}` needs `= value`")));
        }
        let (value, next) = read_value(&chars, i + 1).map_err(at)?;
        if chars[next..].iter().any(|c| !c.is_whitespace()) {
            return Err(at("something follows the value (a list stays on one line)".into()));
        }
        if let Some(t) = tables.last_mut() {
            t.entries.push((key, value));
        }
    }
    Ok(tables)
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            '\\' if quoted => escaped = !escaped,
            '"' if !escaped => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => escaped = false,
        }
        if c != '\\' {
            escaped = false;
        }
    }
    line
}

fn read_string(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut s = String::new();
    let mut i = start + 1;
    loop {
        match chars.get(i) {
            Some('"') => return Ok((s, i + 1)),
            Some('\\') => {
                match chars.get(i + 1) {
                    Some('n') => s.push('\n'),
                    Some(&c) => s.push(c),
                    None => return Err("a string doesn't end".into()),
                }
                i += 2;
            }
            Some('\n') | None => return Err("a string doesn't end".into()),
            Some(&c) => {
                s.push(c);
                i += 1;
            }
        }
    }
}

fn read_value(chars: &[char], start: usize) -> Result<(Value, usize), String> {
    let mut i = start;
    while chars[i] == ' ' {
        i += 1;
    }
    match chars[i] {
        '"' => read_string(chars, i).map(|(s, n)| (Value::Str(s), n)),
        '[' => {
            let mut items = Vec::new();
            i += 1;
            loop {
                while chars[i] == ' ' || chars[i] == ',' {
                    i += 1;
                }
                if chars[i] == ']' {
                    return Ok((Value::List(items), i + 1));
                }
                if chars[i] == '\n' {
                    return Err("a list doesn't end on its line".into());
                }
                let (v, n) = read_value(chars, i)?;
                items.push(v);
                i = n;
            }
        }
        _ => {
            let end = (i..chars.len())
                .find(|&k| matches!(chars[k], ',' | ']' | '\n' | ' '))
                .unwrap_or(chars.len());
            let word: String = chars[i..end].iter().collect();
            word.replace('_', "")
                .parse::<f64>()
                .map(|x| (Value::Num(x), end))
                .map_err(|_| format!("`{word}` isn't a string, a number or a list"))
        }
    }
}

/// A part, read and checked: its axis's ends in metres in the creature's frame.
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub name: String,
    pub from: V3,
    pub to: V3,
    pub up: V3,
    pub parent: Option<usize>,
    pub spacing: f64,
    pub angles: usize,
    pub r_max: f64,
    /// A smooth seam's width; `None` is hard.
    pub seam: Option<f64>,
    /// How far the part sinks toward a cap inside another part (`taper`), in metres.
    pub taper: f64,
    /// How far the axis runs on past each end (then it's trimmed to the mesh): parts that meet
    /// at a joint overlap, so no wedge at a bend between two straight axes is left out of both.
    pub overlap: f64,
    /// A sweep's control sections (engine/sweep.wrela), and bumps on each; 0 sections is a
    /// loft. A sweep's path runs from `from` through `through` to `to`, and its ends' domes
    /// reach `ends` past them.
    pub sections: usize,
    pub bumps: usize,
    pub through: Vec<V3>,
    pub ends: [f64; 2],
}

/// A parts file: its reference and its parts.
pub struct PartsFile {
    pub path: PathBuf,
    pub mesh: PathBuf,
    pub registration: Registration,
    /// The manifest's `attribution`, or its title, author and licence.
    pub attribution: String,
    pub parts: Vec<Part>,
    /// The landmarks, by name.
    pub landmarks: Vec<(String, V3)>,
}

impl PartsFile {
    pub fn read(path: &Path) -> Result<PartsFile, String> {
        let tables = read_toml(path)?;
        let top = &tables[0];
        let dir = path.parent().unwrap_or(Path::new("."));
        let manifest = match top.get("reference") {
            Some(Value::Str(r)) => dir.join(r),
            _ => return Err(format!("{}: give `reference = \"<manifest>\"`", path.display())),
        };
        let (registration, attribution) = read_manifest(&manifest)?;
        let stem = manifest.with_extension("");
        let mesh = ["obj", "stl"]
            .iter()
            .map(|e| stem.with_extension(e))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                format!("no mesh beside {} (a .obj or .stl of the same name)", manifest.display())
            })?;
        let landmarks: Vec<(String, V3)> =
            match tables.iter().find(|t| t.header == "landmarks" && !t.array) {
                Some(t) => t
                    .entries
                    .iter()
                    .map(|(k, v)| {
                        point(v)
                            .map(|p| (k.clone(), p))
                            .ok_or_else(|| format!("landmark `{k}` isn't [x, y, z]"))
                    })
                    .collect::<Result<_, _>>()?,
                None => Vec::new(),
            };
        let resolve = |v: Option<&Value>, what: &str, part: &str| -> Result<V3, String> {
            match v {
                Some(Value::Str(name)) => landmarks
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, p)| *p)
                    .ok_or_else(|| format!("part `{part}`: no landmark `{name}`")),
                Some(v) => point(v)
                    .ok_or_else(|| format!("part `{part}`: `{what}` is a landmark or [x, y, z]")),
                None => Err(format!("part `{part}` needs `{what}`")),
            }
        };
        let mut parts: Vec<Part> = Vec::new();
        for t in tables.iter().filter(|t| t.array && t.header == "part") {
            let name = match t.get("name") {
                Some(Value::Str(s)) if !s.is_empty() => s.clone(),
                _ => return Err(format!("{}: a part needs a `name`", path.display())),
            };
            if parts.iter().any(|p| p.name == name) {
                return Err(format!("two parts are named `{name}`"));
            }
            let from = resolve(t.get("from"), "from", &name)?;
            let to = resolve(t.get("to"), "to", &name)?;
            let w = mesh::sub(to, from);
            if mesh::length(w) < 1e-6 {
                return Err(format!("part `{name}`: its axis has no length"));
            }
            let w = mesh::normalize(w);
            let up = match t.get("up") {
                Some(v) => point(v).ok_or_else(|| format!("part `{name}`: `up` is [x, y, z]"))?,
                None if w[1].abs() > 0.9 => [0.0, 0.0, 1.0],
                None => [0.0, 1.0, 0.0],
            };
            if mesh::length(mesh::cross(up, w)) < 0.1 {
                return Err(format!("part `{name}`: `up` runs along the axis"));
            }
            let parent = match t.get("parent") {
                None => None,
                Some(Value::Str(p)) if p.is_empty() => None,
                Some(Value::Str(p)) => {
                    Some(parts.iter().position(|q| &q.name == p).ok_or_else(|| {
                        format!("part `{name}`: its parent `{p}` isn't a part before it")
                    })?)
                }
                Some(_) => return Err(format!("part `{name}`: `parent` is a part's name")),
            };
            if parent.is_none() && !parts.is_empty() {
                return Err(format!(
                    "part `{name}`: only the first part is the root; give its `parent`"
                ));
            }
            // A key the part doesn't give is the file's (at its top), or the tool's default: a
            // number more than 0, or (`or_zero`) 0 or more.
            let number = |key: &str, default: f64, or_zero: bool| -> Result<f64, String> {
                match t.get(key).or_else(|| top.get(key)) {
                    None => Ok(default),
                    Some(Value::Num(x)) if *x > 0.0 || or_zero && *x == 0.0 => Ok(*x),
                    Some(_) if or_zero => Err(format!("part `{name}`: `{key}` is 0 or more")),
                    Some(_) => Err(format!("part `{name}`: `{key}` is a positive number")),
                }
            };
            let seam = match t.get("seam").or_else(|| top.get("seam")) {
                None => None,
                Some(Value::Str(s)) if s == "hard" => None,
                Some(Value::Num(k)) if *k > 0.0 => Some(*k),
                Some(_) => return Err(format!("part `{name}`: `seam` is \"hard\" or a width")),
            };
            let angles = number("angles", 48.0, false)?;
            if angles < 3.0 || angles.fract() != 0.0 {
                return Err(format!("part `{name}`: `angles` is a whole number, at least 3"));
            }
            let (spacing, r_max) = (number("spacing", 0.01, false)?, number("r_max", 1.0, false)?);
            let (overlap, taper) = (number("overlap", 0.02, true)?, number("taper", 0.0, true)?);
            let count = |key: &str, default: f64, most: f64| -> Result<usize, String> {
                match t.get(key) {
                    None => Ok(default as usize),
                    Some(Value::Num(x)) if *x >= 0.0 && *x <= most && x.fract() == 0.0 => {
                        Ok(*x as usize)
                    }
                    Some(_) => Err(format!("part `{name}`: `{key}` is a whole number to {most}")),
                }
            };
            let sections = count("sections", 0.0, 32.0)?;
            let bumps = count("bumps", 4.0, 4.0)?;
            let through = match t.get("through") {
                None => Vec::new(),
                Some(Value::List(items)) => items
                    .iter()
                    .map(|v| resolve(Some(v), "through", &name))
                    .collect::<Result<Vec<V3>, String>>()?,
                Some(_) => return Err(format!("part `{name}`: `through` is a list of landmarks")),
            };
            if !through.is_empty() && sections == 0 {
                return Err(format!(
                    "part `{name}`: a path `through` points is a sweep's: give `sections`"
                ));
            }
            if through.len() + 2 > sweeps::VERTICES {
                return Err(format!("part `{name}`: a sweep's path has at most 33 points"));
            }
            let ends = match t.get("ends") {
                None => [0.0; 2],
                Some(Value::List(e)) if e.len() == 2 => match (&e[0], &e[1]) {
                    (Value::Num(a), Value::Num(b)) if *a >= 0.0 && *b >= 0.0 => [*a, *b],
                    _ => return Err(format!("part `{name}`: `ends` is [start, end], 0 or more")),
                },
                Some(_) => return Err(format!("part `{name}`: `ends` is [start, end]")),
            };
            parts.push(Part {
                name,
                from,
                to,
                up,
                parent,
                spacing,
                angles: angles as usize,
                r_max,
                seam,
                overlap,
                taper,
                sections,
                bumps,
                through,
                ends,
            });
        }
        if parts.is_empty() {
            return Err(format!("{}: no `[[part]]`", path.display()));
        }
        if parts.len() > 64 {
            return Err("a skeleton has at most 64 bones, so a replica has at most 64 parts".into());
        }
        Ok(PartsFile {
            path: path.to_path_buf(),
            mesh,
            registration,
            attribution,
            parts,
            landmarks,
        })
    }

    /// The mesh's largest piece, placed in the creature's frame.
    pub fn solid(&self) -> Result<Solid, String> {
        let m = mesh::load(&self.mesh)?;
        let pieces = m.pieces();
        let largest = pieces.first().ok_or("the mesh has no triangles")?;
        let mut corners = m.corners(largest);
        for t in &mut corners {
            for p in t.iter_mut() {
                *p = self.registration.place(*p);
            }
        }
        Ok(Solid::new(corners))
    }
}

fn point(v: &Value) -> Option<V3> {
    match v {
        Value::List(items) if items.len() == 3 => {
            let mut p = [0.0; 3];
            for (k, it) in items.iter().enumerate() {
                p[k] = match it {
                    Value::Num(x) => *x,
                    _ => return None,
                };
            }
            Some(p)
        }
        _ => None,
    }
}

/// The tables of the TOML file at `path` (`parse_toml`).
fn read_toml(path: &Path) -> Result<Vec<Table>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    parse_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A manifest's registration (`axes`, `origin`, `metres_per_unit`) and attribution.
fn read_manifest(path: &Path) -> Result<(Registration, String), String> {
    let tables = read_toml(path)?;
    let top = &tables[0];
    let string = |k: &str| match top.get(k) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let axes = Registration::parse_axes(&string("axes").unwrap_or_else(|| "x,y,z".into()))?;
    let origin = top.get("origin").map_or(Some([0.0; 3]), point).ok_or("`origin` is [x, y, z]")?;
    let metres_per_unit = match top.get("metres_per_unit") {
        None => 1.0,
        Some(Value::Num(k)) if *k > 0.0 => *k,
        Some(_) => return Err("`metres_per_unit` is a positive number".into()),
    };
    let attribution = string("attribution").unwrap_or_else(|| {
        format!(
            "\"{}\" by {}, {}",
            string("title").unwrap_or_else(|| "?".into()),
            string("author").unwrap_or_else(|| "?".into()),
            string("licence").unwrap_or_else(|| "?".into())
        )
    });
    let registration = Registration { axes, origin, metres_per_unit };
    if !registration.keeps_handedness() {
        return Err("`axes` mirror the mesh: give axes that keep its handedness".into());
    }
    Ok((registration, attribution))
}

// ---- sampling ---------------------------------------------------------------------------------

/// A part as the replica draws it: a loft's table, or, for a part with `sections`, a sweep.
#[derive(Clone)]
pub struct Sampled {
    pub part: Part,
    pub shape: Shape,
}

/// What a part is drawn as.
#[derive(Clone)]
pub enum Shape {
    Loft(Lofted),
    Sweep(Swept),
}

impl Sampled {
    /// Its table, if it's a loft.
    pub fn loft(&self) -> Option<&Lofted> {
        match &self.shape {
            Shape::Loft(t) => Some(t),
            Shape::Sweep(_) => None,
        }
    }
}

/// A loft's table: its radii, in tenths of a millimetre, station by station.
#[derive(Clone)]
pub struct Lofted {
    /// Angle 0's direction, perpendicular to the axis.
    pub u: V3,
    pub stations: usize,
    pub radii: Vec<i32>,
    /// The stations whose point is outside the mesh, and how many radii are held at `r_max`.
    pub outside: Vec<usize>,
    pub held: usize,
    /// Which radii are held at `r_max` (their rays run on, into another part).
    pub held_at: Vec<bool>,
    /// How much of the run-on axis went at each end (`trimmed`).
    pub trim: (f64, f64),
}

/// A part's sweep: its path's points (trimmed to the mesh), its fitted sections, and how near
/// they came to the measured radii.
#[derive(Clone)]
pub struct Swept {
    pub points: Vec<V3>,
    pub fitted: sweeps::Fitted,
    /// How many measured radii are the part's own surface, and how many were held up.
    pub own: usize,
    pub floored: usize,
}

/// Angle `j` of `angles`'s direction around the axis `w`, from `u` by the right hand: as the
/// engine's loft reads it (cos θ u + sin θ (w × u)).
fn direction(w: V3, u: V3, j: usize, angles: usize) -> V3 {
    let theta = std::f64::consts::TAU * j as f64 / angles as f64;
    mesh::add(mesh::scale(u, theta.cos()), mesh::scale(mesh::cross(w, u), theta.sin()))
}

/// The part with its axis run on by `overlap` past each end, then trimmed to where it's inside
/// the mesh (to 1 mm), so its end stations aren't empty: how much went from each end (of the
/// run-on axis), or `None` if none of it is inside.
pub fn trimmed(solid: &Solid, part: &Part) -> Option<(Part, f64, f64)> {
    let w = mesh::normalize(mesh::sub(part.to, part.from));
    let mut part = part.clone();
    part.from = mesh::sub(part.from, mesh::scale(w, part.overlap));
    part.to = mesh::add(part.to, mesh::scale(w, part.overlap));
    let part = &part;
    let axis = mesh::sub(part.to, part.from);
    let len = mesh::length(axis);
    let steps = (len / 0.001).ceil().max(1.0) as usize;
    let at = |k: usize| mesh::add(part.from, mesh::scale(axis, k as f64 / steps as f64));
    let first = (0..=steps).find(|&k| solid.inside(at(k)))?;
    let last = (first..=steps).rev().find(|&k| solid.inside(at(k)))?;
    if last == first {
        return None;
    }
    let mut p = part.clone();
    p.from = at(first);
    p.to = at(last);
    let step = len / steps as f64;
    Some((p, first as f64 * step, (steps - last) as f64 * step))
}

/// Each part sunk by up to its `taper` over what's left of its axis's run-on (`overlap`) at
/// each end that's inside another part (a joint): from nothing where the run-on starts to all
/// of it at the cap, so the part passes under its neighbour before it ends. A part that ended
/// proud of its neighbour (by a fraction of a millimetre: two lofts' sections of one surface
/// differ that much) left a step, which the lens drew as a line across the form (the torso's
/// front cap, the neck's at the withers). A table's `trim` is how much of its run-on axis went
/// at each end.
pub fn taper(sampled: &mut [Sampled]) {
    let joints: Vec<(bool, bool)> = (0..sampled.len())
        .map(|k| {
            let s = &sampled[k];
            let inside = |p: V3| {
                sampled
                    .iter()
                    .enumerate()
                    .any(|(l, other)| l != k && Loft::of(other).is_some_and(|o| o.inside(p)))
            };
            (inside(s.part.from), inside(s.part.to))
        })
        .collect();
    for (k, s) in sampled.iter_mut().enumerate() {
        let Shape::Loft(t) = &mut s.shape else { continue };
        let len = mesh::length(mesh::sub(s.part.to, s.part.from));
        let run = |trim: f64, joint: bool| if joint { s.part.overlap - trim } else { 0.0 };
        let (l0, l1) = (run(t.trim.0, joints[k].0), run(t.trim.1, joints[k].1));
        let a = s.part.angles;
        for i in 0..t.stations {
            let d = len * i as f64 / (t.stations - 1) as f64;
            let mut sink: f64 = 0.0;
            if l0 > 0.0 && d < l0 {
                sink = sink.max(1.0 - d / l0);
            }
            if l1 > 0.0 && len - d < l1 {
                sink = sink.max(1.0 - (len - d) / l1);
            }
            let by = (sink * s.part.taper * 10_000.0).round() as i32;
            for r in &mut t.radii[i * a..(i + 1) * a] {
                *r = (*r - by).max(0);
            }
        }
    }
}

/// A sweep part as measured: its path's points (from `from` through `through` to `to`, run on
/// by `overlap` and trimmed to the mesh), its path, and its radii, measured in its own planes
/// every `spacing` along it, `angles` round.
pub struct SweepMeasure {
    pub points: Vec<V3>,
    pub path: sweeps::Path,
    pub measured: sweeps::Measured,
}

pub fn measure_sweep(solid: &Solid, p: &Part) -> Result<SweepMeasure, String> {
    let mut points = vec![p.from];
    points.extend(&p.through);
    points.push(p.to);
    let (points, _, _) = sweeps::trimmed(solid, &points, p.up, p.overlap)
        .ok_or_else(|| format!("part `{}`: its path isn't inside the mesh", p.name))?;
    let path = sweeps::Path::new(&points, p.up);
    let measured = sweeps::measure(solid, &path, p.spacing, p.angles, p.r_max);
    Ok(SweepMeasure { points, path, measured })
}

/// A part as measured: a loft's table, sampled; or a sweep's radii, to be fitted with the other
/// sweeps' (`fit_sweeps`).
pub enum Measure {
    Loft(Sampled),
    Sweep(SweepMeasure),
}

/// Each part as the replica draws it: a loft as it was sampled, and each sweep fitted
/// (`sweeps::fit`). Each measured radius is its part's own surface, unless another part (as
/// measured) holds the point 0.5 mm inside it: then it's a ray that ran on along that part,
/// which holds it. Then, as compression does (`compress`), where no fitted part reaches the
/// point 0.5 mm inside a measured radius, that radius is held up and its part fitted again
/// (`sweeps::refit`), until every one is reached. The sweeps are fitted and checked each on a
/// thread of its own.
fn fit_sweeps(parts: &[Part], measured: Vec<Measure>) -> Vec<Sampled> {
    // The sweeps, with their parts' indices.
    let swept: Vec<(usize, &SweepMeasure)> = measured
        .iter()
        .enumerate()
        .filter_map(|(k, m)| match m {
            Measure::Sweep(sm) => Some((k, sm)),
            Measure::Loft(_) => None,
        })
        .collect();
    let point = |sm: &SweepMeasure, n: usize, a: usize, back: f64| {
        let (i, j) = (n / a, n % a);
        let theta = std::f64::consts::TAU * j as f64 / a as f64;
        sm.path.point(sm.measured.s[i], theta, sm.measured.radii[n] - back)
    };
    let kinds: Vec<Vec<sweeps::Kind>> = on_threads(swept.len(), |e| {
        let (k, sm) = swept[e];
        let a = parts[k].angles;
        let mut ks = Vec::with_capacity(sm.measured.radii.len());
        for n in 0..sm.measured.radii.len() {
            if sm.measured.held[n] {
                ks.push(sweeps::Kind::Held);
                continue;
            }
            let r = sm.measured.radii[n];
            let p = point(sm, n, a, 0.0005);
            let shared = r > 0.0005
                && swept
                    .iter()
                    .any(|&(l, o)| l != k && sweeps::inside_measured(&o.path, &o.measured, p));
            ks.push(if shared { sweeps::Kind::Shared } else { sweeps::Kind::Own });
        }
        ks
    });
    let mut fitted: Vec<sweeps::Fitted> = on_threads(swept.len(), |e| {
        let (k, sm) = swept[e];
        sweeps::fit(&sm.measured, &kinds[e], parts[k].sections, parts[k].bumps, parts[k].r_max)
    });
    let mut floors: Vec<Vec<bool>> =
        swept.iter().map(|(_, sm)| vec![false; sm.measured.radii.len()]).collect();
    // Held up first where a part came nearest (short by under 4 mm, then 8, 16, then any), so
    // a part that reaches a place well (a leg, the flank's fold) is held up before one that
    // can't follow it (the torso, which swelled 40 mm past the sculpt there).
    let mut within = 0.004;
    for _ in 0..SWEEP_FLOOR_ROUNDS {
        // Each sweep's radii no fitted sweep reaches: those to hold up now, and whether any
        // is left for a later round.
        let short: Vec<(Vec<usize>, bool)> = on_threads(swept.len(), |e| {
            let (k, sm) = swept[e];
            let a = parts[k].angles;
            let f = &fitted[e];
            let len = sm.path.length();
            let (mut hold, mut further) = (Vec::new(), false);
            for n in 0..sm.measured.radii.len() {
                if kinds[e][n] == sweeps::Kind::Held
                    || floors[e][n]
                    || sm.measured.radii[n] <= sweeps::SLACK + 0.0005
                {
                    continue;
                }
                let p = point(sm, n, a, sweeps::SLACK + 0.0005);
                let reached = swept
                    .iter()
                    .zip(&fitted)
                    .any(|(&(_, o), f)| sweeps::inside_fitted(&o.path, f, p));
                if reached {
                    continue;
                }
                let (i, j) = (n / a, n % a);
                let theta = std::f64::consts::TAU * j as f64 / a as f64;
                let own = sweeps::section_at(&f.sections, &f.places, sm.measured.s[i] / len)
                    .radius(theta);
                if sm.measured.radii[n] - own <= within {
                    hold.push(n);
                } else {
                    further = true;
                }
            }
            (hold, further)
        });
        let further = short.iter().any(|s| s.1);
        let again: Vec<usize> = (0..swept.len()).filter(|&e| !short[e].0.is_empty()).collect();
        if again.is_empty() && !further {
            break;
        }
        for (e, (hold, _)) in short.into_iter().enumerate() {
            for n in hold {
                floors[e][n] = true;
            }
        }
        let refitted = on_threads(again.len(), |i| {
            let e = again[i];
            let (k, sm) = swept[e];
            sweeps::refit(&sm.measured, &kinds[e], &floors[e], &fitted[e], parts[k].r_max)
        });
        for (e, f) in again.into_iter().zip(refitted) {
            fitted[e] = f;
        }
        within = if within >= 0.016 { f64::INFINITY } else { 2.0 * within };
    }
    let mut done = fitted.into_iter().zip(kinds.iter().zip(&floors));
    measured
        .into_iter()
        .zip(parts)
        .map(|(m, p)| {
            let sm = match m {
                Measure::Loft(s) => return s,
                Measure::Sweep(sm) => sm,
            };
            let (fitted, (kinds, floors)) = done.next().expect("a fit for each sweep");
            let mut part = p.clone();
            part.from = sm.points[0];
            part.to = sm.points[sm.points.len() - 1];
            let own = kinds.iter().filter(|&&x| x == sweeps::Kind::Own).count();
            let floored = floors.iter().filter(|&&f| f).count();
            Sampled { part, shape: Shape::Sweep(Swept { points: sm.points, fitted, own, floored }) }
        })
        .collect()
}

/// How many rounds `fit_sweeps` holds up radii the fitted sweeps don't reach, at most.
const SWEEP_FLOOR_ROUNDS: usize = 10;

/// The part's table, sampled from the solid along its axis; `trim` is how much of its run-on
/// axis went at each end (`trimmed`).
pub fn sample(solid: &Solid, part: &Part, trim: (f64, f64)) -> Sampled {
    let axis = mesh::sub(part.to, part.from);
    let len = mesh::length(axis);
    let w = mesh::normalize(axis);
    let u = mesh::normalize(mesh::sub(part.up, mesh::scale(w, mesh::dot(part.up, w))));
    let stations = ((len / part.spacing).ceil() as usize + 1).max(2);
    let mut held_at = Vec::with_capacity(stations * part.angles);
    let (mut radii, mut outside, mut held) =
        (Vec::with_capacity(stations * part.angles), Vec::new(), 0);
    for i in 0..stations {
        let c = mesh::add(part.from, mesh::scale(axis, i as f64 / (stations - 1) as f64));
        if !solid.inside(c) {
            outside.push(i);
            radii.extend(std::iter::repeat_n(0, part.angles));
            held_at.extend(std::iter::repeat_n(false, part.angles));
            continue;
        }
        for j in 0..part.angles {
            let d = direction(w, u, j, part.angles);
            let r = solid.exit(c, d).unwrap_or(0.0);
            let r = if r > part.r_max {
                held += 1;
                part.r_max
            } else {
                r
            };
            held_at.push(r >= part.r_max);
            radii.push((r * 10_000.0).round() as i32);
        }
        let row = i * part.angles..(i + 1) * part.angles;
        fill_held(&mut radii[row.clone()], &held_at[row], part.r_max);
    }
    let table = Lofted { u, stations, radii, outside, held, held_at, trim };
    Sampled { part: part.clone(), shape: Shape::Loft(table) }
}

/// The harmonics a held run is filled from (`fill_held`).
const FILL_HARMONICS: usize = 8;

/// A station's held radii (their rays ran on into another part, so any value up to `r_max` is
/// inside the sculpt) filled from the measured ones around them: a Fourier series of the
/// measured radii, held to [0, `r_max`]. A run held at `r_max` beside a short measured radius
/// is a step in the table, and interpolating across it between stations made slivers past the
/// sculpt (6.5 mm at most on the wolf; 3 mm filled).
fn fill_held(radii: &mut [i32], held: &[bool], r_max: f64) {
    let a = radii.len();
    if held.iter().all(|&h| h) || held.iter().all(|&h| !h) {
        return;
    }
    let k = FILL_HARMONICS.min((a - 1) / 2);
    let terms = |j: usize| fourier(std::f64::consts::TAU * j as f64 / a as f64, k);
    let (mut basis, mut ys) = (Vec::new(), Vec::new());
    for j in (0..a).filter(|&j| !held[j]) {
        basis.push(terms(j));
        ys.push(f64::from(radii[j]));
    }
    let c = least_squares(&basis, &ys, 1e-3 * ys.len() as f64);
    for j in (0..a).filter(|&j| held[j]) {
        let r: f64 = terms(j).iter().zip(&c).map(|(t, c)| t * c).sum();
        radii[j] = r.clamp(0.0, r_max * 10_000.0).round() as i32;
    }
}

// ---- the package ------------------------------------------------------------------------------

/// `text` as `//` comment lines of at most 100 columns (a word longer than a line, such as a
/// link, gets a line of its own).
fn comment(text: &str) -> String {
    let mut out = String::new();
    let mut line = String::from("//");
    for word in text.split_whitespace() {
        if line.len() > 2 && line.len() + 1 + word.len() > 100 {
            out.push_str(&line);
            out.push('\n');
            line = String::from("//");
        }
        line.push(' ');
        line.push_str(word);
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// A number as a wrela `f32` literal, to 0.01 mm.
fn num(x: f64) -> String {
    let s = format!("{:.5}", if x.abs() < 0.000005 { 0.0 } else { x });
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{s}0") } else { s.to_string() }
}

fn vec(p: V3) -> String {
    format!("vec3({}, {}, {})", num(p[0]), num(p[1]), num(p[2]))
}

/// A part's name as an identifier: its words joined by `_`, and a constant's in capitals.
fn ident(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_string();
    if s.starts_with(|c: char| c.is_ascii_digit()) { format!("part_{s}") } else { s }
}

/// The engine package: the nearest `engine/` with a `wrela.toml` above `start`.
fn find_engine(start: &Path) -> Result<PathBuf, String> {
    let mut dir = start.canonicalize().map_err(|e| format!("{}: {e}", start.display()))?;
    loop {
        let e = dir.join("engine");
        if e.join("wrela.toml").is_file() {
            return Ok(e);
        }
        if !dir.pop() {
            return Err(format!("no engine package above {}", start.display()));
        }
    }
}

/// Writes the replica's package to `out`, depending on the engine package at `engine`.
pub fn write_package(
    file: &PartsFile,
    sampled: &[Sampled],
    out: &Path,
    engine: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("can't make {}: {e}", out.display()))?;
    let out_abs = out.canonicalize().map_err(|e| e.to_string())?;
    let engine = engine.canonicalize().map_err(|e| format!("{}: {e}", engine.display()))?;
    let name = ident(out_abs.file_name().and_then(|n| n.to_str()).unwrap_or("replica"));
    let toml = format!(
        "# A replica, written by `wrela reference replica` from {}.\n[package]\nname = \"{name}\"\n\n\
         [dependencies]\nengine = {{ path = \"{}\" }}\n",
        file.path.file_name().and_then(|n| n.to_str()).unwrap_or("its parts file"),
        wrela_driver::lift::relative(&engine, &out_abs)
    );
    let files = [
        ("wrela.toml", toml),
        ("replica.wrela", replica_source(file, sampled)),
        ("subject.wrela", SUBJECT.to_string()),
        ("main.wrela", MAIN.to_string()),
    ];
    for (f, text) in files {
        let formatted = if f.ends_with(".wrela") {
            wrela_driver::on_compiler_thread(|| crate::fmt::format_text(&text))
        } else {
            Some(text.clone())
        };
        // Unformatted where it doesn't parse, so `wrela check` can say why.
        std::fs::write(out.join(f), formatted.as_deref().unwrap_or(&text))
            .map_err(|e| format!("can't write {f}: {e}"))?;
        if formatted.is_none() {
            return Err(format!(
                "the generated {f} doesn't parse (a bug in `wrela reference`): `wrela check {}`",
                out.display()
            ));
        }
    }
    Ok(())
}

const SUBJECT: &str = "// What the lens shows of this package: the replica.

use engine::creature::Creature
use replica::replica
use std::field::{Color, Field, Lipschitz}

pub fn subject() -> Field<Color> + Lipschitz {
    replica().field()
}
";

const MAIN: &str =
    "// The replica for tools: `wrela reference deviation` measures it through these.

use engine::creature::{Creature, CreatureField}
use replica::{Parts, replica}
use std::field::Surface

pub fn init() -> CreatureField<Parts> {
    replica().field()
}

/// The replica's distance at a point.
pub fn distance(state: CreatureField<Parts>, x: f32, y: f32, z: f32) -> f32 {
    state.distance(vec3(x, y, z))
}

/// The part nearest a point: its index, in the parts file's order.
pub fn part(state: CreatureField<Parts>, x: f32, y: f32, z: f32) -> u32 {
    state.part_at(vec3(x, y, z)).1
}

pub fn frame(state: mut CreatureField<Parts>, time: f32, width: u32, height: u32) {}
";

fn replica_source(file: &PartsFile, sampled: &[Sampled]) -> String {
    let mut g = String::new();
    let mesh_name = file.mesh.file_name().and_then(|n| n.to_str()).unwrap_or("the mesh");
    let parts_name = file.path.file_name().and_then(|n| n.to_str()).unwrap_or("its parts file");
    g.push_str(&comment(&format!(
        "A replica of {mesh_name}, written by `wrela reference replica` from {parts_name}. \
         Don't edit it: refactor a copy."
    )));
    g.push_str("//\n");
    let is_sweep = |s: &Sampled| matches!(s.shape, Shape::Sweep(_));
    let how = if sampled.iter().all(is_sweep) {
        "its surface fitted by sweeps. Each part's sections are fitted to radii measured from \
         its path, where a ray leaves the sculpt (where its winding number falls to 0)"
    } else {
        "its surface sampled into lofts. Each part's radius at each station and angle is where \
         a ray from the part's axis leaves the sculpt (where its winding number falls to 0), in \
         tenths of a millimetre"
    };
    g.push_str(&comment(&format!("Adapted from {}: {how}.", file.attribution)));
    g.push('\n');
    let smooth = sampled.iter().any(|s| s.part.seam.is_some());
    let hard = sampled.iter().any(|s| s.part.seam.is_none());
    let mut seams = Vec::new();
    if hard {
        seams.push("hard");
    }
    if smooth {
        seams.push("smooth");
    }
    let lofts = sampled.iter().any(|s| s.loft().is_some());
    let swept = sampled.iter().any(is_sweep);
    let _ = write!(
        g,
        "use engine::creature::{{Body, Placed, Skeleton, Then, creature, {}}}\n{}{}\
         use std::field::{{Color, Surface, With, rgb}}\n\n",
        seams.join(", "),
        if lofts { "use engine::loft::{Loft, loft}\n" } else { "" },
        if swept { "use engine::sweep::{Sweep, section, sweep}\n" } else { "" },
    );
    // The parts' type.
    let leaf = |s: &Sampled| match &s.shape {
        Shape::Sweep(sw) => format!("Placed<With<Sweep<{}>, Color>>", sw.fitted.sections.len()),
        Shape::Loft(t) => format!("Placed<With<Loft<{}>, Color>>", t.radii.len()),
    };
    let mut ty = leaf(&sampled[0]);
    for s in &sampled[1..] {
        ty = format!("Then<{ty}, {}>", leaf(s));
    }
    let _ =
        write!(g, "/// The replica's parts, in the parts file's order.\npub type Parts = {ty}\n\n");
    g.push_str("/// Clay.\nfn clay() -> Color {\n    rgb(r: 0.74, g: 0.72, b: 0.68)\n}\n\n");
    if lofts {
        g.push_str(
            "/// A table of radii in tenths of a millimetre, in metres.\n\
             fn metres<const N: u32>(tenths: [i32; N]) -> [f32; N] {\n    var out = [0.0; N]\n    \
             for i in 0..N {\n        out[i] = f32(tenths[i]) * 0.0001\n    }\n    out\n}\n\n",
        );
    }
    // The skeleton: a bone per part, at the start of its axis.
    g.push_str("/// The skeleton: a bone per part, at the start of the part's axis.\npub fn skeleton() -> Skeleton {\n    var s = Skeleton::new()\n");
    for (i, s) in sampled.iter().enumerate() {
        // A bone is named for the parts under it; the rest are found by name (`find`).
        let id = ident(&s.part.name);
        let bind = if sampled.iter().any(|t| t.part.parent == Some(i)) {
            format!("let {id} = ")
        } else {
            String::new()
        };
        match s.part.parent {
            None => {
                let _ = writeln!(g, "    {bind}s.root(\"{}\", {})", s.part.name, vec(s.part.from));
            }
            Some(p) => {
                let _ = writeln!(
                    g,
                    "    {bind}s.joint(\"{}\", {}, {})",
                    s.part.name,
                    ident(&sampled[p].part.name),
                    vec(s.part.from)
                );
            }
        }
    }
    g.push_str("    s\n}\n\n");
    // The whole.
    g.push_str("/// The replica: each part on its bone, in the parts file's order.\npub fn replica() -> Body<Parts> {\n    let s = skeleton()\n    creature(s)\n");
    for s in sampled {
        let seam = match s.part.seam {
            None => "hard()".to_string(),
            Some(k) => format!("smooth({})", num(k)),
        };
        let _ = writeln!(
            g,
            "        .join(\"{}\", s.find(\"{}\"), {}().with(clay()), seam: {seam})",
            s.part.name,
            s.part.name,
            ident(&s.part.name)
        );
    }
    g.push_str("}\n\n");
    // Each part.
    for s in sampled {
        let id = ident(&s.part.name);
        let t = match &s.shape {
            Shape::Loft(t) => t,
            Shape::Sweep(sw) => {
                g.push_str(&sweep_source(&id, s, sw, &file.landmarks));
                continue;
            }
        };
        let n = t.radii.len();
        let _ = write!(
            g,
            "/// {}: {} stations of {} radii, from {} to {}.\npub fn {id}() -> Loft<{n}> {{\n    \
             loft(\n        vec3(),\n        {},\n        {},\n        stations: {},\n        angles: {},\n        radii: metres({}),\n    )\n}}\n\n",
            s.part.name,
            t.stations,
            s.part.angles,
            vec(s.part.from),
            vec(s.part.to),
            vec(mesh::sub(s.part.to, s.part.from)),
            vec(t.u),
            t.stations,
            s.part.angles,
            id.to_uppercase(),
        );
        let _ = writeln!(g, "const {}: [i32; {n}] = [", id.to_uppercase());
        for row in t.radii.chunks(s.part.angles) {
            let words: Vec<String> = row.iter().map(|r| r.to_string()).collect();
            let _ = writeln!(g, "    {},", words.join(", "));
        }
        g.push_str("]\n\n");
    }
    g
}

/// A sweep part's function: its path's points (from its bone, at its first), its up (the
/// part's), and its fitted sections (every bump on every section, so they stay matched by
/// place).
fn sweep_source(id: &str, s: &Sampled, sw: &Swept, landmarks: &[(String, V3)]) -> String {
    let f = &sw.fitted;
    let mut g = String::new();
    let _ = writeln!(
        g,
        "/// {}: a sweep through {} points, {} sections (within 2 mm of {:.0}% of the measured radii; {:.1} mm rms).",
        s.part.name,
        sw.points.len(),
        f.sections.len(),
        100.0 * f.within_2mm,
        f.rms * 1000.0
    );
    let _ = writeln!(g, "pub fn {id}() -> Sweep<{}> {{\n    sweep(\n        [", f.sections.len());
    for p in &sw.points {
        let _ = writeln!(g, "            {},", vec(mesh::sub(*p, sw.points[0])));
    }
    let _ = writeln!(g, "        ],\n        up: {},\n        sections: [", vec(s.part.up));
    let mm = |x: f64| num((x * 10_000.0).round() / 10_000.0);
    let rad = |x: f64| num((x * 1000.0).round() / 1000.0);
    let path = sweeps::Path::new(&sw.points, s.part.up);
    for (sec, &place) in f.sections.iter().zip(&f.places) {
        // Where it is: how far along, and the landmark it's at (within 4 cm), if any.
        let along = place * path.length();
        let at = path.plane(along).0;
        let near = landmarks
            .iter()
            .map(|(n, p)| (n, mesh::length(mesh::sub(*p, at))))
            .filter(|(_, d)| *d < 0.04)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let _ = match near {
            Some((n, _)) => writeln!(g, "            // {:.0} cm along: at \"{n}\"", along * 100.0),
            None => writeln!(g, "            // {:.0} cm along", along * 100.0),
        };
        let _ = write!(
            g,
            "            section(up: {}, down: {}, left: {}, right: {}, square: {}, turn: {})",
            mm(sec.up),
            mm(sec.down),
            mm(sec.left),
            mm(sec.right),
            num((sec.square * 100.0).round() / 100.0),
            rad(sec.turn)
        );
        for b in sec.bumps.iter().take(s.part.bumps) {
            if b[2] > 0.0 {
                let _ = write!(
                    g,
                    "\n                .bump(at: {}, height: {}, width: {})",
                    rad(b[0]),
                    mm(b[1]),
                    rad(b[2])
                );
            }
        }
        g.push_str(",\n");
    }
    let places: Vec<String> =
        f.places.iter().map(|&p| num((p * 10_000.0).round() / 10_000.0)).collect();
    let _ = write!(
        g,
        "        ],\n        places: [{}],\n        ends: vec2({}, {}),\n    )\n}}\n\n",
        places.join(", "),
        num(s.part.ends[0]),
        num(s.part.ends[1])
    );
    g
}

// ---- deviation --------------------------------------------------------------------------------

fn deviation_command(args: &[String]) -> Result<ExitCode, String> {
    let (mut parts, mut package, mut points, mut json) = (None, None, 20_000usize, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--points" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => points = n,
                None => return Ok(crate::usage()),
            },
            "--json" => json = true,
            _ if parts.is_none() && !a.starts_with('-') => parts = Some(PathBuf::from(a)),
            _ if package.is_none() && !a.starts_with('-') => package = Some(PathBuf::from(a)),
            _ => return Ok(crate::unknown(a)),
        }
    }
    let (Some(parts), Some(package)) = (parts, package) else { return Ok(crate::usage()) };
    let file = PartsFile::read(&parts)?;
    let solid = file.solid()?;
    let report = deviation(&file, &solid, &package, points)?;
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_text());
    }
    Ok(ExitCode::SUCCESS)
}

/// The replica, built and running on the CPU: its build, and instances of it to ask (the
/// first for one question at a time, and one for each thread `each` asks on).
pub struct Replica {
    build: CpuBuild,
    probes: Vec<Probe>,
}

impl Replica {
    pub fn load(package: &Path) -> Result<Replica, String> {
        if !package.join("main.wrela").is_file() {
            return Err(format!("{} isn't a package (it has no main.wrela)", package.display()));
        }
        let out = wrela_driver::build(package);
        if out.has_errors() {
            return Err(wrela_diag::render::render_all(&out.sources, &out.diagnostics));
        }
        let built = package.join("build").join("cpu");
        out.write_to(&built).map_err(|e| e.to_string())?;
        let build = CpuBuild::load(&built).map_err(cant_run)?;
        let probe = Probe(build.start().map_err(cant_run)?);
        Ok(Replica { build, probes: vec![probe] })
    }

    pub fn part(&mut self, p: V3) -> Result<usize, String> {
        self.probes[0].part(p)
    }

    /// `f` of each of `items`, in order: asked on the machine's threads, each with an instance
    /// of its own (the answers are the same as one instance's), taking small pieces of the
    /// items as it's free (some rays take many more questions than others).
    fn each<T: Sync, U: Send>(
        &mut self,
        items: &[T],
        f: impl Fn(&mut Probe, &T) -> Result<U, String> + Sync,
    ) -> Result<Vec<U>, String> {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        while self.probes.len() < threads {
            self.probes.push(Probe(self.build.start_with(1).map_err(cant_run)?));
        }
        let pieces: Vec<&[T]> = items.chunks(items.len().div_ceil(8 * threads).max(1)).collect();
        let next = AtomicUsize::new(0);
        let (pieces, next, f) = (&pieces, &next, &f);
        let mut answers: Vec<(usize, Result<Vec<U>, String>)> = std::thread::scope(|s| {
            let threads: Vec<_> = self
                .probes
                .iter_mut()
                .map(|probe| {
                    s.spawn(move || {
                        let mut answers = Vec::new();
                        loop {
                            let k = next.fetch_add(1, Ordering::Relaxed);
                            let Some(piece) = pieces.get(k) else { break answers };
                            answers.push((k, piece.iter().map(|item| f(probe, item)).collect()));
                        }
                    })
                })
                .collect();
            threads
                .into_iter()
                .flat_map(|t| t.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect()
        });
        answers.sort_by_key(|a| a.0);
        let mut out = Vec::with_capacity(items.len());
        for (_, piece) in answers {
            out.extend(piece?);
        }
        Ok(out)
    }
}

fn cant_run(e: wrela_host::Error) -> String {
    format!("can't run the replica: {e}")
}

/// An instance of the replica, to ask.
struct Probe(CpuHost);

impl Probe {
    fn distance(&mut self, p: V3) -> Result<f64, String> {
        match self.0.call_export("distance", &args3(p)).map_err(|e| e.to_string())?.as_slice() {
            [HostValue::F32(d)] => Ok(f64::from(*d)),
            other => Err(format!("`distance` gave {other:?}")),
        }
    }

    fn part(&mut self, p: V3) -> Result<usize, String> {
        match self.0.call_export("part", &args3(p)).map_err(|e| e.to_string())?.as_slice() {
            [HostValue::I32(i)] => Ok(*i as usize),
            other => Err(format!("`part` gave {other:?}")),
        }
    }
}

fn args3(p: V3) -> [HostValue; 3] {
    [HostValue::F32(p[0] as f32), HostValue::F32(p[1] as f32), HostValue::F32(p[2] as f32)]
}

/// A small, fixed random sequence (xorshift), so a report is the same each run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How a replica differs from its mesh.
pub struct Deviation {
    pub names: Vec<String>,
    /// At points spread evenly over the mesh's surface: how far the replica is from it (more
    /// than 0 where it's short of the mesh: the replica's distance there; less where it's past
    /// it: how far it reaches beyond the mesh outward from there), and the part nearest.
    pub surface: Vec<(V3, f64, usize)>,
    /// Points skipped because their surface is inside the mesh (where it crosses itself).
    pub hidden: usize,
    /// At points spread evenly through the mesh's box: inside both, the mesh alone, the replica
    /// alone, and the box's volume (m³), with how many points.
    pub both: usize,
    pub mesh_only: usize,
    pub replica_only: usize,
    pub box_volume: f64,
    pub volume_points: usize,
    /// How far past the mesh the replica reaches, at most (m).
    pub furthest_past: f64,
    /// Rays from each of `VIEWS`, one every `RAY_CELL`, that pass through the mesh but meet
    /// nothing of the replica: where you'd see through it.
    pub see_through: Vec<SeeThrough>,
    /// How many rays passed through the mesh, by view.
    pub rays: [usize; 5],
}

/// The spacing of the see-through rays.
const RAY_CELL: f64 = 0.002;

pub fn deviation(
    file: &PartsFile,
    solid: &Solid,
    package: &Path,
    points: usize,
) -> Result<Deviation, String> {
    let mut replica = Replica::load(package)?;
    let names: Vec<String> = file.parts.iter().map(|p| p.name.clone()).collect();
    // Surface points: triangles chosen by area, then a point in each.
    let areas: Vec<f64> = solid
        .triangles
        .iter()
        .map(|t| 0.5 * mesh::length(mesh::cross(mesh::sub(t[1], t[0]), mesh::sub(t[2], t[0]))))
        .collect();
    let total: f64 = areas.iter().sum();
    let mut cumulative = Vec::with_capacity(areas.len());
    let mut acc = 0.0;
    for a in &areas {
        acc += a;
        cumulative.push(acc);
    }
    // All drawn first, then the replica asked about them on many threads.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut drawn = Vec::with_capacity(points);
    let mut hidden = 0;
    while drawn.len() < points {
        let x = rng.next() * total;
        let k = cumulative.partition_point(|&c| c < x).min(solid.triangles.len() - 1);
        let t = solid.triangles[k];
        let (mut a, mut b) = (rng.next(), rng.next());
        if a + b > 1.0 {
            a = 1.0 - a;
            b = 1.0 - b;
        }
        let p = mesh::add(
            t[0],
            mesh::add(mesh::scale(mesh::sub(t[1], t[0]), a), mesh::scale(mesh::sub(t[2], t[0]), b)),
        );
        // A surface inside the solid (where a sculpt crosses itself) can't be seen: skip it.
        let n = mesh::normalize(mesh::cross(mesh::sub(t[1], t[0]), mesh::sub(t[2], t[0])));
        if solid.inside(mesh::add(p, mesh::scale(n, 0.001))) {
            hidden += 1;
            if hidden > 10 * points {
                return Err("the mesh's surface is all inside it".into());
            }
            continue;
        }
        drawn.push((p, n));
    }
    let surface = replica.each(&drawn, |probe, &(p, n)| {
        let d = probe.distance(p)?;
        // Short of the mesh: the replica's distance. Past it: probe outward along the normal,
        // and take the mesh's distance at the furthest probe still inside the replica (and
        // outside the mesh: a probe that reaches another part of the mesh stops).
        let mut e = d.max(0.0);
        if d <= 0.0 {
            for step in [0.002, 0.005, 0.01, 0.02, 0.04] {
                let q = mesh::add(p, mesh::scale(n, step));
                if solid.inside(q) || probe.distance(q)? >= 0.0 {
                    break;
                }
                e = -solid.nearest(q);
            }
        }
        Ok((p, e, probe.part(p)?))
    })?;
    // Volume points: inside the mesh, inside the replica, and, inside the replica alone, how
    // far past the mesh.
    let (lo, hi) = solid.bounds();
    let pad = 0.02;
    let lo = mesh::sub(lo, [pad; 3]);
    let hi = mesh::add(hi, [pad; 3]);
    let size = mesh::sub(hi, lo);
    let drawn: Vec<V3> = (0..points)
        .map(|_| {
            [
                lo[0] + rng.next() * size[0],
                lo[1] + rng.next() * size[1],
                lo[2] + rng.next() * size[2],
            ]
        })
        .collect();
    let found = replica.each(&drawn, |probe, &p| {
        let (in_mesh, in_replica) = (solid.inside(p), probe.distance(p)? < 0.0);
        Ok((in_mesh, in_replica, if in_replica && !in_mesh { solid.nearest(p) } else { 0.0 }))
    })?;
    let (mut both, mut mesh_only, mut replica_only, mut furthest_past) = (0, 0, 0, 0.0f64);
    for (in_mesh, in_replica, past) in found {
        match (in_mesh, in_replica) {
            (true, true) => both += 1,
            (true, false) => mesh_only += 1,
            (false, true) => {
                replica_only += 1;
                furthest_past = furthest_past.max(past);
            }
            (false, false) => {}
        }
    }
    let (see_through, rays) = see_through(solid, &mut replica)?;
    Ok(Deviation {
        names,
        surface,
        hidden,
        both,
        mesh_only,
        replica_only,
        box_volume: size[0] * size[1] * size[2],
        volume_points: points,
        furthest_past,
        see_through,
        rays,
    })
}

/// The views the see-through rays look from: the sides, the top, the front, and the lens's
/// three-quarter view and its mirror image.
pub const VIEWS: [(&str, V3); 5] = [
    ("side", [1.0, 0.0, 0.0]),
    ("top", [0.0, 1.0, 0.0]),
    ("front", [0.0, 0.0, 1.0]),
    ("three-quarter", [0.62, 0.45, 0.62]),
    ("three-quarter, mirrored", [-0.62, 0.45, 0.62]),
];

/// For each view, a grid of parallel rays `RAY_CELL` apart over the mesh's box, each one that
/// passes through the mesh tested for the replica: along each stretch inside the mesh, stepping
/// by a quarter of the replica's distance (at least 0.5 mm), until it's inside the replica. A ray
/// whose every stretch ends with no point inside sees through the replica: inside the outline
/// if every ray within two cells of it passes through the mesh too, or at its rim.
fn see_through(
    solid: &Solid,
    replica: &mut Replica,
) -> Result<(Vec<SeeThrough>, [usize; 5]), String> {
    let (lo, hi) = solid.bounds();
    let corners: Vec<V3> = (0..8)
        .map(|k| {
            [
                if k & 1 == 0 { lo[0] } else { hi[0] },
                if k & 2 == 0 { lo[1] } else { hi[1] },
                if k & 4 == 0 { lo[2] } else { hi[2] },
            ]
        })
        .collect();
    let mut out = Vec::new();
    let mut rays = [0; 5];
    for (view, (_, towards)) in VIEWS.iter().enumerate() {
        // The rays run against the view's direction, from the viewer's side.
        let d = mesh::scale(mesh::normalize(*towards), -1.0);
        let helper = if d[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let u = mesh::normalize(mesh::cross(helper, d));
        let v = mesh::cross(d, u);
        let range = |axis: V3| {
            let ps: Vec<f64> = corners.iter().map(|c| mesh::dot(*c, axis)).collect();
            (
                ps.iter().copied().fold(f64::MAX, f64::min),
                ps.iter().copied().fold(f64::MIN, f64::max),
            )
        };
        let ((u0, u1), (v0, v1), (d0, _)) = (range(u), range(v), range(d));
        let (nu, nv) = (((u1 - u0) / RAY_CELL) as usize + 1, ((v1 - v0) / RAY_CELL) as usize + 1);
        // Each ray, row by row: 0 where it misses the mesh, 1 where it passes through the mesh
        // and the replica, 2 through the mesh alone; and where it enters the mesh.
        let rows: Vec<usize> = (0..nu).collect();
        let found = replica.each(&rows, |probe, &i| {
            (0..nv)
                .map(|j| {
                    let o = mesh::add(
                        mesh::add(
                            mesh::scale(u, u0 + (i as f64 + 0.5) * RAY_CELL),
                            mesh::scale(v, v0 + (j as f64 + 0.5) * RAY_CELL),
                        ),
                        mesh::scale(d, d0 - 0.01),
                    );
                    // The stretches inside: where the winding number is at least 1.
                    let mut w = 0;
                    let mut start = None;
                    let mut stretches = Vec::new();
                    for (t, sign) in solid.crossings(o, d) {
                        let was = w;
                        w -= sign;
                        if was < 1 && w >= 1 {
                            start = Some(t);
                        } else if was >= 1
                            && w < 1
                            && let Some(s) = start.take()
                        {
                            stretches.push((s, t));
                        }
                    }
                    if stretches.is_empty() {
                        return Ok((0u8, [0.0; 3]));
                    }
                    let mut covered = false;
                    'stretch: for &(s, e) in &stretches {
                        let mut t = s + 0.0003;
                        while t < e {
                            let dist = probe.distance(mesh::add(o, mesh::scale(d, t)))?;
                            if dist < 0.0 {
                                covered = true;
                                break 'stretch;
                            }
                            t += (dist / 4.0).max(0.0005);
                        }
                    }
                    let entry = mesh::add(o, mesh::scale(d, stretches[0].0));
                    Ok((if covered { 1 } else { 2 }, entry))
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        let (state, entries): (Vec<u8>, Vec<V3>) = found.into_iter().flatten().unzip();
        rays[view] = state.iter().filter(|&&s| s != 0).count();
        for i in 0..nu {
            for j in 0..nv {
                if state[i * nv + j] != 2 {
                    continue;
                }
                let inside = (i.saturating_sub(2)..(i + 3).min(nu)).all(|a| {
                    (j.saturating_sub(2)..(j + 3).min(nv)).all(|b| state[a * nv + b] != 0)
                }) && i >= 2
                    && j >= 2
                    && i + 2 < nu
                    && j + 2 < nv;
                let at = entries[i * nv + j];
                out.push(SeeThrough { view, at, part: replica.part(at)?, inside });
            }
        }
    }
    Ok((out, rays))
}

/// A ray that passes through the mesh but meets nothing of the replica (`see_through`).
#[derive(Clone, Copy, Debug)]
pub struct SeeThrough {
    /// Its view, in `VIEWS`.
    pub view: usize,
    /// Where it enters the mesh.
    pub at: V3,
    /// The part nearest there.
    pub part: usize,
    /// Inside the mesh's outline (every ray within two cells passes through the mesh), not at
    /// its rim, where a ray grazes the mesh for a millimetre or two.
    pub inside: bool,
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// How far surface points are from the mesh (m): how many, the mean, median, 90th percentile
/// and worst of their distances, and the shares within 2 mm and 5 mm.
struct Stats {
    n: usize,
    mean: f64,
    median: f64,
    p90: f64,
    worst: f64,
    within_2mm: f64,
    within_5mm: f64,
}

impl Deviation {
    /// The statistics of the surface points nearest part `part`, or of all.
    fn stats(&self, part: Option<usize>) -> Stats {
        let mut abs: Vec<f64> = self
            .surface
            .iter()
            .filter(|p| part.is_none_or(|i| p.2 == i))
            .map(|p| p.1.abs())
            .collect();
        abs.sort_by(f64::total_cmp);
        let n = abs.len();
        let share = |tolerance: f64| {
            if n == 0 {
                0.0
            } else {
                abs.iter().filter(|&&d| d <= tolerance).count() as f64 / n as f64
            }
        };
        Stats {
            n,
            mean: if n == 0 { 0.0 } else { abs.iter().sum::<f64>() / n as f64 },
            median: percentile(&abs, 0.5),
            p90: percentile(&abs, 0.9),
            worst: abs.last().copied().unwrap_or(0.0),
            within_2mm: share(0.002),
            within_5mm: share(0.005),
        }
    }

    fn cm3(&self, count: usize) -> f64 {
        self.box_volume * count as f64 / self.volume_points as f64 * 1e6
    }

    pub fn iou(&self) -> f64 {
        let union = self.both + self.mesh_only + self.replica_only;
        if union == 0 { 0.0 } else { self.both as f64 / union as f64 }
    }

    /// The `n` worst places at least 2 cm apart, of part `part` or of all.
    fn worst(&self, n: usize, part: Option<usize>) -> Vec<&(V3, f64, usize)> {
        let mut all: Vec<&(V3, f64, usize)> =
            self.surface.iter().filter(|p| part.is_none_or(|i| p.2 == i)).collect();
        all.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        // At least 2 cm apart.
        let mut out: Vec<&(V3, f64, usize)> = Vec::new();
        for p in all {
            if out.iter().all(|q| mesh::length(mesh::sub(q.0, p.0)) > 0.02) {
                out.push(p);
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let all = self.stats(None);
        let _ = writeln!(
            s,
            "surface, {} points ({} more skipped, inside the mesh where it crosses itself): mean {:.1} mm, median {:.1}, 90% {:.1}, worst {:.1}; {:.0}% within 2 mm, {:.0}% within 5 mm",
            all.n,
            self.hidden,
            all.mean * 1e3,
            all.median * 1e3,
            all.p90 * 1e3,
            all.worst * 1e3,
            all.within_2mm * 100.0,
            all.within_5mm * 100.0
        );
        let _ = writeln!(
            s,
            "volume: {:.0}% shared (intersection over union); the mesh's alone {:.0} cm³, the replica's alone {:.0} cm³, at most {:.1} mm past the mesh",
            self.iou() * 100.0,
            self.cm3(self.mesh_only),
            self.cm3(self.replica_only),
            self.furthest_past * 1e3
        );
        let inside: Vec<&SeeThrough> = self.see_through.iter().filter(|r| r.inside).collect();
        let _ = writeln!(
            s,
            "see-through, rays every {} mm from {} views: {} inside the outline ({}), {} more at its rim",
            RAY_CELL * 1e3,
            VIEWS.len(),
            inside.len(),
            VIEWS
                .iter()
                .enumerate()
                .map(|(v, (name, _))| format!(
                    "{name} {} of {}",
                    inside.iter().filter(|r| r.view == v).count(),
                    self.rays[v]
                ))
                .collect::<Vec<_>>()
                .join(", "),
            self.see_through.len() - inside.len()
        );
        let mut by_part = std::collections::BTreeMap::<&str, usize>::new();
        for r in &inside {
            *by_part.entry(self.names.get(r.part).map_or("?", String::as_str)).or_default() += 1;
        }
        for (name, n) in by_part {
            let first =
                inside.iter().find(|r| self.names.get(r.part).map(String::as_str) == Some(name));
            let at = first.map_or([0.0; 3], |r| r.at);
            let _ = writeln!(
                s,
                "  {name}: {n}, at ({:.3}, {:.3}, {:.3}) and near",
                at[0], at[1], at[2]
            );
        }
        let _ = writeln!(s, "by part (the part nearest each point):");
        for (i, name) in self.names.iter().enumerate() {
            let st = self.stats(Some(i));
            let _ = writeln!(
                s,
                "  {name:<20} {:>6} points  mean {:>5.1} mm  90% {:>5.1}  worst {:>6.1}  {:>3.0}% within 2 mm  {:>3.0}% within 5",
                st.n,
                st.mean * 1e3,
                st.p90 * 1e3,
                st.worst * 1e3,
                st.within_2mm * 100.0,
                st.within_5mm * 100.0
            );
        }
        let _ = writeln!(
            s,
            "worst places (more than 0: the replica is short of the mesh; less: past it):"
        );
        for (p, d, i) in self.worst(5, None) {
            let _ = writeln!(
                s,
                "  {:+.1} mm at ({:.3}, {:.3}, {:.3}), {}",
                d * 1e3,
                p[0],
                p[1],
                p[2],
                self.names.get(*i).map_or("?", String::as_str)
            );
        }
        s
    }

    pub fn to_json(&self) -> serde_json::Value {
        let all = self.stats(None);
        let parts: Vec<serde_json::Value> = self
            .names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let st = self.stats(Some(i));
                let places: Vec<serde_json::Value> = self
                    .worst(3, Some(i))
                    .iter()
                    .map(|(p, d, _)| serde_json::json!({"at": p, "distance": d}))
                    .collect();
                serde_json::json!({"part": name, "points": st.n, "mean": st.mean, "median": st.median, "p90": st.p90, "worst": st.worst, "within_2mm": st.within_2mm, "within_5mm": st.within_5mm, "worst_places": places})
            })
            .collect();
        let worst: Vec<serde_json::Value> = self
            .worst(5, None)
            .iter()
            .map(
                |(p, d, i)| serde_json::json!({"at": p, "distance": d, "part": self.names.get(*i)}),
            )
            .collect();
        serde_json::json!({
            "surface": {"points": all.n, "hidden": self.hidden, "mean": all.mean, "median": all.median, "p90": all.p90, "worst": all.worst, "within_2mm": all.within_2mm, "within_5mm": all.within_5mm},
            "volume": {"iou": self.iou(), "mesh_only_cm3": self.cm3(self.mesh_only), "replica_only_cm3": self.cm3(self.replica_only), "furthest_past": self.furthest_past},
            "parts": parts,
            "worst": worst,
            "see_through": {
                "views": VIEWS.iter().map(|(n, d)| serde_json::json!({"name": n, "towards": d})).collect::<Vec<_>>(),
                "rays": self.rays,
                "inside": self.see_through.iter().filter(|r| r.inside).count(),
                "missed": self.see_through.iter().map(|r| serde_json::json!({"view": r.view, "at": r.at, "part": self.names.get(r.part), "inside": r.inside})).collect::<Vec<_>>(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toml_subset_reads_tables_arrays_quotes_and_lists() {
        let t = parse_toml(
            "# a comment\nreference = \"a \\\"b\\\" # c\" # d\n[landmarks]\n\"elbow left\" = [0.1, -2, 3_000.5]\n[[part]]\nname = \"x\"\n[[part]]\nname = \"y\"\nangles = 32\n",
        )
        .unwrap();
        assert_eq!(t[0].get("reference"), Some(&Value::Str("a \"b\" # c".into())));
        assert_eq!(
            t[1].get("elbow left"),
            Some(&Value::List(vec![Value::Num(0.1), Value::Num(-2.0), Value::Num(3000.5)]))
        );
        assert_eq!(t.iter().filter(|t| t.array && t.header == "part").count(), 2);
        assert_eq!(t[3].get("angles"), Some(&Value::Num(32.0)));
        assert!(parse_toml("x = [1, 2\n").is_err());
        assert!(parse_toml("x = 1 2\n").is_err());
    }

    /// A sphere mesh (radius 0.1 m at (0, 0.2, 0) once placed) and its manifest in a new
    /// directory (`wrela-reference-<name>-<pid>` in the temporary one), with the parts file
    /// `parts`, read; and the mesh as a solid.
    fn ball(name: &str, parts: &str) -> (PathBuf, PartsFile, Solid) {
        let dir =
            std::env::temp_dir().join(format!("wrela-reference-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ball.obj"),
            crate::mesh::tests::sphere_obj([0.0, 2.0, 0.0], 1.0, 32, 64),
        )
        .unwrap();
        std::fs::write(
            dir.join("ball.toml"),
            "title = \"Ball\"\nauthor = \"wrela's tests\"\nlicence = \"MIT\"\naxes = \"x,y,z\"\norigin = [0.0, 0.0, 0.0]\nmetres_per_unit = 0.1\n",
        )
        .unwrap();
        std::fs::write(dir.join("ball.parts.toml"), parts).unwrap();
        let file = PartsFile::read(&dir.join("ball.parts.toml")).unwrap();
        let solid = file.solid().unwrap();
        (dir, file, solid)
    }

    /// A replica of a sphere mesh (radius 0.1 m once placed), one part through it: it builds,
    /// its distance is the sphere's beside its side, and the deviation finds it close.
    #[test]
    fn a_sphere_s_replica_builds_and_matches() {
        let (dir, file, solid) = ball(
            "sphere",
            "reference = \"ball.toml\"\n[landmarks]\n\"bottom\" = [0.0, 0.09, 0.0]\n[[part]]\nname = \"ball\"\nfrom = \"bottom\"\nto = [0.0, 0.31, 0.0]\nup = [0.0, 0.0, 1.0]\nangles = 32\nspacing = 0.005\n",
        );
        let (part, start, end) = trimmed(&solid, &file.parts[0]).unwrap();
        // The axis runs 1 cm past the sphere at each end, and 2 cm more for overlap: trimmed.
        assert!((start - 0.03).abs() < 0.002 && (end - 0.03).abs() < 0.002, "{start} {end}");
        let s = sample(&solid, &part, (start, end));
        assert!(s.loft().is_some_and(|t| t.outside.is_empty() && t.held == 0));
        let out = dir.join("replica");
        let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../engine");
        write_package(&file, &[s], &out, &engine).unwrap();
        assert!(
            std::fs::read_to_string(out.join("replica.wrela"))
                .unwrap()
                .contains("\"Ball\" by wrela's tests, MIT")
        );
        let mut replica = Replica::load(&out).unwrap();
        let mut rng = Rng(7);
        let mut n = 0;
        while n < 200 {
            let p = [rng.next() * 0.3 - 0.15, 0.13 + rng.next() * 0.14, rng.next() * 0.3 - 0.15];
            let exact = mesh::length(mesh::sub(p, [0.0, 0.2, 0.0])) - 0.1;
            if exact.abs() > 0.01 {
                continue;
            }
            let d = replica.probes[0].distance(p).unwrap();
            assert!((d - exact).abs() < 0.002, "at {p:?}: {d} against {exact}");
            n += 1;
        }
        let report = deviation(&file, &solid, &out, 2000).unwrap();
        let Stats { mean, within_2mm, .. } = report.stats(None);
        assert!(mean < 0.001 && within_2mm > 0.95, "mean {mean}, within 2 mm {within_2mm}");
        assert!(report.iou() > 0.97);
        // Nothing to see through, inside the outline, from any view.
        assert_eq!(report.see_through.iter().filter(|r| r.inside).count(), 0);
        assert!(report.rays.iter().all(|&n| n > 1000), "{:?}", report.rays);
        // Compressed (a sphere's sections are circles: one term each), it's still the sphere.
        let (mut compact, _) = compress(
            &[sample(&solid, &part, (start, end))],
            Compression { harmonics: 2, every: 0.02 },
        );
        let (compact, numbers) = compact.remove(0);
        // 0.2 m in sections every 2 cm: 11, of 5 numbers each.
        assert_eq!(numbers, 11 * 5);
        let small = dir.join("compact");
        write_package(&file, &[compact], &small, &engine).unwrap();
        let report = deviation(&file, &solid, &small, 2000).unwrap();
        let mean = report.stats(None).mean;
        assert!(mean < 0.0015, "compressed: mean {mean}");
        assert!(Replica::load(&dir.join("nothing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ball again, as a sweep through a bent path, 5 sections of 2 bumps: each section a
    /// circle whose centre is off the path where the path bends away. Fitted, built and
    /// measured, it's the ball within 1.5 mm on average, nothing to see through. A circle's
    /// bumps have no best place, so where the fit puts them turns on the last bits of the
    /// arithmetic, which differ between machines: radii nudged by 1e-15 of themselves give a
    /// mean of 0.84 to 1.02 mm, and 81% to 85% of points within 2 mm. The bars are below that
    /// spread.
    #[test]
    fn a_sphere_s_sweep_replica_builds_and_matches() {
        let (dir, file, solid) = ball(
            "sweep",
            "reference = \"ball.toml\"\n[[part]]\nname = \"ball\"\nfrom = [0.0, 0.11, 0.0]\nthrough = [[0.01, 0.2, 0.0]]\nto = [0.0, 0.29, 0.0]\nup = [0.0, 0.0, 1.0]\nangles = 32\nspacing = 0.005\nsections = 5\nbumps = 2\n",
        );
        let p = &file.parts[0];
        assert_eq!((p.sections, p.bumps, p.through.len()), (5, 2, 1));
        let sampled =
            fit_sweeps(&file.parts, vec![Measure::Sweep(measure_sweep(&solid, p).unwrap())]);
        let Shape::Sweep(sw) = &sampled[0].shape else { panic!("a sweep part is fitted as one") };
        assert!(sw.fitted.sections.len() == 5 && sw.fitted.bumps <= 2);
        let out = dir.join("replica");
        let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../engine");
        write_package(&file, &sampled, &out, &engine).unwrap();
        let source = std::fs::read_to_string(out.join("replica.wrela")).unwrap();
        // A section's numbers go on lines of their own when they don't fit on one.
        let compact: String = source.split_whitespace().collect();
        assert!(compact.contains("sweep(") && compact.contains("section(up:"), "{source}");
        let report = deviation(&file, &solid, &out, 2000).unwrap();
        let Stats { mean, within_2mm, .. } = report.stats(None);
        assert!(mean < 0.0015 && within_2mm > 0.78, "mean {mean}, within 2 mm {within_2mm}");
        assert_eq!(report.see_through.iter().filter(|r| r.inside).count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A circle of radius 300 with a run held at 600 (its rays ran into another part): filled,
    /// the run follows the circle.
    #[test]
    fn held_radii_are_filled_from_the_measured_ones() {
        let held: Vec<bool> = (0..32).map(|j| (10..16).contains(&j)).collect();
        let mut radii: Vec<i32> = held.iter().map(|&h| if h { 600 } else { 300 }).collect();
        fill_held(&mut radii, &held, 0.06);
        assert!(radii.iter().all(|&r| (r - 300).abs() <= 2), "{radii:?}");
        // All held, or none: left alone.
        let mut all = vec![600; 8];
        fill_held(&mut all, &[true; 8], 0.06);
        assert_eq!(all, vec![600; 8]);
    }

    #[test]
    fn catmull_rom_weights_sum_to_1_and_pass_through_their_controls() {
        for t in [0.0, 0.13, 0.5, 0.77, 1.0] {
            let w = catmull_rom_weights(t, 5);
            assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        }
        assert_eq!(catmull_rom_weights(0.5, 5), vec![0.0, 0.0, 1.0, 0.0, 0.0]);
        // A line, to its ends.
        for t in [0.0, 0.05, 0.3, 0.95, 1.0] {
            let w = catmull_rom_weights(t, 3);
            let y: f64 = w.iter().zip([1.0, 3.0, 5.0]).map(|(w, y)| w * y).sum();
            assert!((y - (1.0 + 4.0 * t)).abs() < 1e-12, "{t}: {y}");
        }
    }

    #[test]
    fn least_squares_recovers_an_exact_fit() {
        let basis: Vec<Vec<f64>> = (0..6).map(|i| vec![1.0, i as f64, (i * i) as f64]).collect();
        let y: Vec<f64> = (0..6).map(|i| 2.0 - 0.5 * i as f64 + 0.25 * (i * i) as f64).collect();
        let x = least_squares(&basis, &y, 1e-12);
        for (a, b) in x.iter().zip([2.0, -0.5, 0.25]) {
            assert!((a - b).abs() < 1e-6, "{x:?}");
        }
    }

    /// A table whose sections are two harmonics, changing linearly along the axis, with a run of
    /// held radii (their rays ran into another part) set to `r_max`: two harmonics and two
    /// control sections recover it where it was measured, ignoring the held run.
    #[test]
    fn compression_recovers_harmonics_and_leaves_held_radii_free() {
        let (stations, angles) = (9, 24);
        let r_max = 0.06;
        let mut radii = Vec::new();
        let mut held_at = Vec::new();
        let truth = |i: usize, j: usize| {
            let (t, a) = (i as f64 / 8.0, std::f64::consts::TAU * j as f64 / 24.0);
            0.03 + 0.01 * t + 0.006 * a.cos() + 0.003 * (2.0 * a).sin() * (1.0 - t)
        };
        for i in 0..stations {
            for j in 0..angles {
                let held = (5..9).contains(&j);
                held_at.push(held);
                radii.push(((if held { r_max } else { truth(i, j) }) * 10_000.0).round() as i32);
            }
        }
        let s = Sampled {
            part: Part {
                name: "test".into(),
                to: [0.0, 0.0, 0.08],
                up: [0.0, 1.0, 0.0],
                spacing: 0.01,
                angles,
                r_max,
                ..Part::default()
            },
            shape: Shape::Loft(Lofted {
                u: [0.0, 1.0, 0.0],
                stations,
                radii,
                outside: Vec::new(),
                held: 36,
                held_at,
                trim: (0.0, 0.0),
            }),
        };
        let (c, numbers) = fit_compact(&s, Compression { harmonics: 2, every: 0.08 }, &[]);
        assert_eq!(numbers, 2 * 5);
        for i in 0..stations {
            for j in (0..angles).filter(|j| !(5..9).contains(j)) {
                let got = f64::from(radii_of(&c)[i * angles + j]) / 10_000.0;
                assert!((got - truth(i, j)).abs() < 0.0003, "station {i}, angle {j}: {got}");
            }
        }
    }

    /// A loft's radii.
    fn radii_of(s: &Sampled) -> &[i32] {
        &s.loft().expect("a loft").radii
    }

    /// A straight part along z, 9 stations of 24 radii.
    fn straight(name: &str, centre: [f64; 2], r: impl Fn(usize) -> f64) -> Sampled {
        let (stations, angles) = (9, 24);
        Sampled {
            part: Part {
                name: name.into(),
                from: [centre[0], centre[1], 0.0],
                to: [centre[0], centre[1], 0.08],
                up: [0.0, 1.0, 0.0],
                spacing: 0.01,
                angles,
                r_max: 0.06,
                ..Part::default()
            },
            shape: Shape::Loft(Lofted {
                u: [0.0, 1.0, 0.0],
                stations,
                radii: (0..stations * angles)
                    .map(|n| (r(n % angles) * 10_000.0).round() as i32)
                    .collect(),
                outside: Vec::new(),
                held: 0,
                held_at: vec![false; stations * angles],
                trim: (0.0, 0.0),
            }),
        }
    }

    /// A circle of radius 3 cm with a run of radii at 5 cm (angles 5 and 6: rays that ran along
    /// a neighbour), and the neighbour, a cylinder around them. At two harmonics the circle's
    /// part can't follow the run: held up to the circle where nothing else reaches it, it's
    /// never short of a measured radius there, and the run, which the neighbour reaches, is free.
    #[test]
    fn compression_holds_up_what_no_other_part_reaches() {
        let theta = std::f64::consts::TAU * 5.5 / 24.0;
        // Angle θ's direction is cos θ u + sin θ (w × u), with u = +y and w = +z.
        let at = [-0.045 * theta.sin(), 0.045 * theta.cos()];
        let body = straight("body", [0.0, 0.0], |j| if j == 5 || j == 6 { 0.05 } else { 0.03 });
        let neighbour = straight("neighbour", at, |_| 0.015);
        let c = Compression { harmonics: 2, every: 0.08 };
        let (free, _) = fit_compact(&body, c, &[]);
        assert!(radii_of(&free).iter().zip(radii_of(&body)).any(|(f, r)| *f < r - 5));
        let (out, floored) = compress(&[body.clone(), neighbour.clone()], c);
        assert!(floored > 0);
        for (n, (&fitted, &measured)) in radii_of(&out[0].0).iter().zip(radii_of(&body)).enumerate()
        {
            if n % 24 == 5 || n % 24 == 6 {
                assert!(fitted < 450, "the run, at {n}: {fitted}");
            } else {
                assert!(fitted >= measured, "at {n}: {fitted} < {measured}");
            }
        }
        // The neighbour, a circle, is its two harmonics exactly.
        assert!(radii_of(&out[1].0).iter().all(|&r| (r - 150).abs() <= 1));
    }
}
