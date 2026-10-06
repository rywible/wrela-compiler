//! `wrela edit` (language.md §22): writes new values into literals in the source, each in its
//! literal's own style, so the diff is the literals' characters and nothing else.
//!
//! - **Style.** A literal keeps its form: its decimals (at least as many as it had, more only
//!   where the value needs them to read back exactly), its exponent, its unit suffix, and an
//!   integer stays one where the value is a whole number. The written text reads back as the
//!   exact `f32` asked for: the build rounds a decimal to an `f32` once, and a suffixed number
//!   as its number times its unit, in `f64`, then once to an `f32` (§5).
//! - **Sign.** A literal's value is its own: under a negation (`-0.5`) the expression is its
//!   negative. A value of the other sign moves the minus: `-0.5` given -0.2 becomes `0.2`, and
//!   `0.5` given -0.2 becomes `-0.2`.
//! - **Checks.** Each file must still hash as the edit says it did when its literals were read,
//!   and hold the literal's text at its place; otherwise nothing is written (another tool
//!   changed it). After the edits, the file is lexed again: only the edited literals' tokens
//!   (and a minus moved with one) may differ, or nothing is written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use wrela_syntax::token::{Token, TokenKind};

/// One literal's new value.
#[derive(Clone, Debug, PartialEq)]
pub struct LiteralEdit {
    /// The file, relative to the program's package (as a lifted build's `lift.json` and
    /// `std::lift::file_path` give it).
    pub file: String,
    /// The literal's bytes in the file, and their text.
    pub start: u32,
    pub end: u32,
    pub text: String,
    /// The file's hash when the literal was read (`lift.json`, `std::lift::file_hash`).
    pub hash: String,
    /// The literal's new value: its text's. A literal whose place starts at its minus
    /// (`-0.18`, as a lifted build gives a negative number the build folds) is negative; one
    /// with a minus before its place (`-x` aside, a negation the code makes) is its own,
    /// without the minus.
    pub value: f32,
}

/// One literal as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The bytes replaced (a moved minus included) and what replaced them.
    pub start: u32,
    pub end: u32,
    pub old: String,
    pub new: String,
    /// What the new text must read as (the value asked for, negated where a minus before the
    /// literal was taken into its text), and what it does: the same `f32`.
    pub value: f32,
    pub readback: f32,
}

/// One file's edits.
#[derive(Clone, Debug, PartialEq)]
pub struct FileEdit {
    pub file: String,
    pub old_hash: String,
    pub new_hash: String,
    pub edits: Vec<Written>,
    /// The changed lines: (line number from 1, old, new).
    pub lines: Vec<(u32, String, String)>,
    /// The new text.
    pub text: String,
}

/// Why edits were refused.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub file: String,
    pub why: String,
}

/// A unit suffix's value, from std's `std::units` (`cm` is 0.01).
fn unit_value(suffix: &str) -> Option<f64> {
    let (_, units) = crate::STD_SOURCES.iter().find(|(n, _)| *n == "std::units")?;
    units.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("pub const ")?;
        let (name, rest) = rest.split_once(':')?;
        let value = rest.split_once('=')?.1.trim();
        (name.trim() == suffix).then(|| wrela_syntax::lexer::float_value(value))
    })
}

/// What one unit of a literal's digits is worth (`15cm`: 0.01, `2.5e-3`: 0.001, a plain number:
/// 1): the unit its decimals count in.
pub fn unit_of(text: &str) -> f64 {
    let t = text.trim_start_matches('-').trim_start();
    let (digits, suffix) = wrela_syntax::lexer::split_suffix(t);
    let unit = if suffix.is_empty() { 1.0 } else { unit_value(suffix).unwrap_or(1.0) };
    let exp = digits.find(['e', 'E']).and_then(|i| digits[i + 1..].parse::<i32>().ok());
    unit * 10f64.powi(exp.unwrap_or(0))
}

/// What a literal's text says its value is, as the build reads it (§5, §11).
pub fn literal_value(text: &str) -> Option<f32> {
    let (digits, suffix) = wrela_syntax::lexer::split_suffix(text);
    if digits.is_empty() {
        return None;
    }
    let is_int = !digits.contains(['.', 'e', 'E']);
    if suffix.is_empty() {
        return Some(if is_int {
            wrela_syntax::lexer::int_value(digits).ok()? as f32
        } else {
            wrela_syntax::lexer::float_value_f32(digits)
        });
    }
    let unit = unit_value(suffix)?;
    let n = match wrela_syntax::lexer::int_value(digits) {
        wrela_syntax::ast::IntValue::Ok(v) if is_int => v as f64,
        _ => wrela_syntax::lexer::float_value(digits),
    };
    Some((n * unit) as f32)
}

/// The text for a non-negative value `v` in the style of the literal `old` (its digits and
/// unit): reads back as exactly `v`. `None` if no such text exists, which a finite
/// non-negative `f32` always has.
pub fn format_like(old: &str, v: f32) -> Option<String> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    let (digits, suffix) = wrela_syntax::lexer::split_suffix(old);
    let unit = if suffix.is_empty() { 1.0 } else { unit_value(suffix)? };
    let target = f64::from(v) / unit;
    let exp = digits.find(['e', 'E']);
    let (mant, exp_text) = match exp {
        Some(i) => (&digits[..i], Some(&digits[i..])),
        None => (digits, None),
    };
    let decimals = mant.split_once('.').map_or(0, |(_, f)| f.trim_end_matches('_').len());
    let reads = |s: &str| literal_value(&format!("{s}{suffix}")) == Some(v);
    if let Some(e) = exp_text {
        // Keep the exponent: the mantissa carries the change.
        let k: i32 = e[1..].parse().ok()?;
        let m = target / 10f64.powi(k);
        for d in decimals.max(1)..=17 {
            let s = format!("{m:.d$}{e}");
            if reads(&s) {
                return Some(format!("{s}{suffix}"));
            }
        }
        return None;
    }
    // A whole number stays an integer where its value reads back as one.
    if decimals == 0 && target < 1e15 {
        let s = format!("{}", target.round() as u64);
        if reads(&s) {
            return Some(format!("{s}{suffix}"));
        }
    }
    for d in decimals.max(1)..=17 {
        let s = format!("{target:.d$}");
        if reads(&s) {
            return Some(format!("{s}{suffix}"));
        }
    }
    None
}

/// The tokens of `text` but its line breaks and its end.
fn lex(text: &str) -> impl Iterator<Item = Token> {
    let tokens = wrela_syntax::lexer::lex(wrela_diag::FileId(0), text).tokens;
    tokens.into_iter().filter(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Eof))
}

/// The tokens of `text` (but its line breaks and its end), as (kind, text): what may differ
/// between a file and its edit.
pub(crate) fn tokens(text: &str) -> Vec<(TokenKind, String)> {
    lex(text).map(|t| (t.kind, text[t.span.range()].to_string())).collect()
}

/// Applies `edits` to the files of the program at `root` (each edit's file relative to it):
/// each file's new text and what changed, or why they're refused. Nothing is written here
/// ([`write`] does).
pub fn plan(root: &Path, edits: &[LiteralEdit]) -> Result<Vec<FileEdit>, Refused> {
    let mut by_file: BTreeMap<&str, Vec<&LiteralEdit>> = BTreeMap::new();
    for e in edits {
        by_file.entry(e.file.as_str()).or_default().push(e);
    }
    let mut out = Vec::new();
    for (file, mut list) in by_file {
        let refuse = |why: String| Refused { file: file.to_string(), why };
        let path = root.join(file);
        let text =
            std::fs::read_to_string(&path).map_err(|e| refuse(format!("can't read it: {e}")))?;
        let hash = crate::lift::file_hash(text.as_bytes());
        for e in &list {
            if e.hash != hash {
                return Err(refuse(format!(
                    "it has changed since its literals were read (hash {hash}, not {}): rebuild and read them again",
                    e.hash
                )));
            }
            let at = text.get(e.start as usize..e.end as usize);
            if at != Some(e.text.as_str()) {
                return Err(refuse(format!(
                    "bytes {}..{} aren't `{}`, the literal the edit names",
                    e.start, e.end, e.text
                )));
            }
        }
        list.sort_by_key(|e| e.start);
        if list.windows(2).any(|w| w[0].start == w[1].start) {
            return Err(refuse("two edits of one literal".into()));
        }
        // Each edit's replacement: a minus before it moves with the sign.
        let bytes = text.as_bytes();
        let mut replaced: Vec<(usize, usize, String, f32)> = Vec::new();
        for e in &list {
            let (mut s, end) = (e.start as usize, e.end as usize);
            // A literal whose place holds its minus (`-0.18`) is its number's negative.
            let own_minus = e.text.starts_with('-');
            let number = e.text.trim_start_matches('-').trim_start();
            let negative = e.value < 0.0 || (e.value == 0.0 && e.value.is_sign_negative());
            let minus = if own_minus { None } else { minus_before(bytes, s) };
            let body = format_like(number, e.value.abs()).ok_or_else(|| {
                refuse(format!("no text for the value {} in the style of `{}`", e.value, e.text))
            })?;
            // What the replaced text reads as: the value, or its negative where the minus
            // before it was taken into it.
            let mut reads_as = e.value;
            let new = match (minus, negative) {
                (Some(m), true) => {
                    s = m;
                    reads_as = -e.value;
                    body
                }
                (None, true) => format!("-{body}"),
                _ => body,
            };
            replaced.push((s, end, new, reads_as));
        }
        let mut new_text = String::with_capacity(text.len());
        let mut at = 0;
        let mut written = Vec::new();
        for (s, e, new, v) in &replaced {
            new_text.push_str(&text[at..*s]);
            new_text.push_str(new);
            at = *e;
            let old = text[*s..*e].to_string();
            let readback = signed_value(new).unwrap_or(f32::NAN);
            written.push(Written {
                start: *s as u32,
                end: *e as u32,
                old,
                new: new.clone(),
                value: *v,
                readback,
            });
        }
        new_text.push_str(&text[at..]);
        // Only the literals changed: the old tokens with each edited literal's (and a minus
        // moved with it) replaced by its new text's are the new tokens.
        let mut expected = Vec::new();
        let mut k = 0;
        for t in lex(&text) {
            let (ts, te) = (t.span.start as usize, t.span.end as usize);
            while k < replaced.len() && replaced[k].1 <= ts {
                k += 1;
            }
            if k < replaced.len() && ts >= replaced[k].0 && te <= replaced[k].1 {
                if te == replaced[k].1 {
                    expected.extend(tokens(&replaced[k].2));
                }
                continue;
            }
            expected.push((t.kind, text[ts..te].to_string()));
        }
        if tokens(&new_text) != expected {
            return Err(refuse("the edit would change more than its literals' tokens".into()));
        }
        for w in &written {
            if w.readback.to_bits() != w.value.to_bits() && !(w.readback == 0.0 && w.value == 0.0) {
                return Err(refuse(format!(
                    "`{}` reads back as {}, not {}",
                    w.new, w.readback, w.value
                )));
            }
        }
        // A file `wrela fmt` would leave as it is stays so: a longer literal can push a line
        // past the formatter's width, and the file would then be `fmt`'s to change.
        if formatted(&text) && !formatted(&new_text) {
            return Err(refuse(
                "the edit would leave the file as `wrela fmt` would change it (a line too long): \
                 write the values with fewer digits"
                    .into(),
            ));
        }
        let lines = changed_lines(&text, &new_text);
        out.push(FileEdit {
            file: file.to_string(),
            old_hash: hash,
            new_hash: crate::lift::file_hash(new_text.as_bytes()),
            edits: written,
            lines,
            text: new_text,
        });
    }
    Ok(out)
}

/// `text` as `wrela fmt` formats it, or `None` when it has syntax errors.
pub(crate) fn format_text(text: &str) -> Option<String> {
    let parsed = wrela_syntax::parse(wrela_diag::FileId(0), text);
    (!parsed.has_errors()).then(|| wrela_syntax::fmt::format(&parsed, text))
}

/// Whether `wrela fmt` would leave `text` as it is.
pub(crate) fn formatted(text: &str) -> bool {
    format_text(text).is_some_and(|f| f == text)
}

/// The value a literal's text gives, its minus included.
pub fn signed_value(text: &str) -> Option<f32> {
    let t = text.trim();
    match t.strip_prefix('-') {
        Some(rest) => Some(-literal_value(rest.trim())?),
        None => literal_value(t),
    }
}

/// Where the spaces and tabs just before byte `at` of `bytes` start (`at` if there are none).
fn blanks_before(bytes: &[u8], at: usize) -> usize {
    bytes[..at].iter().rposition(|b| !matches!(b, b' ' | b'\t')).map_or(0, |i| i + 1)
}

/// Where the spaces and tabs from byte `at` of `bytes` end (`at` if there are none).
pub(crate) fn blanks_after(bytes: &[u8], at: usize) -> usize {
    at + bytes
        .get(at..)
        .unwrap_or_default()
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count()
}

/// Whether the `-` at `at` is a negation, not a subtraction: what comes before it (spaces
/// skipped) ends no value.
pub(crate) fn unary(bytes: &[u8], at: usize) -> bool {
    let j = blanks_before(bytes, at);
    if j == 0 {
        return true;
    }
    !(bytes[j - 1].is_ascii_alphanumeric() || matches!(bytes[j - 1], b'_' | b')' | b']' | b'}'))
}

/// Where the unary minus directly before byte `at` of `bytes` is (spaces between allowed), if
/// one is.
pub(crate) fn minus_before(bytes: &[u8], at: usize) -> Option<usize> {
    let m = blanks_before(bytes, at);
    (m > 0 && bytes[m - 1] == b'-' && unary(bytes, m - 1)).then_some(m - 1)
}

/// The lines that differ, as (line from 1, old, new): both texts have the same lines, since
/// literals hold no line breaks.
fn changed_lines(old: &str, new: &str) -> Vec<(u32, String, String)> {
    old.lines()
        .zip(new.lines())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i as u32 + 1, a.to_string(), b.to_string()))
        .collect()
}

/// Writes each file's new text (atomically: a whole file replaces the old). The paths written.
pub fn write(root: &Path, planned: &[FileEdit]) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for f in planned {
        let path = root.join(&f.file);
        crate::write_atomic(&path, f.text.as_bytes())?;
        out.push(path);
    }
    Ok(out)
}

/// A unified diff of the planned edits, a hunk per changed line.
pub fn diff(planned: &[FileEdit]) -> String {
    let mut s = String::new();
    for f in planned {
        s.push_str(&format!("--- {}\n+++ {}\n", f.file, f.file));
        for (line, old, new) in &f.lines {
            s.push_str(&format!("@@ -{line} +{line} @@\n-{old}\n+{new}\n"));
        }
    }
    s
}

/// What `wrela edit --json` prints: the files, each edit, the changed lines, and whether they
/// were written.
pub fn to_json(planned: &[FileEdit], written: bool) -> String {
    crate::pretty(&to_value(planned, written))
}

/// [`to_json`]'s value.
pub fn to_value(planned: &[FileEdit], written: bool) -> serde_json::Value {
    use serde_json::json;
    let files: Vec<_> = planned
        .iter()
        .map(|f| {
            json!({
                "file": f.file,
                "old_hash": f.old_hash,
                "new_hash": f.new_hash,
                "edits": f.edits.iter().map(|e| json!({
                    "start": e.start, "end": e.end, "old": e.old, "new": e.new,
                    "value": e.value, "readback": e.readback,
                })).collect::<Vec<_>>(),
                "lines": f.lines.iter().map(|(n, a, b)| json!({ "line": n, "old": a, "new": b })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "version": 1, "written": written, "files": files, "diff": diff(planned) })
}

/// Reads edits from JSON: `{"edits": [{"file", "start", "end", "text", "hash", "value"}]}`.
pub fn parse_edits(text: &str) -> Result<Vec<LiteralEdit>, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("the edits aren't JSON: {e}"))?;
    let list = v.get("edits").and_then(|e| e.as_array()).ok_or("expected {\"edits\": [...]}")?;
    list.iter()
        .enumerate()
        .map(|(i, e)| {
            let s = |k: &str| {
                e.get(k)
                    .and_then(|x| x.as_str())
                    .map(String::from)
                    .ok_or(format!("edit {i}: `{k}` is missing"))
            };
            let n = |k: &str| {
                e.get(k)
                    .and_then(serde_json::Value::as_u64)
                    .ok_or(format!("edit {i}: `{k}` is missing"))
            };
            let value = e
                .get("value")
                .and_then(serde_json::Value::as_f64)
                .ok_or(format!("edit {i}: `value` is missing"))?;
            Ok(LiteralEdit {
                file: s("file")?,
                start: n("start")? as u32,
                end: n("end")? as u32,
                text: s("text")?,
                hash: s("hash")?,
                value: value as f32,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_are_kept() {
        assert_eq!(format_like("0.086", 0.11), Some("0.110".into()));
        assert_eq!(format_like("0.086", 0.0912), Some("0.0912".into()));
        assert_eq!(format_like("2", 3.0), Some("3".into()));
        assert_eq!(format_like("2", 2.5), Some("2.5".into()));
        assert_eq!(format_like("15cm", 0.17), Some("17cm".into()));
        assert_eq!(format_like("6.5cm", 0.0675), Some("6.75cm".into()));
        assert_eq!(format_like("1e-3", 0.002), Some("2.0e-3".into()));
        assert_eq!(format_like("90deg", std::f32::consts::FRAC_PI_4), Some("45deg".into()));
        for (old, v) in
            [("0.130", 0.13917f32), ("1.13", 1.1342), ("0.3m", 0.31234567), ("7cm", 0.071)]
        {
            let s = format_like(old, v).expect("formats");
            assert_eq!(literal_value(&s), Some(v), "{old} as {v}: {s}");
        }
    }

    #[test]
    fn a_literals_decimals_count_in_its_unit() {
        assert_eq!(unit_of("0.25"), 1.0);
        assert_eq!(unit_of("-15cm"), 0.01);
        assert!((unit_of("2.5e-3") - 0.001).abs() < 1e-18);
        assert!((unit_of("90deg") - std::f64::consts::PI / 180.0).abs() < 1e-15);
    }

    #[test]
    fn an_edit_that_would_unformat_a_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("wrela-edit-fmt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // A formatted call 99 columns wide: past the formatter's width, its arguments would go
        // on lines of their own.
        let args = "0.0, ".repeat(14);
        let text = format!("fn f() {{\n    let x = g(0.25, {args}0.5, 0.7)\n}}\n");
        assert_eq!(text.lines().nth(1).map(str::len), Some(99));
        let parsed = wrela_syntax::parse(wrela_diag::FileId(0), &text);
        assert_eq!(wrela_syntax::fmt::format(&parsed, &text), text, "the file starts formatted");
        std::fs::write(dir.join("m.wrela"), &text).expect("write");
        let start = text.find("0.25").expect("in the text") as u32;
        let edit = |v: f32| LiteralEdit {
            file: "m.wrela".into(),
            start,
            end: start + 4,
            text: "0.25".into(),
            hash: crate::lift::file_hash(text.as_bytes()),
            value: v,
        };
        assert!(plan(&dir, &[edit(0.35)]).is_ok(), "a literal as long as before is written");
        let long = plan(&dir, &[edit(0.123_456_7)]);
        assert!(
            long.as_ref().is_err_and(|r| r.why.contains("wrela fmt")),
            "a literal that pushes the line past the width is refused: {:?}",
            long.map(|p| p[0].text.clone())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minus_signs_move_with_the_sign() {
        let dir = std::env::temp_dir().join(format!("wrela-edit-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let text = "fn f(a: f32) -> f32 {\n    a * -0.5 + vec3(0.25).x - 1.0\n}\n";
        std::fs::write(dir.join("m.wrela"), text).expect("write");
        let hash = crate::lift::file_hash(text.as_bytes());
        let lit = |s: &str, v: f32| {
            let start = text.find(s).expect("in the text") as u32;
            LiteralEdit {
                file: "m.wrela".into(),
                start,
                end: start + s.len() as u32,
                text: s.into(),
                hash: hash.clone(),
                value: v,
            }
        };
        let planned =
            plan(&dir, &[lit("0.5", -0.2), lit("0.25", -0.75), lit("1.0", 2.0)]).expect("plans");
        assert_eq!(
            planned[0].text,
            "fn f(a: f32) -> f32 {\n    a * 0.2 + vec3(-0.75).x - 2.0\n}\n"
        );
        // A stale hash is refused.
        let mut stale = lit("0.5", 0.6);
        stale.hash = "0000".into();
        assert!(plan(&dir, &[stale]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
