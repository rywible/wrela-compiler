//! Frames as files, and comparing two hosts' frames.

use crate::error::{Error, Result};
use std::path::Path;

/// Writes RGBA8 pixels (rows top to bottom) as a PNG.
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(Error::Gpu(format!(
            "a {width}x{height} frame has {expected} bytes, not {}",
            rgba.len()
        )));
    }
    let io = |e: std::io::Error| Error::io(path, e);
    let file = std::fs::File::create(path).map_err(io)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let png_err = |e: png::EncodingError| Error::io(path, std::io::Error::other(e));
    let mut writer = encoder.write_header().map_err(png_err)?;
    writer.write_image_data(rgba).map_err(png_err)?;
    writer.finish().map_err(png_err)
}

/// How far apart two frames are, over every channel of every pixel, in 8-bit steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameDiff {
    pub mean: f64,
    pub max: u8,
    /// Channels that differ at all.
    pub differing: usize,
    pub channels: usize,
}

/// Compares two RGBA8 frames of the same size.
pub fn compare(a: &[u8], b: &[u8]) -> Result<FrameDiff, String> {
    if a.len() != b.len() {
        return Err(format!("the frames differ in size: {} and {} bytes", a.len(), b.len()));
    }
    if a.is_empty() {
        return Err("the frames are empty".into());
    }
    let (mut sum, mut max, mut differing) = (0u64, 0u8, 0usize);
    for (&x, &y) in a.iter().zip(b) {
        let d = x.abs_diff(y);
        sum += u64::from(d);
        max = max.max(d);
        differing += usize::from(d != 0);
    }
    Ok(FrameDiff { mean: sum as f64 / a.len() as f64, max, differing, channels: a.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_channels() {
        let d = compare(&[0, 10, 255, 255], &[2, 10, 250, 255]).expect("same size");
        assert_eq!(d, FrameDiff { mean: 7.0 / 4.0, max: 5, differing: 2, channels: 4 });
        assert!(compare(&[0; 4], &[0; 8]).is_err());
    }

    #[test]
    fn writes_a_png_that_decodes() {
        let dir = std::env::temp_dir().join(format!("wrela-host-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("frame.png");
        let rgba: Vec<u8> = (0..2 * 3 * 4).map(|i| (i * 10) as u8).collect();
        write_png(&path, 2, 3, &rgba).expect("written");
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).expect("open")));
        let mut reader = decoder.read_info().expect("header");
        let mut out = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut out).expect("frame");
        assert_eq!((info.width, info.height), (2, 3));
        assert_eq!(&out[..info.buffer_size()], &rgba[..]);
        assert!(write_png(&path, 2, 2, &rgba).is_err());
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
