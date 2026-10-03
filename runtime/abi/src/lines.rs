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

/// The custom section's name.
pub const SECTION: &str = "wrela.lines";

/// A decoded table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    locations: Vec<String>,
    /// (offset, location number), ascending by offset.
    entries: Vec<(u32, u32)>,
}

impl Lines {
    /// A table from `(offset, location)` entries in ascending offset order.
    pub fn new(entries: &[(u32, Option<String>)]) -> Lines {
        let mut lines = Lines::default();
        for (offset, loc) in entries {
            let k = match loc {
                None => 0,
                Some(l) => match lines.locations.iter().position(|x| x == l) {
                    Some(i) => i as u32 + 1,
                    None => {
                        lines.locations.push(l.clone());
                        lines.locations.len() as u32
                    }
                },
            };
            lines.entries.push((*offset, k));
        }
        lines
    }

    /// The location of the code at module offset `offset`, if it has one.
    pub fn at(&self, offset: u32) -> Option<&str> {
        let i = self.entries.partition_point(|(o, _)| *o <= offset).checked_sub(1)?;
        let k = self.entries[i].1;
        (k > 0).then(|| self.locations[k as usize - 1].as_str())
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

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn leb(&mut self) -> Result<u32, String> {
        let mut x = 0u32;
        for shift in (0..35).step_by(7) {
            let b = *self.bytes.get(self.at).ok_or("the section ends early")?;
            self.at += 1;
            x |= u32::from(b & 0x7f).checked_shl(shift).ok_or("a number is too large")?;
            if b & 0x80 == 0 {
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
    let mut r = Reader { bytes: module, at: 8 };
    while r.at < module.len() {
        let id = *module.get(r.at)?;
        r.at += 1;
        let size = r.leb().ok()? as usize;
        let body = r.take(size).ok()?;
        if id == 0 {
            let mut b = Reader { bytes: body, at: 0 };
            let n = b.leb().ok()? as usize;
            if b.take(n).ok()? == name.as_bytes() {
                return Some(&body[b.at..]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_by_offset() {
        let lines = Lines::new(&[
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
}
