//! `wrela studio <package> [serve [--port N]]`: builds the lens on the package (AC8–AC10 of
//! #39) and serves it to Chrome, on 127.0.0.1 only.
//!
//! `wrela studio <package> <action> [args...] [--png FILE] [--size WxH]`: runs one of the lens's
//! actions headless, on the native host: prints its JSON answer, and with `--png` the screen
//! after it. `wrela studio <package> run <script> [--size WxH]`: runs a session, one command a
//! line (the lens's commands, as a person types them), and `png <file>` to save the screen; the
//! same session runs in Chrome as typed input.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_driver::studio;
use wrela_host::{Event, Host, Value};

/// The screen a headless run uses unless told: under 900 pixels wide, so the views fill it
/// (wider, the lens draws its panel for people beside them).
const SIZE: (u32, u32) = (896, 896);

/// How many frames a headless run waits for the GPU's answers at most.
const WAIT: u32 = 240;

pub fn run(args: &[String]) -> ExitCode {
    let mut rest = Vec::new();
    let (mut png, mut size, mut port, mut debug) = (None, SIZE, 8417u16, false);
    let mut reference: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--png" => match it.next() {
                Some(p) => png = Some(PathBuf::from(p)),
                None => return crate::usage(),
            },
            "--size" => match it.next().and_then(|s| crate::parse_size(s)) {
                Some(s) => size = s,
                None => return crate::usage(),
            },
            "--port" => match it.next().and_then(|s| s.parse().ok()) {
                Some(p) => port = p,
                None => return crate::usage(),
            },
            "--debug" => debug = true,
            "--reference" => match it.next() {
                Some(p) => reference = Some(PathBuf::from(p)),
                None => return crate::usage(),
            },
            _ => rest.push(a.clone()),
        }
    }
    let Some(pkg) = rest.first().map(PathBuf::from) else { return crate::usage() };
    if let Err(code) = crate::package_dir(&pkg) {
        return code;
    }
    let action = rest.get(1).cloned().unwrap_or_else(|| "serve".into());
    let page = match build(&pkg, debug) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mask = match reference.as_deref().map(read_reference).transpose() {
        Ok(m) => m,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    match action.as_str() {
        "serve" => {
            finish(wrela_driver::serve::start(&pkg, &page, port, debug, mask).map(|server| {
                println!("the lens on `{}`: http://127.0.0.1:{}/", pkg.display(), server.port);
                server.wait();
            }))
        }
        "build" => {
            println!("built the lens on `{}` in {}", pkg.display(), page.display());
            ExitCode::SUCCESS
        }
        "look" => finish(look(&pkg, &page, size, png.as_deref())),
        "beside" | "variants" | "sweep" => compare(&pkg, &page, &action, &rest[2..], size, png),
        "run" => {
            let Some(script) = rest.get(2).map(Path::new) else { return crate::usage() };
            match std::fs::read_to_string(script) {
                Ok(text) => finish(session(&pkg, &page, script, &text, size, mask)),
                Err(e) => {
                    eprintln!("error: can't read {}: {e}", script.display());
                    ExitCode::from(2)
                }
            }
        }
        name => match parse_action(name, &rest[2..]) {
            Ok(a) => finish(one_action(&pkg, &page, &a, size, png.as_deref(), mask)),
            Err(code) => code,
        },
    }
}

/// The exit status of a run of the lens: 0, or 1 after printing why it failed.
fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(1)
        }
    }
}

/// `beside <manifest>`, `variants <package>...` and `sweep <literal> <value>...`
/// (compare.rs), each with `[--view f]` and its PNG (`--png`, or `<action>.png` here).
fn compare(
    pkg: &Path,
    page: &Path,
    action: &str,
    args: &[String],
    size: (u32, u32),
    png: Option<PathBuf>,
) -> ExitCode {
    let mut facing = 0u32;
    let mut plain = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--view" {
            match it.next().and_then(|v| v.parse().ok()) {
                Some(f) if f < 4 => facing = f,
                _ => return crate::usage(),
            }
        } else {
            plain.push(a.clone());
        }
    }
    let png = png.unwrap_or_else(|| PathBuf::from(format!("{action}.png")));
    // Each panel is square: the screen's shorter side.
    let n = size.0.min(size.1);
    match action {
        "beside" => match plain.as_slice() {
            [manifest] => finish(crate::compare::beside(pkg, page, n, Path::new(manifest), &png)),
            _ => crate::usage(),
        },
        "variants" => {
            let others: Vec<PathBuf> = plain.iter().map(PathBuf::from).collect();
            finish(crate::compare::variants(pkg, page, &others, n, facing, &png))
        }
        _ => {
            let Some((literal, values)) = plain.split_first() else { return crate::usage() };
            let (Ok(literal), Ok(values)) = (
                literal.parse::<u32>(),
                values.iter().map(|v| v.parse::<f32>()).collect::<Result<Vec<_>, _>>(),
            ) else {
                return crate::usage();
            };
            finish(crate::compare::sweep(pkg, page, n, literal, &values, facing, &png))
        }
    }
}

/// A reference image as the lens's fit takes it (`studio::reference_mask`).
pub(crate) fn read_reference(path: &Path) -> Result<Vec<u8>, String> {
    let (w, h, rgba) = wrela_host::image::read_png(path)
        .map_err(|e| format!("can't read the reference {}: {e}", path.display()))?;
    studio::reference_mask(w, h, &rgba)
}

/// Builds the lens on `pkg`: its page's directory, or the exit status after printing why not.
pub(crate) fn build(pkg: &Path, debug: bool) -> Result<PathBuf, ExitCode> {
    match studio::build(pkg, debug) {
        Ok((out, page)) => {
            if out.has_errors() {
                crate::report(&out, false);
                return Err(ExitCode::from(1));
            }
            // Warnings in a line each time the lens builds would bury its answers: their count,
            // and where to read them.
            let warnings = out.diagnostics.iter().filter(|d| !d.is_error()).count();
            if warnings > 0 {
                eprintln!(
                    "{warnings} warning{}: `wrela check {}` shows {}",
                    wrela_diag::plural(warnings),
                    pkg.display(),
                    if warnings == 1 { "it" } else { "them" }
                );
            }
            // The lens's compiled code, kept beside the build: each action loads it.
            if let Err(e) = wrela_host::precompile(&page) {
                eprintln!("error: can't compile the lens: {e}");
                return Err(ExitCode::from(1));
            }
            Ok(page)
        }
        Err(why) => {
            eprintln!("error: {why}");
            Err(ExitCode::from(1))
        }
    }
}

/// The lens running headless, on the native host: the screen's size, and the frames run.
pub(crate) struct Lens {
    host: Host,
    size: (u32, u32),
    frame: u32,
}

impl Lens {
    /// Loads the lens and runs a first frame, so it knows the screen's size. Its writes
    /// (`studio/edit` posts) go through `wrela edit` to the subject's files, as the server's do;
    /// its fit's reference (`studio/reference`) is `mask`.
    pub(crate) fn start(
        pkg: &Path,
        page: &Path,
        size: (u32, u32),
        mask: Option<Vec<u8>>,
    ) -> Result<Lens, String> {
        let post = studio::post_handler(pkg, page, mask);
        // The runner prints the lens's lines itself: the host doesn't echo them too.
        let options = wrela_host::Options { post: Some(post), quiet: true, ..Default::default() };
        let mut host =
            Host::load_with(page, &options).map_err(|e| format!("can't run the lens: {e}"))?;
        host.frame(0.0, size.0, size.1).map_err(|e| e.to_string())?;
        Ok(Lens { host, size, frame: 0 })
    }

    /// The next frame.
    fn step(&mut self) -> Result<(), String> {
        self.frame += 1;
        let t = wrela_host::frame_time(self.frame, 60.0);
        self.host.frame(t, self.size.0, self.size.1).map_err(|e| e.to_string())
    }

    /// Frames until the lens isn't waiting for the GPU (at most `WAIT`), then one more.
    fn settle(&mut self) -> Result<(), String> {
        for _ in 0..WAIT {
            let busy =
                match self.host.call_export("busy", &[]).map_err(|e| e.to_string())?.as_slice() {
                    [Value::I32(b)] => *b != 0,
                    _ => false,
                };
            self.step()?;
            if !busy {
                return Ok(());
            }
        }
        Err(format!("the lens was still waiting for the GPU after {WAIT} frames"))
    }

    /// Export `name` called with `args`, the lens's lines before it dropped, and the frames
    /// after until it settles.
    pub(crate) fn call(&mut self, name: &str, args: &[Value]) -> Result<(), String> {
        let _ = self.host.take_logs();
        self.host.call_export(name, args).map_err(|e| e.to_string())?;
        self.settle()
    }

    /// [`Lens::call`], and its answer: the last JSON line the lens printed, read (which fails
    /// if it printed none).
    pub(crate) fn answer(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<serde_json::Result<serde_json::Value>, String> {
        self.call(name, args)?;
        Ok(serde_json::from_str(crate::last_json(&self.host.take_logs()).unwrap_or_default()))
    }

    /// The screen's RGBA8 pixels.
    pub(crate) fn screen(&mut self) -> Result<Vec<u8>, String> {
        self.host.read_screen().map_err(|e| e.to_string())
    }

    /// The screen, saved as a PNG at `path`.
    fn save(&mut self, path: &Path) -> Result<(), String> {
        let rgba = self.screen()?;
        save_rgba(path, self.size.0, self.size.1, &rgba)
    }

    fn print_logs(&mut self) {
        for line in self.host.take_logs() {
            println!("{line}");
        }
    }
}

/// RGBA8 pixels, `w` by `h`, as a PNG at `path`.
pub(crate) fn save_rgba(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    wrela_host::image::write_png(path, w, h, rgba).map_err(|e| e.to_string())
}

/// `x` to 3 decimals, as the answers give shares and sizes.
pub(crate) fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// `look`: the contact sheet saved (to `png`, or `look.png` here), what changed since the last
/// look marked on a copy (`<png>-changed.png`), and the diagnosis and the spec, in brief: one
/// answer for the loop an author goes round.
fn look(pkg: &Path, page: &Path, size: (u32, u32), png: Option<&Path>) -> Result<(), String> {
    let out = png.map_or_else(|| PathBuf::from("look.png"), Path::to_path_buf);
    let mut lens = Lens::start(pkg, page, size, None)?;
    // The contact sheet, and the diagnosis's and the spec's answers (null if one isn't JSON).
    lens.call("view", &[Value::I32(4)])?;
    lens.save(&out)?;
    let d = lens.answer("diagnose", &[Value::F32(0.0); 4])?.unwrap_or_default();
    let spec = lens.answer("spec", &[])?.unwrap_or_default();
    // What changed since the last look.
    let last = studio::dir(pkg).join("last-look.png");
    let mut changed = serde_json::Value::Null;
    let (w, h, now) = wrela_host::image::read_png(&out).map_err(|e| e.to_string())?;
    if let Ok((lw, lh, before)) = wrela_host::image::read_png(&last)
        && (lw, lh) == (w, h)
    {
        let mut marked = now.clone();
        let mut count = 0usize;
        for (i, (a, b)) in now.chunks_exact(4).zip(before.chunks_exact(4)).enumerate() {
            let diff = (0..3).map(|c| (i32::from(a[c]) - i32::from(b[c])).abs()).max().unwrap_or(0);
            if diff > 24 {
                count += 1;
                marked[4 * i] = 255;
                marked[4 * i + 1] /= 3;
                marked[4 * i + 2] /= 3;
            }
        }
        let stem =
            out.file_stem().map_or_else(|| "look".into(), |s| s.to_string_lossy().into_owned());
        let marked_path = out.with_file_name(format!("{stem}-changed.png"));
        save_rgba(&marked_path, w, h, &marked)?;
        changed = serde_json::json!({
            "png": marked_path.display().to_string(),
            "pixels": count,
            "share": round3(count as f64 / (w as f64 * h as f64)),
        });
    }
    let _ = std::fs::copy(&out, &last);
    let size_of = |lo: &serde_json::Value, hi: &serde_json::Value| -> serde_json::Value {
        let at = |v: &serde_json::Value, k: usize| v[k].as_f64().unwrap_or(0.0);
        serde_json::json!([0, 1, 2].map(|k| round3(at(hi, k) - at(lo, k))))
    };
    let worst: Vec<serde_json::Value> =
        d["worst_gradients"].as_array().cloned().unwrap_or_default().into_iter().take(3).collect();
    let misses: Vec<serde_json::Value> = spec["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["holds"] != true)
        .map(|c| serde_json::json!({ "name": c["name"], "value": c["value"], "target": c["target"], "miss": c["miss"] }))
        .collect();
    let answer = serde_json::json!({
        "action": "look",
        "png": out.display().to_string(),
        "changed": changed,
        "diagnose": {
            "pieces": d["pieces"],
            "pieces_if_thin_parts_join": d["pieces_if_thin_parts_join"],
            "size_m": size_of(&d["lo"], &d["hi"]),
            "lowest": d["lo"][1],
            "below_ground": d["below_ground"],
            "cut_off": d["cut_off"],
            "mass_kg": d["mass"],
            "largest_gradient": d["largest_gradient"],
            "worst_gradients": worst,
        },
        "spec": if spec["checks"].is_array() {
            serde_json::json!({ "held": spec["held"], "missed": spec["missed"], "misses": misses })
        } else {
            serde_json::Value::String("no spec (spec.wrela)".into())
        },
    });
    println!("{answer}");
    Ok(())
}

/// One of the lens's actions, as the command line gives it: its export and the export's
/// arguments (by the lens's table), and for a move or a fit, the literals it may change.
struct Action<'a> {
    name: &'a str,
    args: Vec<Value>,
    literals: Vec<Value>,
}

/// `name` and `args` read as one of the lens's actions, or the exit status after printing why
/// not.
fn parse_action<'a>(name: &'a str, args: &[String]) -> Result<Action<'a>, ExitCode> {
    let Some((_, kinds)) = studio::ACTIONS.iter().find(|(n, _)| *n == name) else {
        let names: Vec<&str> = studio::ACTIONS.iter().map(|(n, _)| *n).collect();
        eprintln!(
            "error: the lens has no action `{name}` (it has serve, build, run, {})",
            names.join(", ")
        );
        return Err(ExitCode::from(2));
    };
    // A move's and a fit's arguments past their own name the literals they may change.
    let named = if (name == "move" || name == "fit") && args.len() > kinds.len() {
        &args[kinds.len()..]
    } else {
        &[]
    };
    if args.len() != kinds.len() + named.len() {
        eprintln!("error: `{name}` takes {} arguments, not {}", kinds.len(), args.len());
        return Err(ExitCode::from(2));
    }
    let mut literals = Vec::new();
    for a in named {
        match a.parse::<u32>() {
            Ok(n) => literals.push(Value::I32(n as i32)),
            Err(_) => {
                eprintln!("error: `{a}` isn't a literal's index");
                return Err(ExitCode::from(2));
            }
        }
    }
    let mut values = Vec::new();
    for (a, k) in args.iter().zip(kinds.chars()) {
        let v = match k {
            'u' => a.parse::<u32>().ok().map(|n| Value::I32(n as i32)),
            _ => a.parse::<f32>().ok().map(Value::F32),
        };
        let Some(v) = v else {
            eprintln!("error: `{a}` isn't a number");
            return Err(ExitCode::from(2));
        };
        values.push(v);
    }
    Ok(Action { name, args: values, literals })
}

/// One action: its literals chosen, its export called, the answer printed, and with `png`, the
/// screen after saved.
fn one_action(
    pkg: &Path,
    page: &Path,
    a: &Action,
    size: (u32, u32),
    png: Option<&Path>,
    mask: Option<Vec<u8>>,
) -> Result<(), String> {
    let mut lens = Lens::start(pkg, page, size, mask)?;
    for l in &a.literals {
        lens.host.call_export("choose", std::slice::from_ref(l)).map_err(|e| e.to_string())?;
    }
    lens.call(a.name, &a.args)?;
    lens.print_logs();
    if let Some(p) = png {
        lens.save(p)?;
    }
    Ok(())
}

/// A session: each line of `text` (the script at `script`) a command typed into the lens (then
/// Enter), or `png FILE` (the screen, saved, its path relative to the script's directory), or
/// `#` a comment.
fn session(
    pkg: &Path,
    page: &Path,
    script: &Path,
    text: &str,
    size: (u32, u32),
    mask: Option<Vec<u8>>,
) -> Result<(), String> {
    let base = script.parent().unwrap_or(Path::new("."));
    let mut lens = Lens::start(pkg, page, size, mask)?;
    let _ = lens.host.take_logs();
    let enter = wrela_abi::input::key_code("Enter");
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(file) = line.strip_prefix("png ") {
            lens.step()?;
            lens.save(&base.join(file.trim()))?;
            continue;
        }
        for c in line.chars() {
            lens.host.push_input(Event::text(c));
        }
        lens.host.push_input(Event::key(true, enter, false, 0));
        lens.host.push_input(Event::key(false, enter, false, 0));
        lens.step()?;
        lens.settle()?;
        lens.print_logs();
    }
    Ok(())
}
