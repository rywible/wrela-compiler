//! Sound as numbers, for `wrela audio`: WAV files, levels, MIDI's variable-length numbers, and
//! a Fourier transform.

use std::path::Path;

/// Writes `samples` (channels interleaved) as a 32-bit float WAV file at the voice's rate.
pub fn write_wav(path: &Path, samples: &[f32], channels: u16) -> std::io::Result<()> {
    let rate = wrela_abi::AUDIO_SAMPLE_RATE;
    let data = (samples.len() * 4) as u32;
    let mut out = Vec::with_capacity(58 + samples.len() * 4);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(50 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4 * u32::from(channels)).to_le_bytes());
    out.extend_from_slice(&(4 * channels).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&((samples.len() / channels as usize) as u32).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out)
}

/// A WAV file's samples (channels interleaved, as f32 in [-1, 1]), its channels and its rate.
/// Reads 16-, 24- and 32-bit PCM and 32-bit float.
pub fn read_wav(path: &Path) -> Result<(Vec<f32>, usize, f64), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{} isn't a WAV file", path.display()));
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0usize, 0.0, 0u16);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32_at(at + 4) as usize;
        let body = at + 8;
        if id == b"fmt " {
            format = u16_at(body);
            channels = usize::from(u16_at(body + 2));
            rate = f64::from(u32_at(body + 4));
            bits = u16_at(body + 14);
            if format == 0xFFFE {
                // WAVE_FORMAT_EXTENSIBLE: the sub-format's first two bytes are the format.
                format = u16_at(body + 24);
            }
        } else if id == b"data" {
            let data = &bytes[body..(body + len).min(bytes.len())];
            let samples: Vec<f32> = match (format, bits) {
                (1, 16) => data
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect(),
                (1, 24) => data
                    .chunks_exact(3)
                    .map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0)
                    .collect(),
                (1, 32) => data
                    .chunks_exact(4)
                    .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0)
                    .collect(),
                (3, 32) => data
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
                _ => {
                    return Err(format!(
                        "{}: WAV format {format} at {bits} bits isn't read",
                        path.display()
                    ));
                }
            };
            if channels == 0 {
                return Err(format!("{}: no `fmt ` chunk before the data", path.display()));
            }
            return Ok((samples, channels, rate));
        }
        at = body + len + (len % 2);
    }
    Err(format!("{} has no data", path.display()))
}

/// A level in decibels (of full scale, for samples).
pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

pub fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / samples.len() as f64)
        .sqrt()
}

/// MIDI's variable-length number: seven bits a byte, most significant first.
pub fn vlq(out: &mut Vec<u8>, mut n: u64) {
    let mut bytes = vec![(n & 0x7F) as u8];
    n >>= 7;
    while n > 0 {
        bytes.push((n & 0x7F) as u8 | 0x80);
        n >>= 7;
    }
    bytes.reverse();
    out.extend_from_slice(&bytes);
}

/// The Fourier transform of `re` + i `im`, in place (radix 2: the length a power of two).
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let w = -2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (w.cos(), w.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

/// The magnitude spectrum of `n` samples from `at` (zero past the end), Hann-windowed:
/// `n / 2` bins.
pub fn spectrum(samples: &[f32], at: usize, n: usize) -> Vec<f64> {
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for (i, r) in re.iter_mut().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
        *r = samples.get(at + i).map_or(0.0, |s| f64::from(*s)) * w;
    }
    fft(&mut re, &mut im);
    // A sine of amplitude 1 gives a peak of 1: the window's gain is n / 4.
    (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * 4.0 / n as f64).collect()
}

/// The peak in `mag` between bins `lo` and `hi`: its bin, interpolated by a parabola through
/// its neighbours' logs, and its magnitude.
pub fn peak(mag: &[f64], lo: usize, hi: usize) -> Option<(f64, f64)> {
    let hi = hi.min(mag.len() - 2);
    let lo = lo.max(1);
    if lo >= hi {
        return None;
    }
    let k = (lo..=hi).max_by(|a, b| mag[*a].total_cmp(&mag[*b]))?;
    let (a, b, c) =
        (mag[k - 1].max(1e-15).ln(), mag[k].max(1e-15).ln(), mag[k + 1].max(1e-15).ln());
    let d = a - 2.0 * b + c;
    let off = if d.abs() > 1e-12 { 0.5 * (a - c) / d } else { 0.0 };
    Some((k as f64 + off.clamp(-0.5, 0.5), mag[k]))
}
