//! A fuzz test (AC9): mutated programs must not crash the compiler. Every `.wrela` file in the
//! repository is a seed. Each case applies a few random edits (deleting, duplicating or swapping
//! spans, inserting tokens that often matter, and inserting deeply nested or very long
//! constructs that probe the compiler's limits) and runs the whole pipeline, checking and
//! building.
//!
//! The cases run in a worker process: this test binary again, running `worker`. A panic, an
//! abort (a stack overflow can't be caught in-process), an internal compiler error (I0001: the
//! compiler caught its own bug) or a case that takes longer than [`CASE_TIMEOUT`] fails the
//! test, with the seed and the program; the worker is restarted after the case that killed it.
//!
//! `WRELA_FUZZ_ITERS` sets the cases (default 100, or 1000 with `WRELA_FULL`) and
//! `WRELA_FUZZ_SEED` the run's seed (default 1).

use crate::scratch;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use wrela_tests::{Rng, files_under, repo_root, sized};

/// How long one case may take. Generous: debug builds are slow, and the worker compiles std
/// once per case.
const CASE_TIMEOUT: Duration = Duration::from_secs(60);

const TOKENS: &[&str] = &[
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ",",
    ".",
    ":",
    "::",
    "=",
    "+",
    "-",
    "*",
    "/",
    "<",
    ">",
    "!",
    "&",
    "|",
    "?",
    "\n",
    " ",
    "mut ",
    "take ",
    "let ",
    "var ",
    "fn ",
    "struct ",
    "if ",
    "else ",
    "match ",
    "for ",
    "in ",
    "return ",
    "0",
    "1.5",
    "-1",
    "x",
    "self",
    "vec3",
    "f32",
    "u32",
    "[f32; 3]",
    "@compute(64)",
    "@gpu",
    "pub ",
    "use ",
    "impl ",
    "trait ",
    "=>",
    "_",
    "..",
];

/// A char boundary at or before `i`.
fn boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A construct that probes a limit: nesting, length, instantiation depth or recursion in
/// types. Sizes range around the compiler's limits (`MAX_NESTING` 128, `MAX_EXPR_DEPTH` 512).
fn stress(rng: &mut Rng) -> String {
    let n = [8, 100, 127, 128, 129, 300, 600, 2000, 20_000][rng.below(9)];
    match rng.below(8) {
        0 => format!("{}x{}", "(".repeat(n), ")".repeat(n)),
        1 => vec!["1"; n].join(" + "),
        2 => format!("{}{}", "{ ".repeat(n), " }".repeat(n)),
        3 => format!("{}f32{}", "Option<".repeat(n), ">".repeat(n)),
        4 => format!("{}1", "-".repeat(n)),
        5 => format!("[{}0{}]", "[".repeat(n.min(2000)), "]".repeat(n.min(2000))),
        6 => "\nfn grow<T: Copy>(x: T, n: u32) -> u32 {\n    if n == 0 { return 1 }\n    \
              grow((x, x), n - 1)\n}\npub fn poly() -> u32 { grow(1, 3) }\n"
            .into(),
        _ => "\nstruct A: Copy + Clone { b: B }\nstruct B: Copy + Clone { a: Option<A> }\n".into(),
    }
}

fn mutate(rng: &mut Rng, mut s: String) -> String {
    for _ in 0..1 + rng.below(3) {
        let n = s.len();
        let a = boundary(&s, rng.below(n + 1));
        let b = boundary(&s, (a + rng.below(24)).min(n));
        match rng.below(6) {
            0 => s.replace_range(a..b, ""),
            1 => {
                let piece = s[a..b].to_string();
                let at = boundary(&s, rng.below(n + 1));
                s.insert_str(at, &piece);
            }
            2 => s.insert_str(a, TOKENS[rng.below(TOKENS.len())]),
            3 => {
                // Swap two lines.
                let mut lines: Vec<String> = s.lines().map(String::from).collect();
                if lines.len() > 1 {
                    let (x, y) = (rng.below(lines.len()), rng.below(lines.len()));
                    lines.swap(x, y);
                }
                s = lines.join("\n");
            }
            4 => {
                let piece = stress(rng);
                s.insert_str(a, &piece);
            }
            _ => {
                // Duplicate a large part of the file.
                let c = boundary(&s, (a + rng.below(4000)).min(n));
                let piece = s[a..c].to_string();
                s.insert_str(c, &piece);
            }
        }
    }
    s
}

/// The case for a seed: a mutated seed file.
fn case(corpus: &[String], seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let pick = rng.below(corpus.len());
    mutate(&mut rng, corpus[pick].clone())
}

/// The seeds: every `.wrela` file under compiler/ and examples/.
fn corpus() -> Vec<String> {
    let corpus: Vec<String> = ["compiler", "examples"]
        .iter()
        .flat_map(|dir| files_under(&repo_root().join(dir), &["wrela"]))
        .map(|p| std::fs::read_to_string(p).expect("read"))
        .collect();
    assert!(corpus.len() > 50, "only {} seeds", corpus.len());
    corpus
}

fn seed_of(base: u64, i: u64) -> u64 {
    base.wrapping_mul(1_000_003).wrapping_add(i)
}

/// Starts each of the worker's reports. The test harness writes on the same stdout, and not
/// always whole lines (`test fuzz::worker ... ` comes just before the first report), so a
/// report is found by this mark, anywhere in a line.
const MARK: &str = "wrela-fuzz:";

/// A report from the worker.
#[derive(Debug, PartialEq)]
enum Report<'a> {
    Start(u64),
    Panic(u64, &'a str),
    Internal(u64, &'a str),
    Done,
}

/// The report in a line of the worker's stdout, if it has one.
fn report(line: &str) -> Option<Report<'_>> {
    let (_, report) = line.split_once(MARK)?;
    let mut w = report.trim_start().splitn(3, ' ');
    let (kind, i, rest) = (w.next()?, w.next().and_then(|i| i.parse().ok()), w.next());
    Some(match (kind, i) {
        ("done", _) => Report::Done,
        ("start", Some(i)) => Report::Start(i),
        ("panic", Some(i)) => Report::Panic(i, rest.unwrap_or("")),
        ("internal", Some(i)) => Report::Internal(i, rest.unwrap_or("")),
        _ => panic!("the worker reported `{report}`"),
    })
}

#[test]
fn reports_are_found_in_the_harness_output() {
    assert_eq!(report("running 1 test"), None);
    assert_eq!(report("test fuzz::worker ... wrela-fuzz: start 0"), Some(Report::Start(0)));
    assert_eq!(
        report("wrela-fuzz: panic 3 index out of bounds"),
        Some(Report::Panic(3, "index out of bounds"))
    );
    assert_eq!(
        report("wrela-fuzz: internal 4 I0001: no type"),
        Some(Report::Internal(4, "I0001: no type"))
    );
    assert_eq!(report("wrela-fuzz: done"), Some(Report::Done));
}

/// What the worker reports on stdout, each after [`MARK`]: `start <i>` before case `i`,
/// `panic <i> <message>` if it panicked, `internal <i> <message>` if it reported an internal
/// compiler error, and `done` at the end.
#[test]
#[ignore = "run by mutated_programs_dont_crash_the_compiler, in its own process"]
fn worker() {
    let Ok(spec) = std::env::var("WRELA_FUZZ_WORKER") else { return };
    let mut parts = spec.split(' ').map(|x| x.parse::<u64>().expect("a number"));
    let (base, from, to) = (parts.next(), parts.next(), parts.next());
    let (Some(base), Some(from), Some(to)) = (base, from, to) else { panic!("bad spec {spec}") };
    let corpus = corpus();
    let dir = scratch(&format!("fuzz-{}", std::process::id()));
    std::panic::set_hook(Box::new(|_| {}));
    let mut out = std::io::stdout().lock();
    for i in from..to {
        let text = case(&corpus, seed_of(base, i));
        std::fs::write(dir.join("main.wrela"), &text).expect("write");
        writeln!(out, "{MARK} start {i}").and_then(|()| out.flush()).expect("stdout");
        let r = std::panic::catch_unwind(|| wrela_driver::build(&dir));
        match r {
            Ok(built) => {
                if let Some(d) = built.diagnostics.iter().find(|d| d.code.is_internal()) {
                    let msg = format!("{}: {}", d.code, d.message);
                    writeln!(out, "{MARK} internal {i} {}", msg.replace('\n', " "))
                        .expect("stdout");
                }
            }
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                writeln!(out, "{MARK} panic {i} {}", msg.replace('\n', " ")).expect("stdout");
            }
        }
    }
    writeln!(out, "{MARK} done").and_then(|()| out.flush()).expect("stdout");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mutated_programs_dont_crash_the_compiler() {
    let iters: u64 = std::env::var("WRELA_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(sized(100, 1000));
    let base: u64 = std::env::var("WRELA_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let exe = std::env::current_exe().expect("the test binary");
    let mut crashes: Vec<(u64, String)> = Vec::new();
    let mut next = 0;
    while next < iters && crashes.len() < 3 {
        let mut child = Command::new(&exe)
            .args(["fuzz::worker", "--exact", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("WRELA_FUZZ_WORKER", format!("{base} {next} {iters}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the worker");
        let (tx, rx) = mpsc::channel();
        let stdout = child.stdout.take().expect("stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut current = None;
        let mut finished = false;
        loop {
            match rx.recv_timeout(CASE_TIMEOUT) {
                // Lines without a report are the test harness's own output.
                Ok(line) => match report(&line) {
                    None => {}
                    Some(Report::Done) => {
                        finished = true;
                        break;
                    }
                    Some(Report::Start(i)) => current = Some(i),
                    Some(Report::Panic(i, why)) => crashes.push((i, format!("panicked: {why}"))),
                    Some(Report::Internal(i, why)) => {
                        crashes.push((i, format!("an internal compiler error: {why}")));
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    if let Some(i) = current {
                        crashes.push((i, format!("took longer than {CASE_TIMEOUT:?}")));
                    }
                    break;
                }
                // The worker died: the case it was on killed it.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let status = child.wait().expect("wait for the worker");
        if finished {
            break;
        }
        match current {
            Some(i) => {
                if !crashes.iter().any(|(c, _)| *c == i) {
                    crashes.push((i, format!("killed the compiler ({status})")));
                }
                next = i + 1;
            }
            None => panic!("the worker didn't start ({status}); crashes before it: {crashes:?}"),
        }
    }
    if crashes.is_empty() {
        return;
    }
    let corpus = corpus();
    let report: Vec<String> = crashes
        .iter()
        .map(|(i, why)| {
            let seed = seed_of(base, *i);
            let text = case(&corpus, seed);
            let shown = if text.len() > 4000 {
                format!("{}...", &text[..boundary(&text, 4000)])
            } else {
                text
            };
            format!("seed {seed} (case {i}): {why}\n----\n{shown}\n----")
        })
        .collect();
    panic!("{} cases crashed the compiler:\n{}", crashes.len(), report.join("\n"));
}
