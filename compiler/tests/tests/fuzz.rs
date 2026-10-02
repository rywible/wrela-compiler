//! A fuzz smoke test (AC9): mutated programs never crash the compiler. Every `.wrela` file in
//! the repository is a seed; each iteration applies a few random edits (deleting, duplicating
//! or swapping spans, inserting tokens that often matter) and runs the whole pipeline, checking
//! and building. A panic anywhere fails, with the seed and the program that caused it.
//!
//! `WRELA_FUZZ_ITERS` sets the iterations (default 400: a smoke test, seconds).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use wrela_tests::root;

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

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn seeds(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name == "build" || name == "target" || name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            seeds(&p, out);
        } else if p.extension().is_some_and(|x| x == "wrela") {
            out.push(std::fs::read_to_string(&p).expect("read"));
        }
    }
}

/// A char boundary at or before `i`.
fn boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn mutate(rng: &mut Rng, mut s: String) -> String {
    for _ in 0..1 + rng.below(3) {
        let n = s.len();
        let a = boundary(&s, rng.below(n + 1));
        let b = boundary(&s, (a + rng.below(24)).min(n));
        match rng.below(4) {
            0 => s.replace_range(a..b, ""),
            1 => {
                let piece = s[a..b].to_string();
                let at = boundary(&s, rng.below(n + 1));
                s.insert_str(at, &piece);
            }
            2 => s.insert_str(a, TOKENS[rng.below(TOKENS.len())]),
            _ => {
                // Swap two lines.
                let mut lines: Vec<String> = s.lines().map(String::from).collect();
                if lines.len() > 1 {
                    let (x, y) = (rng.below(lines.len()), rng.below(lines.len()));
                    lines.swap(x, y);
                }
                s = lines.join("\n");
            }
        }
    }
    s
}

#[test]
fn mutated_programs_dont_crash_the_compiler() {
    let iters: usize =
        std::env::var("WRELA_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    let mut corpus = Vec::new();
    seeds(&root().join("compiler"), &mut corpus);
    seeds(&root().join("examples"), &mut corpus);
    assert!(corpus.len() > 50, "only {} seeds", corpus.len());
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fuzz");
    std::fs::create_dir_all(&dir).expect("dir");
    let base: u64 = std::env::var("WRELA_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    // The compiler's own panics are what's being looked for: keep their messages quiet.
    std::panic::set_hook(Box::new(|_| {}));
    let mut crashes = Vec::new();
    for i in 0..iters {
        let seed = base.wrapping_mul(1_000_003).wrapping_add(i as u64);
        let mut rng = Rng(seed);
        let pick = rng.below(corpus.len());
        let text = mutate(&mut rng, corpus[pick].clone());
        std::fs::write(dir.join("main.wrela"), &text).expect("write");
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = wrela_driver::build(&dir);
        }));
        if let Err(e) = result {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            crashes.push(format!("seed {seed}: {msg}\n----\n{text}\n----"));
            if crashes.len() >= 3 {
                break;
            }
        }
    }
    let _ = std::panic::take_hook();
    assert!(crashes.is_empty(), "the compiler panicked:\n{}", crashes.join("\n"));
}
