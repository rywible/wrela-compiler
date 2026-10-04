//! What each diagnostic code means, with a program that has it and the program fixed: what
//! `wrela explain <code>` prints. One file per code, `compiler/driver/explain/<code>.wrela`:
//!
//! ```text
//! // What the code means, in `//` lines.
//! // ---- wrong
//! <the program's main.wrela>
//! // ---- file wrela.toml
//! <another of its files, by its path in the package>
//! // ---- fixed
//! <the fixed program's main.wrela, and its files as above>
//! ```
//!
//! A file left empty isn't part of the program (W0003's has no `main.wrela`). A code that no
//! program can show (a symbolic link's, a bug in the compiler's) has the line
//! `// ---- untested: <why>` instead of programs. The test suite checks that every code has an
//! explanation, that each wrong program gives its code, and that each fixed one compiles
//! (compiler/tests/tests/suite/explain.rs).

include!(concat!(env!("OUT_DIR"), "/explanations.rs"));

/// One code's explanation.
#[derive(Clone, Debug)]
pub struct Explanation {
    pub code: &'static str,
    /// What the code means.
    pub meaning: String,
    /// A program with the code, and the program fixed: each file's path and text. Both are
    /// empty when `untested` says why there's none.
    pub wrong: Vec<(String, String)>,
    pub fixed: Vec<(String, String)>,
    pub untested: Option<String>,
}

/// Every explanation, by code.
pub fn all() -> Vec<Explanation> {
    FILES.iter().map(|(code, text)| parse(code, text)).collect()
}

/// The explanation of `code` (as written, `E0518`), if there is one.
pub fn get(code: &str) -> Option<Explanation> {
    FILES.iter().find(|(c, _)| c.eq_ignore_ascii_case(code)).map(|(c, t)| parse(c, t))
}

fn parse(code: &'static str, text: &str) -> Explanation {
    let mut e = Explanation {
        code,
        meaning: String::new(),
        wrong: Vec::new(),
        fixed: Vec::new(),
        untested: None,
    };
    // Where lines go: the meaning, then a program's current file.
    #[derive(PartialEq)]
    enum In {
        Meaning,
        Wrong,
        Fixed,
    }
    let mut at = In::Meaning;
    for line in text.lines() {
        if let Some(marker) = line.strip_prefix("// ---- ") {
            let (side, file) = match marker.split_once(' ') {
                Some(("file", path)) => (None, path.to_string()),
                Some(("untested:", why)) => {
                    e.untested = Some(why.to_string());
                    continue;
                }
                _ => (Some(marker), "main.wrela".to_string()),
            };
            match side {
                Some("wrong") => at = In::Wrong,
                Some("fixed") => at = In::Fixed,
                _ => {}
            }
            let files = if at == In::Fixed { &mut e.fixed } else { &mut e.wrong };
            files.push((file, String::new()));
            continue;
        }
        match at {
            In::Meaning => {
                let l = line.strip_prefix("//").unwrap_or(line);
                e.meaning.push_str(l.strip_prefix(' ').unwrap_or(l));
                e.meaning.push('\n');
            }
            In::Wrong | In::Fixed => {
                let files = if at == In::Fixed { &mut e.fixed } else { &mut e.wrong };
                if let Some((_, t)) = files.last_mut() {
                    t.push_str(line);
                    t.push('\n');
                }
            }
        }
    }
    e.meaning = e.meaning.trim_end().to_string();
    e
}
