//! A take's sheet: one PNG an agent reads instead of listening. From the top: the piano roll
//! (each note from its onset to its release, its voice's colour, brighter when struck harder,
//! with bar lines and their numbers), each note's velocity, the tempo beat by beat, the pedals'
//! depth, the loudness, and a spectrogram (30 Hz to 12 kHz, log-spaced).

use super::{Take, dsp};

const WIDTH: usize = 1800;
const LEFT: usize = 70;
const PLOT: usize = WIDTH - LEFT - 20;

/// Each voice's colour, in order.
const VOICES: [[u8; 3]; 6] =
    [[240, 140, 40], [70, 140, 230], [80, 190, 90], [180, 100, 220], [220, 80, 80], [200, 200, 70]];

struct Image {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Image {
    fn new(w: usize, h: usize) -> Image {
        Image { w, h, px: vec![18; w * h * 3] }
    }

    fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x < self.w && y < self.h {
            let i = 3 * (y * self.w + x);
            self.px[i..i + 3].copy_from_slice(&c);
        }
    }

    fn rect(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 3]) {
        for y in y0..y1.max(y0 + 1) {
            for x in x0..x1.max(x0 + 1) {
                self.set(x, y, c);
            }
        }
    }

    fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, c: [u8; 3]) {
        let n = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
        for i in 0..=n {
            let t = i as f64 / n as f64;
            self.set(
                (x0 + (x1 - x0) * t).round() as usize,
                (y0 + (y1 - y0) * t).round() as usize,
                c,
            );
        }
    }

    /// Text in a 5×7 font, `scale` pixels a dot, from its top-left corner.
    fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: [u8; 3]) {
        let mut pen = x;
        for ch in s.chars() {
            let up = match ch {
                'é' | 'É' => 'E',
                c => c.to_ascii_uppercase(),
            };
            if let Some((_, rows)) = FONT.iter().find(|(k, _)| *k == up) {
                for (r, bits) in rows.iter().enumerate() {
                    for col in 0..5 {
                        if bits & (0b10000 >> col) != 0 {
                            self.rect(
                                pen + col * scale,
                                y + r * scale,
                                pen + (col + 1) * scale,
                                y + (r + 1) * scale,
                                c,
                            );
                        }
                    }
                }
            }
            pen += 6 * scale;
        }
    }

    fn png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w as u32, self.h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&self.px).expect("png data");
        }
        out
    }
}

fn shade(c: [u8; 3], t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [(f64::from(c[0]) * t) as u8, (f64::from(c[1]) * t) as u8, (f64::from(c[2]) * t) as u8]
}

/// A heat colour for `t` from 0 to 1: black, purple, orange, white.
fn heat(t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let stops = [[0.0, 0.0, 0.0], [90.0, 20.0, 120.0], [230.0, 90.0, 30.0], [255.0, 240.0, 200.0]];
    let x = t * 3.0;
    let i = (x.floor() as usize).min(2);
    let u = x - i as f64;
    let mix = |k: usize| (stops[i][k] + (stops[i + 1][k] - stops[i][k]) * u) as u8;
    [mix(0), mix(1), mix(2)]
}

const GRID: [u8; 3] = [55, 55, 60];
const LABEL: [u8; 3] = [200, 200, 205];

/// The sheet, as PNG bytes, for the whole take or bars `bars` (first and last, from 1).
pub fn draw(t: &Take, samples: &[f32], bars: Option<(u32, u32)>) -> Result<Vec<u8>, String> {
    let per = t.beats_per_bar.max(1) as usize;
    let bar_time = |b: u32| t.beats.get((b as usize - 1) * per).copied();
    let (first, last) = bars.unwrap_or((1, t.bars));
    let t0 = bar_time(first).ok_or(format!("the take has no bar {first}"))?;
    let t1 = bar_time(last + 1).unwrap_or(t.seconds.max(t0 + 1.0));
    let x_of = |s: f64| LEFT as f64 + (s - t0) / (t1 - t0) * PLOT as f64;

    let (roll_top, roll_h) = (40, 420);
    let (vel_top, vel_h) = (roll_top + roll_h + 30, 110);
    let (tempo_top, tempo_h) = (vel_top + vel_h + 30, 120);
    let (pedal_top, pedal_h) = (tempo_top + tempo_h + 30, 50);
    let (loud_top, loud_h) = (pedal_top + pedal_h + 30, 90);
    let (spec_top, spec_h) = (loud_top + loud_h + 30, 260);
    let mut img = Image::new(WIDTH, spec_top + spec_h + 20);

    let title = format!("{} - {} - bars {first}-{last} - {:.1} s", t.title, t.name, t1 - t0);
    img.text(LEFT, 10, &title, 2, LABEL);
    let mut x = LEFT + 6 * 2 * title.chars().count() + 40;
    for (i, name) in t.voices.iter().enumerate() {
        img.rect(x, 12, x + 14, 24, VOICES[i % VOICES.len()]);
        img.text(x + 20, 12, name, 2, LABEL);
        x += 20 + 12 * name.chars().count() + 30;
    }

    // Bar lines over every lane, with every bar's number (every fourth's, when there are many).
    let step = if last - first > 40 { 4 } else { 1 };
    for b in first..=last + 1 {
        let Some(s) = bar_time(b) else { continue };
        let x = x_of(s) as usize;
        for lane in [
            (roll_top, roll_h),
            (vel_top, vel_h),
            (tempo_top, tempo_h),
            (pedal_top, pedal_h),
            (loud_top, loud_h),
        ] {
            img.rect(x, lane.0, x + 1, lane.0 + lane.1, GRID);
        }
        if b <= last && (b - first) % step == 0 {
            img.text(x + 2, roll_top - 12, &b.to_string(), 1, LABEL);
        }
    }

    // The piano roll.
    let used: Vec<u32> =
        t.notes.iter().filter(|n| n.release >= t0 && n.onset <= t1).map(|n| n.key).collect();
    let lo = used.iter().min().copied().unwrap_or(21).saturating_sub(2);
    let hi = used.iter().max().copied().unwrap_or(108) + 2;
    let row = roll_h as f64 / f64::from(hi - lo + 1);
    let y_of_key = |k: u32| roll_top as f64 + (f64::from(hi - k)) * row;
    for k in lo..=hi {
        // C's lines, labelled.
        if k % 12 == 0 {
            let y = y_of_key(k) as usize;
            img.rect(LEFT, y, LEFT + PLOT, y + 1, GRID);
            img.text(8, y.saturating_sub(4), &format!("C{}", k / 12 - 1), 1, LABEL);
        }
    }
    for n in &t.notes {
        if n.release < t0 || n.onset > t1 {
            continue;
        }
        let c = shade(VOICES[n.voice as usize % VOICES.len()], 0.35 + 0.65 * n.velocity);
        let (x0, x1) = (x_of(n.onset.max(t0)), x_of(n.release.min(t1)));
        let y = y_of_key(n.key);
        img.rect(
            x0 as usize,
            y as usize,
            x1.max(x0 + 2.0) as usize,
            (y + row.max(2.0) - 1.0) as usize,
            c,
        );
        // The onset, bright.
        img.rect(
            x0 as usize,
            y as usize,
            x0 as usize + 2,
            (y + row.max(2.0) - 1.0) as usize,
            [255, 255, 255],
        );
    }

    // Velocities.
    img.text(8, vel_top, "VEL", 1, LABEL);
    for v in [0.25, 0.5, 0.75] {
        let y = vel_top + ((1.0 - v) * vel_h as f64) as usize;
        img.rect(LEFT, y, LEFT + PLOT, y + 1, GRID);
        img.text(40, y.saturating_sub(3), &format!("{:.2}", v), 1, LABEL);
    }
    for n in &t.notes {
        if n.onset < t0 || n.onset > t1 {
            continue;
        }
        let x = x_of(n.onset) as usize;
        let y = vel_top + ((1.0 - n.velocity) * vel_h as f64) as usize;
        img.rect(
            x.saturating_sub(1),
            y.saturating_sub(1),
            x + 2,
            y + 2,
            VOICES[n.voice as usize % VOICES.len()],
        );
    }

    // The tempo: each beat's, in quarter notes a minute.
    let mut bpm: Vec<(f64, f64)> = Vec::new();
    for w in t.beats.windows(2) {
        if w[1] > t0 && w[0] < t1 && w[1] > w[0] {
            bpm.push((0.5 * (w[0] + w[1]), 60.0 / (w[1] - w[0])));
        }
    }
    let (bmin, bmax) =
        bpm.iter().fold((f64::MAX, f64::MIN), |(a, b), (_, v)| (a.min(*v), b.max(*v)));
    let (bmin, bmax) = ((bmin - 5.0).floor().max(0.0), (bmax + 5.0).ceil());
    let y_of_bpm =
        |v: f64| tempo_top as f64 + (1.0 - (v - bmin) / (bmax - bmin).max(1.0)) * tempo_h as f64;
    img.text(8, tempo_top, "BPM", 1, LABEL);
    img.text(40, tempo_top + 10, &format!("{bmax:.0}"), 1, LABEL);
    img.text(40, tempo_top + tempo_h - 8, &format!("{bmin:.0}"), 1, LABEL);
    for w in bpm.windows(2) {
        img.line(x_of(w[0].0), y_of_bpm(w[0].1), x_of(w[1].0), y_of_bpm(w[1].1), [120, 220, 255]);
    }

    // The pedals: sustain filled, soft as a line.
    img.text(8, pedal_top, "PED", 1, LABEL);
    let depth = |points: &[(f64, f64)], s: f64| {
        let mut d = 0.0;
        for w in points.windows(2) {
            if s >= w[0].0 && s < w[1].0 {
                let u = (s - w[0].0) / (w[1].0 - w[0].0).max(1e-9);
                return w[0].1 + (w[1].1 - w[0].1) * u;
            }
            d = w[1].1;
        }
        if points.len() == 1 { points[0].1 } else { d }
    };
    for px in 0..PLOT {
        let s = t0 + (t1 - t0) * px as f64 / PLOT as f64;
        let d = depth(&t.sustain, s);
        let h = (d * pedal_h as f64) as usize;
        img.rect(
            LEFT + px,
            pedal_top + pedal_h - h,
            LEFT + px + 1,
            pedal_top + pedal_h,
            [60, 90, 170],
        );
        let sd = depth(&t.soft, s);
        img.set(LEFT + px, pedal_top + pedal_h - (sd * pedal_h as f64) as usize, [255, 170, 60]);
    }

    // Loudness: RMS over 50 ms, in dB of full scale.
    img.text(8, loud_top, "DB", 1, LABEL);
    let rate = wrela_abi::AUDIO_SAMPLE_RATE as f64;
    let y_of_db = |d: f64| loud_top as f64 + (-d / 60.0).clamp(0.0, 1.0) * loud_h as f64;
    for d in [-20.0, -40.0] {
        let y = y_of_db(d) as usize;
        img.rect(LEFT, y, LEFT + PLOT, y + 1, GRID);
        img.text(40, y.saturating_sub(3), &format!("{d:.0}"), 1, LABEL);
    }
    let mut prev: Option<(f64, f64)> = None;
    for px in (0..PLOT).step_by(2) {
        let s = t0 + (t1 - t0) * px as f64 / PLOT as f64;
        let a = (2.0 * s * rate) as usize & !1;
        let b = ((2.0 * (s + 0.05) * rate) as usize).min(samples.len());
        let d = if a < b { dsp::db(dsp::rms(&samples[a..b])) } else { -120.0 };
        let p = (x_of(s), y_of_db(d));
        if let Some(q) = prev {
            img.line(q.0, q.1, p.0, p.1, [230, 230, 120]);
        }
        prev = Some(p);
    }

    // The spectrogram, of both channels together.
    let mono: Vec<f32> = samples.chunks_exact(2).map(|c| 0.5 * (c[0] + c[1])).collect();
    let n = 4096;
    let (f_lo, f_hi) = (30.0f64, 12000.0f64);
    img.text(8, spec_top, "HZ", 1, LABEL);
    for f in [100.0, 1000.0, 10000.0] {
        let y = spec_top as f64 + (1.0 - (f / f_lo).ln() / (f_hi / f_lo).ln()) * spec_h as f64;
        img.text(30, y as usize, &format!("{f:.0}"), 1, LABEL);
    }
    for px in 0..PLOT {
        let s = t0 + (t1 - t0) * px as f64 / PLOT as f64;
        let at = ((s * rate) as usize).saturating_sub(n / 2);
        let mag = dsp::spectrum(&mono, at, n);
        for py in 0..spec_h {
            let u0 = 1.0 - (py + 1) as f64 / spec_h as f64;
            let u1 = 1.0 - py as f64 / spec_h as f64;
            let (fa, fb) = (f_lo * (f_hi / f_lo).powf(u0), f_lo * (f_hi / f_lo).powf(u1));
            let (ka, kb) = (
                (fa * n as f64 / rate) as usize,
                ((fb * n as f64 / rate) as usize).max((fa * n as f64 / rate) as usize + 1),
            );
            let m =
                mag[ka.min(mag.len() - 1)..kb.min(mag.len())].iter().copied().fold(0.0, f64::max);
            let d = dsp::db(m);
            img.set(LEFT + px, spec_top + py, heat((d + 100.0) / 80.0));
        }
    }
    Ok(img.png())
}

/// A 5×7 font: each row's five dots, the leftmost in bit 4.
const FONT: &[(char, [u8; 7])] = &[
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
    ('A', [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('B', [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('C', [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('D', [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100]),
    ('E', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111]),
    ('F', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('G', [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111]),
    ('H', [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('I', [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('J', [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('K', [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('L', [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111]),
    ('M', [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001]),
    ('N', [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001]),
    ('O', [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('P', [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('Q', [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101]),
    ('R', [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001]),
    ('S', [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('T', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('U', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('V', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('W', [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010]),
    ('X', [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001]),
    ('Y', [0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100]),
    ('Z', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111]),
    ('.', [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
    (':', [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000]),
    ('/', [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000]),
    ('(', [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010]),
    (')', [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000]),
    ('%', [0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011]),
    ('#', [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010]),
    ('=', [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000]),
];
