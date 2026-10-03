//! Parse → format → parse gives the same AST, and formatting is idempotent, on hand-written
//! samples. (The grammar-generated round trips live in wrela-grammar's tests.)

use crate::collect;
use wrela_diag::FileId;
use wrela_syntax::lexer::{Brackets, line_break_is_newline};
use wrela_syntax::{TokenKind, fmt, parse};

/// Formats `src`, and checks that the result parses to the same AST and that formatting it
/// again changes nothing ([`fmt::check_round_trip`]).
fn round_trip(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors:\n{:#?}", p.diagnostics);
    fmt::check_round_trip(&p, src).unwrap_or_else(|e| panic!("{e}"))
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

/// Comments stay between the same tokens, each on its own line or at the end of its line: none
/// merges into another, moves to another item or becomes a doc comment of something else.
#[test]
fn comments_keep_their_places() {
    let cases = [
        // A doc comment after `{` stays the field's; a comment after `pub` stays there.
        (
            "pub // a\nstruct S { /// the field\n    x: f32,\n}\n",
            "pub // a\n    struct S { /// the field\n    x: f32,\n}\n",
        ),
        // Comments in a struct pattern break it, like those in any list.
        (
            "fn f(e: E) -> u32 {\n    match e {\n        E::Key {\n            code, // the key\n            down, // pressed?\n        } => 1,\n    }\n}\n",
            "fn f(e: E) -> u32 {\n    match e {\n        E::Key {\n            code, // the key\n            down, // pressed?\n        } => 1,\n    }\n}\n",
        ),
        (
            "fn g(c: C) {\n    let C { width, // in pixels\n        height, .. } = c\n    let (lo, // inclusive\n        hi) = r\n}\n",
            "fn g(c: C) {\n    let C {\n        width, // in pixels\n        height,\n        ..\n    } = c\n    let (\n        lo, // inclusive\n        hi,\n    ) = r\n}\n",
        ),
        // Comments after an attribute stay before the item, not in its parameters; a comment
        // in the parameters stays there.
        (
            "@compute(64)\n// One invocation per cell.\nfn step(id: GlobalId) {}\n\nimpl G {\n    @inline\n    /// The area.\n    pub fn area(self) -> f32 {\n        1.0\n    }\n}\n\n@inline fn f(a: i32, // the a\n    b: i32) {}\n",
            "@compute(64)\n// One invocation per cell.\nfn step(id: GlobalId) {}\n\nimpl G {\n    @inline\n    /// The area.\n    pub fn area(self) -> f32 {\n        1.0\n    }\n}\n\n@inline\nfn f(\n    a: i32, // the a\n    b: i32,\n) {}\n",
        ),
        // Comments in a `use` group stay in it.
        (
            "use std::gpu::{\n    // The thread index.\n    GlobalId, // trailing\n    Slots,\n}\n\nfn f() {}\n",
            "use std::gpu::{\n    // The thread index.\n    GlobalId, // trailing\n    Slots,\n}\n\nfn f() {}\n",
        ),
        // A `)` in a comment, or in a generic bound, doesn't end the parameters.
        (
            "fn f(\n    a: i32, // see g(x)\n    // second\n) -> i32 {\n    a\n}\n",
            "fn f(\n    a: i32, // see g(x)\n    // second\n) -> i32 {\n    a\n}\n",
        ),
        (
            "fn f<T: Foo<fn(i32)>>(\n    // none\n) {}\n",
            "fn f<T: Foo<fn(i32)>>(\n    // none\n) {}\n",
        ),
        // A blank line after a section's comment stays, in a list as between statements.
        (
            "fn h() {\n    g(\n        // section one\n\n        a,\n        b,\n    )\n    let s = S { // header\n\n        x: 1,\n    }\n}\n",
            "fn h() {\n    g(\n        // section one\n\n        a,\n        b,\n    )\n    let s = S { // header\n\n        x: 1,\n    }\n}\n",
        ),
    ];
    for (src, want) in cases {
        assert_eq!(round_trip(src), want, "from:\n{src}");
    }
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

/// Every repository source the formatter owns, with comments put after random tokens where a
/// line break changes no token (L17), at the end of the line or on a line of their own:
/// formatting keeps every comment in its place, doesn't change the AST and is idempotent.
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
    let mut brackets = Brackets::default();
    let mut spots = Vec::new();
    for w in tokens.windows(2) {
        brackets.track(w[0].kind);
        let (a, b) = (w[0].kind, w[1].kind);
        if a == TokenKind::Newline || b == TokenKind::Eof {
            continue;
        }
        // A NEWLINE is already there, or a line break would be whitespace.
        if b == TokenKind::Newline || !line_break_is_newline(brackets.in_brace(), a, b) {
            spots.push(w[0].span.end as usize);
        }
    }
    for round in 0..4 {
        let mut at: Vec<usize> = (0..6).map(|_| spots[rng.below(spots.len())]).collect();
        at.sort();
        at.dedup();
        let mut src = String::new();
        let mut last = 0;
        for (i, &p) in at.iter().enumerate() {
            src.push_str(&text[last..p]);
            let own_line = if rng.below(2) == 0 { "\n" } else { " " };
            src.push_str(&format!("{own_line}// c{round}x{i}\n"));
            last = p;
        }
        src.push_str(&text[last..]);
        let name = path.display();
        let p = parse(FileId(0), &src);
        assert!(!p.has_errors(), "{name}: the comments broke the parse:\n{src}");
        let out = fmt::check_round_trip(&p, &src).unwrap_or_else(|e| panic!("{name}: {e}"));
        for i in 0..at.len() {
            let c = format!("// c{round}x{i}");
            assert_eq!(out.matches(&c).count(), 1, "{name}: {c} lost or doubled:\n{out}");
        }
    }
}
