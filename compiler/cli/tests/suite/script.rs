//! `wrela script`: a `std::input::Script` constant, printed or written in the hosts' format.
use super::common::{FRAME, package, wrela};

const SCRIPTS: &str = r#"use std::input::{Key, Mods, Script}

/// Two presses.
pub const KEYS: Script = Script::frames().press(1, Key::A, Mods { meta: true }).press(3, Key::Tab)

/// Not a script.
pub const COUNT: u32 = 2
"#;

#[test]
fn a_script_constant_is_printed_or_written_in_the_hosts_format() {
    let dir = package("script-keys", &[("main.wrela", FRAME), ("scripts.wrela", SCRIPTS)]);
    let want = "[\n  {\"frame\": 1, \"type\": \"key\", \"code\": 1, \"meta\": true},\n  {\"frame\": 3, \"type\": \"key\", \"code\": 40}\n]\n";
    let out = wrela().args(["script", dir.to_str().unwrap(), "scripts::KEYS"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), want);
    let file = dir.join("keys.json");
    let out = wrela()
        .args(["script", dir.to_str().unwrap(), "KEYS", "-o", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success() && out.stdout.is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), want);
    for (name, why) in [
        ("scripts::COUNT", "`scripts::COUNT` is a `u32`, not a `std::input::Script`"),
        ("scripts::MISSING", "the package has no constant `scripts::MISSING`"),
    ] {
        let out = wrela().args(["script", dir.to_str().unwrap(), name]).output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains(why), "{name}");
    }
}
