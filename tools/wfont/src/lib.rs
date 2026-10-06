//! # Fonts for the `ui` package
//!
//! `wfont` turns a TrueType font into a `.wfont` file: each Latin-1 glyph as a signed distance
//! field, its metrics, and the font's kerning. The `ui` package embeds the file and draws text
//! from it at any size (ui/text.wrela). This crate both writes the format ([`make`]) and reads it
//! ([`Font::parse`]), so the tests lay text out exactly as the package does.
//!
//! ## The format, version 1
//!
//! Little-endian. A header, then the glyphs, then the kerning, then the glyphs' fields.
//!
//! | Offset | Field | |
//! |---|---|---|
//! | 0 | magic | the bytes `WFNT` |
//! | 4 | version | 1 |
//! | 8 | `em` | f32: the pixels per em the fields were sampled at |
//! | 12 | `spread` | f32: the distance, in those pixels, that 0 and 255 stand for |
//! | 16 | ascender, descender, line gap | f32 each, in ems (the descender is negative) |
//! | 28 | glyph count | u32 |
//! | 32 | kerning pairs | u32 |
//! | 36 | field bytes | u32 |
//! | 40 | glyphs | 32 bytes each, by code point: the code point, the advance (ems), the field's left edge from the pen and its top above the baseline (pixels at `em`), its width and height (pixels), its offset into the fields, and 0 |
//! | then | kerning | 4 bytes each, by (left, right): two glyphs' indices (a byte each) and the adjustment to the left glyph's advance, an i16 in 1/8192 ems |
//! | then | fields | each glyph's `width × height` bytes, rows from the top |
//!
//! A field's byte `v` is the signed distance `(v / 255 − 0.5) × 2 × spread` pixels at `em`
//! from the pixel's centre to the outline: positive inside. A glyph with nothing to draw (a
//! space) has a 0 × 0 field.

use std::collections::BTreeMap;

pub const MAGIC: [u8; 4] = *b"WFNT";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 40;
pub const GLYPH_LEN: usize = 32;
pub const KERN_LEN: usize = 4;
/// Kerning's unit: 1/8192 em.
pub const KERN_UNIT: f32 = 8192.0;

/// The characters a font has: printable ASCII and the rest of Latin-1.
pub fn latin1() -> impl Iterator<Item = char> {
    (0x20u32..=0x7E).chain(0xA0..=0xFF).filter_map(char::from_u32)
}

/// How fields are sampled.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Pixels per em.
    pub em: f32,
    /// Pixels of distance the field spans each way from the outline.
    pub spread: f32,
}

impl Default for Options {
    fn default() -> Options {
        Options { em: 32.0, spread: 4.0 }
    }
}

/// One glyph, as the file has it.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub codepoint: u32,
    /// Ems.
    pub advance: f32,
    /// The field's left edge from the pen, and its top above the baseline: pixels at `em`.
    pub left: f32,
    pub top: f32,
    pub width: u32,
    pub height: u32,
    /// Into [`Font::fields`].
    pub offset: u32,
}

/// A `.wfont` file, read.
#[derive(Clone, Debug)]
pub struct Font {
    pub em: f32,
    pub spread: f32,
    pub ascender: f32,
    pub descender: f32,
    pub line_gap: f32,
    pub glyphs: Vec<Glyph>,
    /// By (left, right) code points: ems added to the left glyph's advance.
    pub kerning: BTreeMap<(u32, u32), f32>,
    pub fields: Vec<u8>,
}

// ---- making a font ------------------------------------------------------------------------------

/// A point of an outline, in pixels at `em`, y up.
type P = (f64, f64);

/// Collects a glyph's outline as closed polygons, its curves flattened finely enough that the
/// distance to them is within 0.002 px of the true curves' at 32 px per em.
struct Flatten {
    scale: f64,
    contours: Vec<Vec<P>>,
    at: P,
}

impl Flatten {
    fn p(&self, x: f32, y: f32) -> P {
        (f64::from(x) * self.scale, f64::from(y) * self.scale)
    }

    fn push(&mut self, p: P) {
        if let Some(c) = self.contours.last_mut() {
            c.push(p);
        }
        self.at = p;
    }

    /// Segments for a curve whose control polygon is `len` pixels long.
    fn steps(len: f64) -> usize {
        ((len * 4.0).ceil() as usize).clamp(4, 256)
    }
}

impl ttf_parser::OutlineBuilder for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.contours.push(vec![p]);
        self.at = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.push(p);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (a, b, c) = (self.at, self.p(x1, y1), self.p(x, y));
        let n = Self::steps(dist(a, b) + dist(b, c));
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let u = 1.0 - t;
            self.push((
                u * u * a.0 + 2.0 * u * t * b.0 + t * t * c.0,
                u * u * a.1 + 2.0 * u * t * b.1 + t * t * c.1,
            ));
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b, c, d) = (self.at, self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        let n = Self::steps(dist(a, b) + dist(b, c) + dist(c, d));
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let u = 1.0 - t;
            let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.push((
                k0 * a.0 + k1 * b.0 + k2 * c.0 + k3 * d.0,
                k0 * a.1 + k1 * b.1 + k2 * c.1 + k3 * d.1,
            ));
        }
    }

    fn close(&mut self) {
        if let Some(c) = self.contours.last_mut()
            && let (Some(&first), Some(&last)) = (c.first(), c.last())
            && first != last
        {
            c.push(first);
        }
    }
}

fn dist(a: P, b: P) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// The distance from `p` to the segment `a`–`b`.
fn segment_distance(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

/// Whether `p` is inside the polygons, by the nonzero winding rule (TrueType's).
fn inside(p: P, contours: &[Vec<P>]) -> bool {
    let mut winding = 0i32;
    for c in contours {
        for w in c.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a.1 <= p.1 {
                if b.1 > p.1 && cross(a, b, p) > 0.0 {
                    winding += 1;
                }
            } else if b.1 <= p.1 && cross(a, b, p) < 0.0 {
                winding -= 1;
            }
        }
    }
    winding != 0
}

fn cross(a: P, b: P, p: P) -> f64 {
    (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1)
}

/// A glyph's field: its left and top (pixels at `em`), width, height and bytes.
fn field(contours: &[Vec<P>], o: &Options) -> (f32, f32, u32, u32, Vec<u8>) {
    let points = contours.iter().flatten();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in points {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    if x0 > x1 {
        return (0.0, 0.0, 0, 0, Vec::new());
    }
    let pad = f64::from(o.spread).ceil() + 1.0;
    let (left, bottom) = (x0.floor() - pad, y0.floor() - pad);
    let (right, top) = (x1.ceil() + pad, y1.ceil() + pad);
    let (w, h) = ((right - left) as u32, (top - bottom) as u32);
    let segments: Vec<(P, P)> =
        contours.iter().flat_map(|c| c.windows(2).map(|w| (w[0], w[1]))).collect();
    let mut bytes = Vec::with_capacity((w * h) as usize);
    for row in 0..h {
        // Rows from the top: row 0's centre is half a pixel below `top`.
        let y = top - f64::from(row) - 0.5;
        for col in 0..w {
            let p = (left + f64::from(col) + 0.5, y);
            let d =
                segments.iter().map(|&(a, b)| segment_distance(p, a, b)).fold(f64::MAX, f64::min);
            let signed = if inside(p, contours) { d } else { -d };
            let v = (0.5 + signed / (2.0 * f64::from(o.spread))).clamp(0.0, 1.0);
            bytes.push((v * 255.0).round() as u8);
        }
    }
    (left as f32, top as f32, w, h, bytes)
}

/// The kerning between glyphs, from the font's GPOS pair adjustments (the `kern` feature's), in
/// design units: (left glyph, right glyph) → the left glyph's advance's change.
fn gpos_kerning(face: &ttf_parser::Face, ids: &[ttf_parser::GlyphId]) -> BTreeMap<(u16, u16), i32> {
    use ttf_parser::gpos::{PairAdjustment, PositioningSubtable};
    let mut out = BTreeMap::new();
    let Some(gpos) = face.tables().gpos else { return out };
    for lookup in gpos.lookups {
        for sub in lookup.subtables.into_iter::<PositioningSubtable>() {
            let PositioningSubtable::Pair(pair) = sub else { continue };
            for &a in ids {
                for &b in ids {
                    if out.contains_key(&(a.0, b.0)) {
                        continue; // the first subtable that covers a pair decides it
                    }
                    let v = match pair {
                        PairAdjustment::Format1 { coverage, sets } => coverage
                            .get(a)
                            .and_then(|i| sets.get(i))
                            .and_then(|set| set.get(b))
                            .map(|(r, _)| r.x_advance),
                        PairAdjustment::Format2 { coverage, classes, matrix } => {
                            if coverage.get(a).is_none() {
                                None
                            } else {
                                matrix
                                    .get((classes.0.get(a), classes.1.get(b)))
                                    .map(|(r, _)| r.x_advance)
                            }
                        }
                    };
                    if let Some(v) = v {
                        out.insert((a.0, b.0), i32::from(v));
                    }
                }
            }
        }
    }
    out
}

/// Makes a `.wfont` file from a TrueType font: its Latin-1 glyphs.
pub fn make(ttf: &[u8], o: &Options) -> Result<Vec<u8>, String> {
    let face = ttf_parser::Face::parse(ttf, 0).map_err(|e| format!("not a font: {e}"))?;
    let upem = f64::from(face.units_per_em());
    let scale = f64::from(o.em) / upem;
    let ems = |units: f64| (units / upem) as f32;
    let mut glyphs = Vec::new();
    let mut fields = Vec::new();
    let mut ids = Vec::new();
    for c in latin1() {
        // A soft hyphen shows only where a line breaks, as a hyphen: some fonts leave it out.
        let id = face
            .glyph_index(c)
            .or_else(|| (c == '\u{AD}').then(|| face.glyph_index('-')).flatten())
            .ok_or_else(|| format!("the font has no `{c}` (U+{:04X})", c as u32))?;
        ids.push(id);
        let advance = ems(f64::from(face.glyph_hor_advance(id).unwrap_or(0)));
        let mut f = Flatten { scale, contours: Vec::new(), at: (0.0, 0.0) };
        face.outline_glyph(id, &mut f);
        let (left, top, width, height, bytes) = field(&f.contours, o);
        glyphs.push(Glyph {
            codepoint: c as u32,
            advance,
            left,
            top,
            width,
            height,
            offset: fields.len() as u32,
        });
        fields.extend(bytes);
    }
    let by_id: BTreeMap<u16, u32> =
        ids.iter().zip(latin1()).map(|(id, c)| (id.0, c as u32)).collect();
    let mut kerning = BTreeMap::new();
    for ((a, b), v) in gpos_kerning(&face, &ids) {
        let q = (f64::from(v) / upem * f64::from(KERN_UNIT)).round() as f32 / KERN_UNIT;
        if q != 0.0 {
            kerning.insert((by_id[&a], by_id[&b]), q);
        }
    }
    let font = Font {
        em: o.em,
        spread: o.spread,
        ascender: ems(f64::from(face.ascender())),
        descender: ems(f64::from(face.descender())),
        line_gap: ems(f64::from(face.line_gap())),
        glyphs,
        kerning,
        fields,
    };
    Ok(font.bytes())
}

// ---- the file ------------------------------------------------------------------------------------

impl Font {
    /// The file's bytes.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(MAGIC);
        let words = [VERSION];
        out.extend(words.iter().flat_map(|w| w.to_le_bytes()));
        for f in [self.em, self.spread, self.ascender, self.descender, self.line_gap] {
            out.extend(f.to_le_bytes());
        }
        for n in [self.glyphs.len(), self.kerning.len(), self.fields.len()] {
            out.extend((n as u32).to_le_bytes());
        }
        for g in &self.glyphs {
            out.extend(g.codepoint.to_le_bytes());
            for f in [g.advance, g.left, g.top] {
                out.extend(f.to_le_bytes());
            }
            for n in [g.width, g.height, g.offset, 0] {
                out.extend(n.to_le_bytes());
            }
        }
        let index = |cp: u32| {
            self.glyphs.binary_search_by_key(&cp, |g| g.codepoint).expect("kerning between glyphs")
                as u8
        };
        for (&(a, b), v) in &self.kerning {
            out.push(index(a));
            out.push(index(b));
            out.extend(((v * KERN_UNIT).round() as i16).to_le_bytes());
        }
        out.extend(&self.fields);
        out
    }

    /// Reads a `.wfont` file.
    pub fn parse(bytes: &[u8]) -> Result<Font, String> {
        let u32_at = |at: usize| -> Result<u32, String> {
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .ok_or_else(|| "the file is cut short".to_string())
        };
        let f32_at = |at: usize| u32_at(at).map(f32::from_bits);
        if bytes.get(..4) != Some(&MAGIC[..]) {
            return Err("not a .wfont file".into());
        }
        if u32_at(4)? != VERSION {
            return Err(format!("a .wfont of version {}, not {VERSION}", u32_at(4)?));
        }
        let (n, k, fb) = (u32_at(28)? as usize, u32_at(32)? as usize, u32_at(36)? as usize);
        let mut glyphs = Vec::with_capacity(n);
        for i in 0..n {
            let at = HEADER_LEN + i * GLYPH_LEN;
            glyphs.push(Glyph {
                codepoint: u32_at(at)?,
                advance: f32_at(at + 4)?,
                left: f32_at(at + 8)?,
                top: f32_at(at + 12)?,
                width: u32_at(at + 16)?,
                height: u32_at(at + 20)?,
                offset: u32_at(at + 24)?,
            });
        }
        let mut kerning = BTreeMap::new();
        let kern_at = HEADER_LEN + n * GLYPH_LEN;
        for i in 0..k {
            let at = kern_at + i * KERN_LEN;
            let b = bytes.get(at..at + 4).ok_or("the kerning is cut short")?;
            let cp = |j: u8| {
                glyphs
                    .get(usize::from(j))
                    .map(|g| g.codepoint)
                    .ok_or("a kerning pair's glyph isn't there")
            };
            let v = f32::from(i16::from_le_bytes([b[2], b[3]])) / KERN_UNIT;
            kerning.insert((cp(b[0])?, cp(b[1])?), v);
        }
        let fields_at = kern_at + k * KERN_LEN;
        let fields =
            bytes.get(fields_at..fields_at + fb).ok_or("the fields are cut short")?.to_vec();
        Ok(Font {
            em: f32_at(8)?,
            spread: f32_at(12)?,
            ascender: f32_at(16)?,
            descender: f32_at(20)?,
            line_gap: f32_at(24)?,
            glyphs,
            kerning,
            fields,
        })
    }

    /// The glyph for `c`, or `?`'s for a character the font hasn't.
    pub fn glyph(&self, c: char) -> &Glyph {
        let find = |cp: u32| self.glyphs.binary_search_by_key(&cp, |g| g.codepoint).ok();
        let i = find(c as u32).or_else(|| find('?' as u32)).unwrap_or(0);
        &self.glyphs[i]
    }

    /// Where each glyph of `text` goes at `size` pixels per em, its pen starting at `x` on the
    /// baseline: the glyphs and their pens' x, as the ui package lays a line out (ui/text.wrela:
    /// advance and kerning in ems, times the size, summed in f32).
    pub fn layout(&self, text: &str, size: f32, x: f32) -> Vec<(&Glyph, f32)> {
        let mut out = Vec::new();
        let mut pen = x;
        let mut prev: Option<u32> = None;
        for c in text.chars() {
            let g = self.glyph(c);
            if let Some(p) = prev {
                pen += self.kerning.get(&(p, g.codepoint)).copied().unwrap_or(0.0) * size;
            }
            out.push((g, pen));
            pen += g.advance * size;
            prev = Some(g.codepoint);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square, wound as TrueType winds an outer contour: its field is positive inside,
    /// negative outside, and half-way at the edge.
    #[test]
    fn a_squares_field() {
        let sq = vec![vec![(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0), (0.0, 0.0)]];
        let o = Options { em: 32.0, spread: 4.0 };
        let (left, top, w, h, bytes) = field(&sq, &o);
        assert_eq!((left, top, w, h), (-5.0, 15.0, 20, 20));
        let at = |x: u32, y: u32| bytes[(y * w + x) as usize];
        // The centre (5, 5) is 5 px inside: past the spread, 255. Far outside, 0.
        assert_eq!(at(10, 10), 255);
        assert_eq!(at(0, 0), 0);
        // Pixel (5, 10)'s centre is at x = 0.5, 0.5 px inside the left edge.
        let v = f64::from(at(5, 10)) / 255.0;
        assert!((v - (0.5 + 0.5 / 8.0)).abs() < 0.003, "{v}");
    }

    #[test]
    fn files_read_back() {
        let font = Font {
            em: 32.0,
            spread: 4.0,
            ascender: 0.9,
            descender: -0.25,
            line_gap: 0.0,
            glyphs: vec![Glyph {
                codepoint: 65,
                advance: 0.6,
                left: -2.0,
                top: 25.0,
                width: 2,
                height: 1,
                offset: 0,
            }],
            kerning: BTreeMap::from([((65, 65), -410.0 / KERN_UNIT)]),
            fields: vec![10, 20],
        };
        let back = Font::parse(&font.bytes()).expect("reads back");
        assert_eq!(back.glyphs, font.glyphs);
        assert_eq!(back.kerning, font.kerning);
        assert_eq!(back.fields, font.fields);
        assert_eq!((back.em, back.descender), (32.0, -0.25));
    }
}
