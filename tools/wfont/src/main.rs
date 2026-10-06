//! `wfont <font.ttf> <out.wfont> [--em 32] [--spread 4]`: makes a `.wfont` file (the
//! wrela-wfont crate's docs) from a TrueType font's Latin-1 glyphs, and says how big it is.

use std::process::ExitCode;
use wrela_wfont::{Font, Options, make};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Options::default();
    let mut paths = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut number =
            |flag: &str| it.next().and_then(|v| v.parse::<f32>().ok()).ok_or(flag.to_string());
        match a.as_str() {
            "--em" => match number("--em") {
                Ok(v) => o.em = v,
                Err(f) => return usage(&format!("{f} needs a number")),
            },
            "--spread" => match number("--spread") {
                Ok(v) => o.spread = v,
                Err(f) => return usage(&format!("{f} needs a number")),
            },
            _ => paths.push(a.clone()),
        }
    }
    let [from, to] = paths.as_slice() else { return usage("expected a font and an output") };
    let ttf = match std::fs::read(from) {
        Ok(b) => b,
        Err(e) => return usage(&format!("can't read {from}: {e}")),
    };
    let bytes = match make(&ttf, &o) {
        Ok(b) => b,
        Err(e) => return usage(&e),
    };
    if let Err(e) = std::fs::write(to, &bytes) {
        return usage(&format!("can't write {to}: {e}"));
    }
    let font = Font::parse(&bytes).expect("what make wrote reads back");
    println!(
        "{to}: {} bytes: {} glyphs, {} kerning pairs, {} bytes of fields",
        bytes.len(),
        font.glyphs.len(),
        font.kerning.len(),
        font.fields.len()
    );
    ExitCode::SUCCESS
}

fn usage(why: &str) -> ExitCode {
    eprintln!("{why}\nusage: wfont <font.ttf> <out.wfont> [--em 32] [--spread 4]");
    ExitCode::from(2)
}
