//! AC10: every public item of std has a doc comment (`///`, just above it or its attributes),
//! methods of its public impls and traits included: `wrela doc` shows them.

use wrela_diag::FileId;
use wrela_syntax::ast::{ImplMemberKind, ItemKind, TraitMemberKind};

/// Whether a `///` comment ends just above `start` (only whitespace, and attributes, between).
fn documented(text: &str, comments: &[wrela_syntax::Comment], start: u32) -> bool {
    comments.iter().rev().filter(|c| c.span.end <= start).take(1).any(|c| {
        let between = &text[c.span.end as usize..start as usize];
        c.doc && between.trim().is_empty() && between.matches('\n').count() <= 1
    })
}

#[test]
fn every_public_std_item_has_a_doc_comment() {
    let mut missing = Vec::new();
    for (module, text) in wrela_driver::STD_SOURCES {
        let parsed = wrela_syntax::parse(FileId(0), text);
        let at = |offset: u32| 1 + text[..offset as usize].matches('\n').count();
        let mut check = |name: &str, start: u32| {
            if !documented(text, &parsed.comments, start) {
                missing.push(format!("{module}::{name} (line {})", at(start)));
            }
        };
        for item in &parsed.file.items {
            match &item.kind {
                ItemKind::Impl(imp) => {
                    for m in &imp.members {
                        let ImplMemberKind::Fn(f) = &m.kind else { continue };
                        // An inherent impl's `pub` methods; a trait impl's are the trait's.
                        if m.vis.is_some() && imp.trait_.is_none() {
                            check(&f.name.name, m.span.start);
                        }
                    }
                }
                ItemKind::Trait(t) if item.vis.is_some() => {
                    check(&t.name.name, item.span.start);
                    for m in &t.members {
                        if let TraitMemberKind::Fn(f) = &m.kind {
                            check(&format!("{}::{}", t.name.name, f.name.name), m.span.start);
                        }
                    }
                }
                ItemKind::Use(_) | ItemKind::Error(_) => {}
                kind if item.vis.is_some() => {
                    if let Some(name) = kind.name() {
                        check(&name.name, item.span.start);
                    }
                }
                _ => {}
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} public items have no doc comment:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}
