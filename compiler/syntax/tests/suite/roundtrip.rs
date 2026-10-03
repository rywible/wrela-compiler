//! Parse → format → parse gives the same AST, and formatting is idempotent, on hand-written
//! samples. (The grammar-generated round trips live in wrela-grammar's tests.)

use wrela_diag::FileId;
use wrela_syntax::{fmt, parse};

/// The AST's Debug output without spans, and without the flags that record layout (whether a
/// list or a chain link was written on several lines), which formatting may change.
fn shape(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors in:\n{src}\n{:#?}", p.diagnostics);
    let debug = format!("{:#?}", p.file);
    let kept: Vec<&str> = debug
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            !l.starts_with("multiline:") && !l.starts_with("newline_before:")
        })
        .collect();
    strip(&kept.join("\n"))
}

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("Span {") {
        out.push_str(&rest[..i]);
        let j = rest[i..].find('}').map_or(rest.len(), |j| i + j + 1);
        out.push('S');
        rest = &rest[j..];
    }
    out.push_str(rest);
    out
}

fn round_trip(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors:\n{:#?}", p.diagnostics);
    let once = fmt::format(&p, src);
    assert_eq!(shape(src), shape(&once), "format changed the AST:\n{once}");
    let p2 = parse(FileId(0), &once);
    let twice = fmt::format(&p2, &once);
    assert_eq!(once, twice, "formatting isn't idempotent");
    once
}

/// A file using most of the syntax (compiler/grammar's tests read it too).
const SAMPLE: &str = include_str!("../sample.wrela");

#[test]
fn sample_round_trips() {
    round_trip(SAMPLE);
}

#[test]
fn formatting_normalizes() {
    let out = round_trip("fn f(){let a=1;let b=a+2\n\n\n  let c = b}\n");
    assert_eq!(out, "fn f() {\n    let a = 1\n    let b = a + 2\n\n    let c = b\n}\n");
}

#[test]
fn comments_survive() {
    let src = "// top\n\n/// doc\nfn f() { // after brace\n    let a = 1 // trailing\n    // own line\n    let b = 2\n    // before close\n}\n// end\n";
    let out = round_trip(src);
    for c in [
        "// top",
        "/// doc",
        "// after brace",
        "// trailing",
        "// own line",
        "// before close",
        "// end",
    ] {
        assert!(out.contains(c), "lost {c}:\n{out}");
    }
}

#[test]
fn leading_dot_chains_are_kept() {
    let src = "fn f() {\n    let r = a\n        .b(1)\n        .c(2)\n}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
fn multiline_arguments_are_kept() {
    let src = "fn f() {\n    g(\n        1,\n        2,\n    )\n}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
#[ignore]
fn show_sample() {
    println!("{}", round_trip(SAMPLE));
}

#[test]
fn long_lists_and_chains_break_to_fit() {
    let src = "fn f() {\n    let v = compute(first_argument_value, second_argument_value, third_argument, fourth_argument_value)\n    let w = items.filter(is_good_enough_for_us).map(transform_each_one).fold(starting_value, combine_two)\n    let t = alpha_value_one + beta_value_two * gamma + delta_value_three + epsilon_value_four_five_six\n}\n";
    let out = round_trip(src);
    assert_eq!(
        out,
        "fn f() {\n    let v = compute(\n        first_argument_value,\n        second_argument_value,\n        third_argument,\n        fourth_argument_value,\n    )\n    let w = items\n        .filter(is_good_enough_for_us)\n        .map(transform_each_one)\n        .fold(starting_value, combine_two)\n    let t = alpha_value_one +\n        beta_value_two * gamma +\n        delta_value_three +\n        epsilon_value_four_five_six\n}\n"
    );
    assert!(out.lines().all(|l| l.len() <= 100), "{out}");
}

#[test]
fn a_comparison_breaks_only_where_it_can() {
    // A line break after `>` could end the statement (L20): the chain around it breaks
    // elsewhere.
    let src = "fn f() -> bool {\n    first_long_operand_name + second_long_operand_name > third_long_operand_name_that_is_long && done\n}\n";
    let out = round_trip(src);
    assert!(!out.contains(">\n"), "{out}");
}

#[test]
fn comments_inside_expressions_stay_put() {
    let src = "fn f() {\n    let v = g(\n        a, // why a\n        // about b\n        b,\n    )\n    let t = x + // why x\n        y\n    let s = P { a: 1, // one\n        b: 2 }\n    let r = items\n        // keep\n        .map(h)\n}\n";
    let out = round_trip(src);
    assert_eq!(
        out,
        "fn f() {\n    let v = g(\n        a, // why a\n        // about b\n        b,\n    )\n    let t = x + // why x\n        y\n    let s = P {\n        a: 1, // one\n        b: 2,\n    }\n    let r = items\n        // keep\n        .map(h)\n}\n"
    );
}

/// A tiny deterministic generator, for the comment positions.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Every repository source the formatter owns, with comments put after random tokens that
/// can't end a statement (where a line break is whitespace, L17): formatting keeps every
/// comment, doesn't change the AST and is idempotent.
#[test]
fn comments_anywhere_survive_formatting() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["examples", "compiler/std", "compiler/tests/fields", "compiler/tests/run"] {
        collect(&root.join(dir), &mut files);
    }
    assert!(files.len() > 10, "only {} files", files.len());
    // A file per thread at a time; each file's comments come from its own seed.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = files.get(i) else { break };
                        random_comments_survive(
                            path,
                            XorShift(0x9e37_79b9_7f4a_7c15 ^ (i as u64 + 1)),
                        );
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        }
    });
}

fn random_comments_survive(path: &std::path::Path, mut rng: XorShift) {
    let text = std::fs::read_to_string(path).expect("read");
    let tokens = wrela_syntax::lex(FileId(0), &text).tokens;
    let spots: Vec<usize> = tokens
        .iter()
        .filter(|t| !t.kind.can_end_statement() && t.kind != wrela_syntax::TokenKind::Eof)
        .map(|t| t.span.end as usize)
        .collect();
    for round in 0..4 {
        let mut at: Vec<usize> = (0..6).map(|_| spots[rng.below(spots.len())]).collect();
        at.sort();
        at.dedup();
        let mut src = String::new();
        let mut last = 0;
        for (i, &p) in at.iter().enumerate() {
            src.push_str(&text[last..p]);
            src.push_str(&format!(" // c{round}x{i}\n"));
            last = p;
        }
        src.push_str(&text[last..]);
        let name = path.display();
        let p = parse(FileId(0), &src);
        assert!(!p.has_errors(), "{name}: the comments broke the parse:\n{src}");
        let out = round_trip(&src);
        for i in 0..at.len() {
            let c = format!("// c{round}x{i}");
            assert_eq!(out.matches(&c).count(), 1, "{name}: {c} lost or doubled:\n{out}");
        }
    }
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "wrela") {
            out.push(p);
        }
    }
}
