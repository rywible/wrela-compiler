//! The `wrela.lines` custom section of a program's WASM: where in the source each part of its
//! code came from, so a trap can say where it happened.
//!
//! The section is a table of module offsets, ascending, each mapped to a location
//! (`main.wrela:12:5`) or to none (where code with no source starts, such as a generated
//! helper). A trap at module offset `x` is at the location of the last entry at or before `x`.
//!
//! Encoding, all integers unsigned LEB128: the number of locations, then each as a length and
//! UTF-8 bytes; then the number of entries, then each as an offset and a location number (0 for
//! none, `k` for the `k`-th location).
//!
//! An optimized build ([`SIDECAR`]) keeps the table in a file of its own beside the WASM, with
//! the same encoding, which a host reads only when a trap needs it. The optimizer moves code, so
//! the table goes through it as a source map ([`Lines::source_map`], [`Lines::from_source_map`]).

use std::collections::HashMap;

/// The custom section's name.
pub const SECTION: &str = "wrela.lines";

/// The extension of an optimized build's table, beside its WASM: `game.lines` for `game.wasm`.
pub const SIDECAR: &str = "lines";

/// The base64 digits of a source map's VLQs.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A decoded table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    locations: Vec<String>,
    /// (offset, location number), ascending by offset.
    entries: Vec<(u32, u32)>,
}

impl Lines {
    /// A table from `(offset, location)` entries in ascending offset order. Locations are
    /// numbered in the order they first appear.
    pub fn new(entries: impl IntoIterator<Item = (u32, Option<String>)>) -> Lines {
        let mut lines = Lines::default();
        let mut numbers: HashMap<String, u32> = HashMap::new();
        for (offset, loc) in entries {
            let k = match loc {
                None => 0,
                Some(l) => match numbers.get(&l) {
                    Some(&k) => k,
                    None => {
                        lines.locations.push(l.clone());
                        let k = lines.locations.len() as u32;
                        numbers.insert(l, k);
                        k
                    }
                },
            };
            lines.entries.push((offset, k));
        }
        lines
    }

    /// The location of the code at module offset `offset`, if it has one.
    pub fn at(&self, offset: u32) -> Option<&str> {
        let i = self.entries.partition_point(|(o, _)| *o <= offset).checked_sub(1)?;
        let k = self.entries[i].1;
        (k > 0).then(|| self.locations[k as usize - 1].as_str())
    }

    /// The table as a source map (version 3) of the module: one line, whose columns are module
    /// offsets. An entry with no location is a segment of its column alone. Errors on a location
    /// that isn't `file:line:column`.
    pub fn source_map(&self) -> Result<String, String> {
        let mut sources: Vec<&str> = Vec::new();
        let mut parsed = Vec::with_capacity(self.locations.len());
        for l in &self.locations {
            let bad = || format!("the location `{l}` isn't `file:line:column`");
            let (rest, column) = l.rsplit_once(':').ok_or_else(bad)?;
            let (file, line) = rest.rsplit_once(':').ok_or_else(bad)?;
            let number = |n: &str| n.parse::<i64>().ok().filter(|&n| n >= 1).ok_or_else(bad);
            let (line, column) = (number(line)?, number(column)?);
            let source = match sources.iter().position(|s| *s == file) {
                Some(i) => i,
                None => {
                    sources.push(file);
                    sources.len() - 1
                }
            };
            parsed.push([source as i64, line - 1, column - 1]);
        }
        let mut mappings = String::new();
        let mut last = [0i64; 4];
        for (i, &(offset, k)) in self.entries.iter().enumerate() {
            if i > 0 {
                mappings.push(',');
            }
            vlq(&mut mappings, i64::from(offset) - last[0]);
            last[0] = i64::from(offset);
            if k > 0 {
                for (field, value) in parsed[k as usize - 1].iter().enumerate() {
                    vlq(&mut mappings, value - last[field + 1]);
                    last[field + 1] = *value;
                }
            }
        }
        Ok(serde_json::json!({ "version": 3, "sources": sources, "names": [], "mappings": mappings })
            .to_string())
    }

    /// Reads a source map of a module, as [`Lines::source_map`] writes it or an optimizer
    /// rewrites it: a segment's location is `file:line:column`, and a segment of a column alone
    /// has none.
    pub fn from_source_map(text: &str) -> Result<Lines, String> {
        let map: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("the source map isn't JSON: {e}"))?;
        if map["version"] != 3 {
            return Err("the source map isn't version 3".into());
        }
        let sources: Vec<&str> = map["sources"]
            .as_array()
            .ok_or("the source map has no `sources`")?
            .iter()
            .map(|s| s.as_str().ok_or("a source isn't a string"))
            .collect::<Result<_, _>>()?;
        let mappings = map["mappings"].as_str().ok_or("the source map has no `mappings`")?;
        if mappings.contains(';') {
            return Err("a module's source map has one line".into());
        }
        let mut entries = Vec::new();
        let mut last = [0i64; 4];
        for segment in mappings.split(',').filter(|s| !s.is_empty()) {
            let mut fields = Vec::new();
            let mut bytes = segment.bytes();
            while bytes.len() > 0 {
                fields.push(unvlq(&mut bytes)?);
            }
            if !matches!(fields.len(), 1 | 4 | 5) {
                return Err(format!("the segment `{segment}` has {} fields", fields.len()));
            }
            for (field, value) in fields.iter().take(4).enumerate() {
                last[field] += value;
            }
            let offset = u32::try_from(last[0]).map_err(|_| format!("the offset {}", last[0]))?;
            if entries.last().is_some_and(|&(o, _)| offset < o) {
                return Err(format!("the segments aren't in order at {offset}"));
            }
            let location = if fields.len() == 1 {
                None
            } else {
                let file = usize::try_from(last[1])
                    .ok()
                    .and_then(|i| sources.get(i))
                    .ok_or_else(|| format!("the segment `{segment}` names no source"))?;
                if last[2] < 0 || last[3] < 0 {
                    return Err(format!("the segment `{segment}` is before its source's start"));
                }
                Some(format!("{file}:{}:{}", last[2] + 1, last[3] + 1))
            };
            entries.push((offset, location));
        }
        Ok(Lines::new(entries))
    }

    /// The section's payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        leb(&mut out, self.locations.len() as u32);
        for l in &self.locations {
            leb(&mut out, l.len() as u32);
            out.extend_from_slice(l.as_bytes());
        }
        leb(&mut out, self.entries.len() as u32);
        for (offset, k) in &self.entries {
            leb(&mut out, *offset);
            leb(&mut out, *k);
        }
        out
    }

    /// Reads a section's payload.
    pub fn decode(payload: &[u8]) -> Result<Lines, String> {
        let mut r = Reader { bytes: payload, at: 0 };
        let mut lines = Lines::default();
        for _ in 0..r.leb()? {
            let n = r.leb()? as usize;
            let bytes = r.take(n)?;
            let s = std::str::from_utf8(bytes).map_err(|_| "a location isn't UTF-8")?;
            lines.locations.push(s.to_string());
        }
        let mut last = None;
        for _ in 0..r.leb()? {
            let (offset, k) = (r.leb()?, r.leb()?);
            if k as usize > lines.locations.len() {
                return Err(format!(
                    "entry at {offset} names location {k}, of {}",
                    lines.locations.len()
                ));
            }
            if last.is_some_and(|l| offset < l) {
                return Err(format!("the entries aren't in order at {offset}"));
            }
            last = Some(offset);
            lines.entries.push((offset, k));
        }
        if r.at != payload.len() {
            return Err("bytes after the entries".into());
        }
        Ok(lines)
    }
}

fn leb(out: &mut Vec<u8>, mut x: u32) {
    loop {
        let b = (x & 0x7f) as u8;
        x >>= 7;
        if x == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// `value` as a source map's base64 VLQ: sign in the lowest bit, five bits a digit.
fn vlq(out: &mut String, value: i64) {
    let mut v = if value < 0 { ((-value) << 1) | 1 } else { value << 1 };
    loop {
        let digit = (v & 31) as usize;
        v >>= 5;
        out.push(BASE64[digit | if v > 0 { 32 } else { 0 }] as char);
        if v == 0 {
            return;
        }
    }
}

/// The next base64 VLQ of a segment.
fn unvlq(bytes: &mut std::str::Bytes) -> Result<i64, String> {
    let mut v: i64 = 0;
    for shift in (0..60).step_by(5) {
        let b = bytes.next().ok_or("a segment ends in the middle of a number")?;
        let digit = BASE64.iter().position(|&d| d == b).ok_or("a segment isn't base64")? as i64;
        v |= (digit & 31) << shift;
        if digit & 32 == 0 {
            return Ok(if v & 1 == 1 { -(v >> 1) } else { v >> 1 });
        }
    }
    Err("a number in a segment is too long".into())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// An unsigned LEB128 that fits in 32 bits, in at most 5 bytes.
    fn leb(&mut self) -> Result<u32, String> {
        let mut x = 0u32;
        for shift in (0..35).step_by(7) {
            let b = *self.bytes.get(self.at).ok_or("the section ends early")?;
            self.at += 1;
            x |= u32::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                // The fifth byte holds bits 28 to 34: past 31 is too large.
                if shift == 28 && b > 0x0f {
                    return Err("a number is too large".into());
                }
                return Ok(x);
            }
        }
        Err("a number is too long".into())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.bytes.get(self.at..self.at + n).ok_or("the section ends early")?;
        self.at += n;
        Ok(s)
    }
}

/// The payload of the custom section `name` in a WASM module, if it has one.
pub fn custom_section<'a>(module: &'a [u8], name: &str) -> Option<&'a [u8]> {
    sections(module).filter(|&(id, _)| id == 0).find_map(|(_, body)| {
        let mut b = Reader { bytes: body, at: 0 };
        let n = b.leb().ok()? as usize;
        (b.take(n).ok()? == name.as_bytes()).then(|| &body[b.at..])
    })
}

/// A WASM module's sections, as (id, body), up to the first one it can't read.
pub(crate) fn sections(module: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut r = Reader { bytes: module, at: 8 };
    std::iter::from_fn(move || {
        let id = *r.bytes.get(r.at)?;
        r.at += 1;
        let size = r.leb().ok()? as usize;
        Some((id, r.take(size).ok()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_by_offset() {
        let lines = Lines::new([
            (10, None),
            (12, Some("main.wrela:3:5".into())),
            (20, Some("main.wrela:4:5".into())),
            (30, None),
            (31, Some("main.wrela:3:5".into())),
        ]);
        assert_eq!(lines.at(9), None);
        assert_eq!(lines.at(11), None);
        assert_eq!(lines.at(12), Some("main.wrela:3:5"));
        assert_eq!(lines.at(29), Some("main.wrela:4:5"));
        assert_eq!(lines.at(30), None);
        assert_eq!(lines.at(1000), Some("main.wrela:3:5"));
        let bytes = lines.encode();
        assert_eq!(Lines::decode(&bytes), Ok(lines));
        assert!(Lines::decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn a_table_survives_its_source_map() {
        let lines = Lines::new([
            (3, None),
            (12, Some("main.wrela:3:5".into())),
            (40, Some("engine/sky.wrela:120:17".into())),
            (41, None),
            (70_000, Some("main.wrela:1:1".into())),
            (70_001, Some("main.wrela:3:5".into())),
        ]);
        let map = lines.source_map().expect("map");
        assert!(map.contains(r#""sources":["main.wrela","engine/sky.wrela"]"#), "{map}");
        assert_eq!(Lines::from_source_map(&map), Ok(lines));
        // A source map's own segments: a column alone, and five fields (the fifth a name).
        let theirs =
            r#"{"version":3,"sources":["a.wrela"],"names":["f"],"mappings":"C,EAEEA,CAAC"}"#;
        assert_eq!(
            Lines::from_source_map(theirs),
            Ok(Lines::new([
                (1, None),
                (3, Some("a.wrela:3:3".into())),
                (4, Some("a.wrela:3:4".into())),
            ]))
        );
        assert!(Lines::new([(1, Some("main.wrela".into()))]).source_map().is_err());
        for bad in [
            r#"{"version":3,"sources":[],"mappings":"CAAA"}"#,
            r#"{"version":3,"sources":["a"],"mappings":"C;C"}"#,
            r#"{"version":3,"sources":["a"],"mappings":"E,D"}"#,
            r#"{"version":3,"sources":["a"],"mappings":"CA"}"#,
            r#"{"version":2,"sources":["a"],"mappings":"C"}"#,
        ] {
            assert!(Lines::from_source_map(bad).is_err(), "{bad}");
        }
    }
}
