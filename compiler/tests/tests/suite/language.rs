//! docs/language.md's tier-0 program (§19) builds: the reference's example stays true.

use std::path::PathBuf;
use wrela_tests::{build, root};

#[test]
fn the_tier_0_program_in_language_md_builds() {
    let doc = std::fs::read_to_string(root().join("docs/language.md")).expect("language.md");
    let section = &doc[doc.find("## 19. A tier-0 program").expect("§19")..];
    let code = &section[section.find("```wrela\n").expect("a wrela block") + 9..];
    let code = &code[..code.find("```").expect("the block ends")];
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("language-md");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("main.wrela"), code).expect("write");
    if let Err(e) = build(&dir, &dir.join("build")) {
        panic!("§19's program doesn't build:\n{e}");
    }
}
