//! docs/language.md's tier-0 program (§19) builds: the reference's example stays true.

use crate::package;
use wrela_tests::{build, repo_root};

#[test]
fn the_tier_0_program_in_language_md_builds() {
    let doc = std::fs::read_to_string(repo_root().join("docs/language.md")).expect("language.md");
    let section = &doc[doc.find("## 19. A tier-0 program").expect("§19")..];
    let code = &section[section.find("```wrela\n").expect("a wrela block") + 9..];
    let code = &code[..code.find("```").expect("the block ends")];
    if let Err(e) = build(&package("language-md", code)) {
        panic!("§19's program doesn't build:\n{e}");
    }
}
