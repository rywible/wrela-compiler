//! `wrela migrate`: makes every migration's fix (a warning for a form the language moved away
//! from, `wrela_diag::codes::MIGRATIONS`) and nothing else; `--dry-run` shows the diff.

use crate::common::{self, outputs};

fn package(name: &str, main: &str) -> std::path::PathBuf {
    common::package(name, &[("wrela.toml", &common::manifest("migrating")), ("main.wrela", main)])
}

const OLD: &str = "struct P: Copy + Clone {\n    x: f32,\n}\n\npub fn frame(time: f32, width: u32, height: u32) {\n    let _p = P { x: time }\n}\n";

#[test]
fn a_migration_is_made_and_nothing_else() {
    // An unused local (W0001) has a fix too, which isn't a migration's: it stays.
    let main = OLD.replace("let _p", "let p");
    let dir = package("migrate-made", &main);
    let d = dir.to_str().unwrap();
    let (code, _, report) = outputs(&["migrate", d]);
    assert_eq!(code, Some(0), "{report}");
    assert!(report.contains("migrated W0004"), "{report}");
    let text = std::fs::read_to_string(dir.join("main.wrela")).unwrap();
    assert!(text.contains("struct P: Copy {") && text.contains("let p ="), "{text}");
    // Running it again changes nothing.
    let (code, _, report) = outputs(&["migrate", d]);
    assert_eq!(code, Some(0), "{report}");
    assert!(report.contains("nothing to migrate"), "{report}");
}

#[test]
fn json_says_since_when_a_form_is_a_migration() {
    let dir = package("migrate-json", OLD);
    let v = common::json(common::wrela().args(["migrate", dir.to_str().unwrap(), "--json"]));
    let made = &v["fixed"][0];
    assert_eq!((&made["code"], &made["since"]), (&"W0004".into(), &"0.5.0".into()), "{v}");
    assert_eq!(v["written"], true);
}

#[test]
fn a_dry_run_shows_the_diff_and_writes_nothing() {
    let dir = package("migrate-dry", OLD);
    let (code, out, report) = outputs(&["migrate", dir.to_str().unwrap(), "--dry-run"]);
    assert_eq!(code, Some(0), "{report}");
    assert!(
        out.contains("-struct P: Copy + Clone {") && out.contains("+struct P: Copy {"),
        "{out}"
    );
    assert!(report.contains("would be made"), "{report}");
    assert_eq!(std::fs::read_to_string(dir.join("main.wrela")).unwrap(), OLD);
}

#[test]
fn an_error_is_left_and_the_migration_made() {
    let main = OLD.replace("let _p = P { x: time }", "let _p = P { x: tiem }");
    let dir = package("migrate-errors", &main);
    let (code, _, report) = outputs(&["migrate", dir.to_str().unwrap()]);
    // The migration is still made; the error isn't a migration, so it's reported and left.
    assert_eq!(code, Some(1), "{report}");
    assert!(report.contains("migrated W0004") && report.contains("E0200"), "{report}");
    let text = std::fs::read_to_string(dir.join("main.wrela")).unwrap();
    assert!(text.contains("struct P: Copy {") && text.contains("x: tiem"), "{text}");
}
