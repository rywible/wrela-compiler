//! After a syntax error the parser keeps going and keeps what it read: a broken statement,
//! argument, field, parameter, arm or item becomes an error node in its place, so the tree
//! still holds every token and later passes can check the rest.

use wrela_diag::FileId;
use wrela_syntax::ast::*;
use wrela_syntax::{TokenKind, lex, parse};

const BROKEN: &str = "\
struct P { x: f32, y: + }

fn f(a: i32, b: ) -> i32 {
    let c = a + * 2
    g(1, ], 3)
    match a {
        0 => 1,
        => 2,
        _ => 3,
    }
}

fn h<(x: i32) {}

const K = 1
";

fn fn_named<'a>(file: &'a File, name: &str) -> &'a FnDecl {
    file.items
        .iter()
        .find_map(|i| match &i.kind {
            ItemKind::Fn(f) if f.name.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fn `{name}`"))
}

#[test]
fn errors_become_nodes_in_place() {
    let p = parse(FileId(0), BROKEN);
    assert!(p.has_errors());
    let codes: Vec<&str> = p.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes.len(), 6, "{:#?}", p.diagnostics);

    // Every item is there; the one whose header broke keeps its name.
    let kinds: Vec<String> = p
        .file
        .items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Error(n) => format!("error {}", n.as_ref().map_or("?", |n| &n.name)),
            k => k.name().map_or("?".into(), |n| n.name.clone()),
        })
        .collect();
    assert_eq!(kinds, ["P", "f", "error h", "K"]);

    let ItemKind::Struct(s) = &p.file.items[0].kind else { panic!("not a struct") };
    assert_eq!(s.fields.len(), 2);
    assert!(matches!(s.fields[1].ty.kind, TypeExprKind::Error));

    let f = fn_named(&p.file, "f");
    assert_eq!(f.params.len(), 2);
    assert!(
        matches!(&f.params[1], Param::Named { ty, .. } if matches!(ty.kind, TypeExprKind::Error))
    );
    let body = &f.body.as_ref().expect("a body").stmts;
    assert_eq!(body.len(), 3);
    // The binding stays, with an error for its value.
    assert!(matches!(
        &body[0].kind,
        StmtKind::Let { init: Expr { kind: ExprKind::Error, .. }, .. }
    ));
    // The call keeps its good arguments.
    let StmtKind::Expr(Expr { kind: ExprKind::Call { args, .. }, .. }) = &body[1].kind else {
        panic!("not a call: {:?}", body[1].kind)
    };
    let shapes: Vec<bool> = args.iter().map(|a| matches!(a.value.kind, ExprKind::Error)).collect();
    assert_eq!(shapes, [false, true, false]);
    // The broken arm matches anything, between the two good ones.
    let StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) = &body[2].kind else {
        panic!("not a match")
    };
    assert_eq!(arms.len(), 3);
    assert!(matches!(arms[1].pats[0].kind, PatKind::Error));
}

/// Every token but separators lies inside an item, so nothing the parser skipped is lost
/// from the tree.
fn assert_tokens_covered(name: &str, text: &str) {
    let p = parse(FileId(0), text);
    let lexed = lex(FileId(0), text);
    for t in &lexed.tokens {
        if matches!(t.kind, TokenKind::Newline | TokenKind::Semi | TokenKind::Eof) {
            continue;
        }
        let inside =
            p.file.items.iter().any(|i| i.span.start <= t.span.start && t.span.end <= i.span.end);
        assert!(inside, "{name}: the token at {}..{} is in no item", t.span.start, t.span.end);
    }
}

#[test]
fn no_token_is_dropped() {
    assert_tokens_covered("BROKEN", BROKEN);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests");
    let mut checked = 0;
    for dir in ["conformance", "diagnostics"] {
        let mut paths: Vec<_> = std::fs::read_dir(root.join(dir))
            .expect("the cases")
            .map(|e| e.expect("an entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "wrela"))
            .collect();
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path).expect("read");
            assert_tokens_covered(&path.display().to_string(), &text);
            checked += 1;
        }
    }
    assert!(checked > 100, "only {checked} files");
}
