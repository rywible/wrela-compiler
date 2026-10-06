//! AC1, AC10: language.md §17 publishes the closed list of std items the compiler knows
//! (D-081), and the files of std's unsafe core. Both are checked against what's built: the
//! list against the compiler's own table (`wrela_sema::defs::Lang::PATHS`), in both
//! directions, and the core against the std files that use `unsafe`.

use std::collections::BTreeSet;
use wrela_tests::repo_root;

fn spec() -> String {
    std::fs::read_to_string(repo_root().join("docs/language.md")).expect("language.md")
}

/// §17's text, from its heading to the next section's.
fn section_17(spec: &str) -> &str {
    let start = spec.find("## 17. The stdlib boundary").expect("§17");
    let end = spec[start..].find("\n## 18.").map_or(spec.len(), |e| start + e);
    &spec[start..end]
}

#[test]
fn the_closed_list_is_the_compilers() {
    let spec = spec();
    let s17 = section_17(&spec);
    let list = &s17[s17.find("**The closed list**").expect("the closed list")..];
    let mut published = BTreeSet::new();
    for line in list.lines().skip(1) {
        let Some(item) = line.strip_prefix("  - `") else {
            if line.starts_with("- ") {
                break;
            }
            continue;
        };
        let (module, rest) = item.split_once("`: ").expect("`module`: items");
        for name in rest.split(", ") {
            published.insert(format!("{module}::{}", name.trim_matches('`')));
        }
    }
    let known: BTreeSet<String> =
        wrela_sema::defs::Lang::PATHS.iter().map(|(_, p)| p.to_string()).collect();
    let unpublished: Vec<&String> = known.difference(&published).collect();
    let unknown: Vec<&String> = published.difference(&known).collect();
    assert!(
        unpublished.is_empty(),
        "items the compiler knows that §17 doesn't list: {unpublished:?}"
    );
    assert!(unknown.is_empty(), "items §17 lists that the compiler doesn't know: {unknown:?}");
}

#[test]
fn unsafe_is_only_in_the_listed_core() {
    let spec = spec();
    let s17 = section_17(&spec);
    let core = &s17[s17.find("small unsafe core").expect("the unsafe core")..];
    let core = &core[..core.find('\n').unwrap_or(core.len())];
    let listed: BTreeSet<String> = core
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|m| m.strip_prefix("std::"))
        .map(String::from)
        .collect();
    let mut using = BTreeSet::new();
    for (path, text) in wrela_driver::STD_SOURCES {
        let module = path.strip_prefix("std::").expect("std::");
        let code: String =
            text.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
        if code.contains("unsafe") {
            using.insert(module.to_string());
        }
    }
    // `std::mem` declares the raw operations, which only `unsafe` code may call.
    using.insert("mem".into());
    assert_eq!(using, listed, "§17's unsafe core and the std files that use `unsafe`");
    let compiler: BTreeSet<String> =
        wrela_sema::defs::UNSAFE_CORE.iter().map(|m| m.to_string()).collect();
    assert_eq!(compiler, listed, "§17's unsafe core and the compiler's (`UNSAFE_CORE`)");
}

/// `@effects` and `@thread_entry` are the unsafe core's alone (§17): only its files use them,
/// and the compiler rejects them in a program (and in any other std module) with E0204.
#[test]
fn only_the_core_states_effects_and_thread_entries() {
    let core = wrela_sema::defs::UNSAFE_CORE;
    let mut entries = 0;
    for (path, text) in wrela_driver::STD_SOURCES {
        let module = path.strip_prefix("std::").expect("std::");
        let uses = text.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("@effects") || l.starts_with("@thread_entry")
        });
        if uses {
            assert!(core.contains(&module), "std::{module} uses `@effects` or `@thread_entry`");
        }
        entries += text.lines().filter(|l| l.trim_start().starts_with("@thread_entry")).count();
    }
    assert_eq!(entries, 3, "std's thread entries: the helpers', the voice's and the ticker's");
    let dir = crate::package(
        "closed_list_attrs",
        "@effects(nondet)\nfn now() -> u32 {\n    0\n}\n\n@thread_entry\nfn run(thread: u32) {}\n\npub fn frame(time: f32, width: u32, height: u32) {\n    let _ = now()\n}\n",
    );
    let out = wrela_driver::check(&dir);
    let codes: Vec<&str> = out.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E0204", "E0204"], "{:#?}", out.diagnostics);
}
