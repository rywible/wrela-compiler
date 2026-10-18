use super::*;
use crate::token::TokenKind as T;

#[test]
fn frontend_perf_punctuation_dispatch_preserves_longest_match() {
    fn check(text: &str) {
        let lexer = Lexer {
            file: FileId(0),
            src: text.as_bytes(),
            text,
            pos: 0,
            out: Lexed::default(),
            holes: Vec::new(),
        };
        let expected = TokenKind::PUNCT
            .iter()
            .copied()
            .find(|k| text.starts_with(k.fixed_text().unwrap()))
            .filter(|k| {
                *k != TokenKind::Underscore
                    || !text.chars().nth(1).is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
        assert_eq!(lexer.punct(), expected, "{text:?}");
    }
    for &kind in TokenKind::PUNCT {
        let text = kind.fixed_text().unwrap();
        assert!(text.is_ascii() && text.len() <= 3, "{text:?}");
        check(text);
        for &next in TokenKind::PUNCT {
            check(&format!("{text}{}", next.fixed_text().unwrap()));
        }
        for suffix in ["name", "42", " ", "\n", "é", "αβ", "🦀"] {
            check(&format!("{text}{suffix}"));
        }
    }
    for byte in 0..=127u8 {
        for suffix in ["", "=", ">=", "ab", "_", "é", "🦀"] {
            check(&format!("{}{suffix}", char::from(byte)));
        }
    }
    for text in ["", "é", "🦀", "ident"] {
        check(text);
    }
}

#[test]
fn frontend_perf_literal_decoding_preserves_values_and_float_rounding() {
    for text in ["42", "0xff", "18446744073709551615", "1.5", "1e-300", "1.5f32"] {
        assert!(matches!(number_digits(text), Cow::Borrowed(_)), "{text}");
    }
    for text in ["4_2", "0xf_f", "1_000u32", "1.000_000_059_604_644_8"] {
        assert!(matches!(number_digits(text), Cow::Owned(_)), "{text}");
    }
    let ints = ["42", "0xff", "18446744073709551615", "4_2", "0xf_f", "1_000u32"];
    for text in ints {
        let clean: String = without_type_suffix(text).chars().filter(|c| *c != '_').collect();
        let expected = if let Some(digits) = clean.strip_prefix("0x") {
            u64::from_str_radix(digits, 16).unwrap()
        } else {
            clean.parse().unwrap()
        };
        assert_eq!(int_value(text), IntValue::Ok(expected));
    }
    for text in [
        "1.5",
        "1_000.0",
        "1e-300",
        "1.5f32",
        "1.0000000596046448",
        "1.000_000_059_604_644_8",
        "invalid",
    ] {
        let clean: String = without_type_suffix(text).chars().filter(|c| *c != '_').collect();
        let expected64: f64 = clean.parse().unwrap_or(f64::NAN);
        let expected32: f32 = clean.parse().unwrap_or(f32::NAN);
        assert_eq!(float_value(text).to_bits(), expected64.to_bits(), "{text}");
        assert_eq!(float_value_f32(text).to_bits(), expected32.to_bits(), "{text}");
    }
    // This decimal is above an f32 midpoint but rounds to that midpoint in f64.
    assert_ne!(float_value_f32("1.0000000596046448"), float_value("1.0000000596046448") as f32);
}

fn kinds(src: &str) -> Vec<TokenKind> {
    lex(FileId(0), src).tokens.iter().map(|t| t.kind).collect()
}

fn codes_of(src: &str) -> Vec<&'static str> {
    lex(FileId(0), src).diagnostics.iter().map(|d| d.code.as_str()).collect()
}

fn texts(src: &str) -> Vec<String> {
    lex(FileId(0), src).tokens.iter().map(|t| src[t.span.range()].to_string()).collect()
}

#[test]
fn l10_l11_identifiers_and_keywords() {
    assert_eq!(
        kinds("fn foo _ _x Self self"),
        [T::Fn, T::Ident, T::Underscore, T::Ident, T::SelfType, T::SelfValue, T::Eof]
    );
    assert_eq!(kinds("f32 vec3"), [T::Ident, T::Ident, T::Eof]);
}

#[test]
fn l3_non_ascii_identifier_is_one_token_with_one_error() {
    let l = lex(FileId(0), "let größe = 1");
    assert_eq!(l.tokens.len(), 5);
    assert_eq!(l.diagnostics.len(), 1);
    assert_eq!(l.diagnostics[0].code.as_str(), "E0001");

    // A bad character immediately before the name must not consume its non-ASCII start.
    assert_eq!(texts("$éx"), ["éx", ""]);
    assert_eq!(codes_of("$éx"), ["E0001", "E0001"]);

    // `_` is a wildcard only on its own; a non-ASCII name keeps its leading underscore too.
    for name in ["_é", "_αβ", "_²"] {
        assert_eq!(texts(name), [name, ""], "{name}");
        assert_eq!(kinds(name), [T::Ident, T::Eof], "{name}");
        assert_eq!(codes_of(name), ["E0001"], "{name}");
    }
}

#[test]
fn l3_bad_characters_are_one_error_per_run() {
    assert_eq!(codes_of("a $$ b # c"), ["E0001", "E0001"]);
    assert_eq!(codes_of("'x'"), ["E0001", "E0001"]);
    assert_eq!(kinds("a ~ b"), [T::Ident, T::Ident, T::Eof]);
}

#[test]
fn l3_skipped_characters_keep_the_line_break() {
    // Indented with no-break spaces, as text pasted from the web often is.
    let src = "{\n\u{a0}\u{a0}let a = 1\n\u{a0}\u{a0}let b = a\n}";
    assert_eq!(
        kinds(src),
        [
            T::LBrace,
            T::Let,
            T::Ident,
            T::Eq,
            T::Int,
            T::Newline,
            T::Let,
            T::Ident,
            T::Eq,
            T::Ident,
            T::Newline,
            T::RBrace,
            T::Eof
        ]
    );
    assert_eq!(codes_of(src), ["E0001", "E0001"]);
    assert_eq!(kinds("a\n# b"), [T::Ident, T::Newline, T::Ident, T::Eof]);
    // A comment after skipped characters only is on its own line.
    assert!(lex(FileId(0), "a\n\u{a0}// c\nb").comments[0].own_line);
}

#[test]
fn l2_bom_is_skipped() {
    assert_eq!(kinds("\u{feff}fn"), [T::Fn, T::Eof]);
}

#[test]
fn l4_lone_cr_is_error_and_line_break() {
    assert_eq!(codes_of("a\rb"), ["E0001"]);
    assert_eq!(kinds("a\rb"), [T::Ident, T::Newline, T::Ident, T::Eof]);
    assert_eq!(kinds("a\r\nb"), [T::Ident, T::Newline, T::Ident, T::Eof]);
}

#[test]
fn l6_l7_comments_are_trivia() {
    let l = lex(FileId(0), "a // x\n/// doc\n//// not doc\nb");
    assert_eq!(
        l.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        [T::Ident, T::Newline, T::Ident, T::Eof]
    );
    assert_eq!(l.comments.len(), 3);
    assert!(!l.comments[0].doc && !l.comments[0].own_line);
    assert!(l.comments[1].doc && l.comments[1].own_line);
    assert!(!l.comments[2].doc);
}

#[test]
fn l9_block_comments_are_errors_with_a_fix() {
    let l = lex(FileId(0), "a /* x */\nb");
    assert_eq!(l.diagnostics.len(), 1);
    assert_eq!(l.diagnostics[0].code.as_str(), "E0002");
    assert_eq!(l.diagnostics[0].fixes[0].edits[0].replacement, "// x");
    let l = lex(FileId(0), "a /* x\n y */ b");
    assert!(l.diagnostics[0].fixes.is_empty());
    assert_eq!(codes_of("/* /* nested */ */ a"), ["E0002"]);
    assert_eq!(kinds("/* /* nested */ */ a"), [T::Ident, T::Eof]);
}

#[test]
fn l12_l13_numbers() {
    assert_eq!(
        kinds("1 1.5 1e5 1.5e-3 0xff 0b101 0o17 1_000"),
        [T::Int, T::Float, T::Float, T::Float, T::Int, T::Int, T::Int, T::Int, T::Eof]
    );
    assert_eq!(texts("1..5"), ["1", "..", "5", ""]);
    assert_eq!(texts("t.0.1"), ["t", ".", "0", ".", "1", ""]);
    assert_eq!(texts("1.x"), ["1", ".", "x", ""]);
    assert_eq!(texts("1. 5"), ["1", ".", "5", ""]);
    assert_eq!(kinds("15cm 2m"), [T::Suffixed, T::Suffixed, T::Eof]);
    assert_eq!(kinds("1e+5"), [T::Float, T::Eof]);
    // An exponent or a unit takes a `.` and a digit like any decimal number: the rest of the
    // text is then malformed (E0004), not a field access.
    for bad in ["1e5.5", "1E5.0", "1kelvin.5", "1cm.5"] {
        assert_eq!(texts(bad), [bad, ""], "{bad}");
        assert_eq!(codes_of(bad), ["E0004"], "{bad}");
    }
    assert_eq!(texts("1e5.max(x)"), ["1e5", ".", "max", "(", "x", ")", ""]);
}

#[test]
fn l13_type_suffixes_are_e0003_with_a_fix() {
    let l = lex(FileId(0), "1.0f32");
    assert_eq!(l.diagnostics[0].code.as_str(), "E0003");
    assert_eq!(l.diagnostics[0].fixes[0].edits[0].replacement, "1.0");
    assert_eq!(l.tokens[0].kind, T::Float);
    assert_eq!(codes_of("7_u32"), ["E0003"]);
    // `e` and `f` are hex digits: these are INTs with a type suffix, and they have values.
    for (src, value) in [("0x1eu32", 0x1e), ("0xEu8", 0xe), ("0xffu32", 0xff), ("0x1f_i64", 0x1f)] {
        let l = lex(FileId(0), src);
        assert_eq!(l.tokens[0].kind, T::Int, "{src}");
        assert_eq!(codes_of(src), ["E0003"], "{src}");
        assert_eq!(int_value(src), IntValue::Ok(value), "{src}");
    }
    assert_eq!(int_value("0x1f32"), IntValue::Ok(0x1f32));
    assert_eq!(int_value("255u32"), IntValue::Ok(255));
    assert_eq!(float_value("1.5f32"), 1.5);
    assert_eq!(float_value("1e2f64"), 100.0);
}

#[test]
fn l13_malformed_numbers_are_e0004() {
    for bad in ["0x", "0b2", "0X1F", "1e", "2.5e+", "1.5e", "0b_", "1e_+5", "1e+_5", "0o9"] {
        let c = codes_of(bad);
        assert!(c.first() == Some(&"E0004"), "{bad}: {c:?}");
    }
}

#[test]
fn l14_strings() {
    assert_eq!(kinds(r#""a\n\"b""#), [T::Str, T::Eof]);
    assert_eq!(codes_of("\"abc"), ["E0005"]);
    assert_eq!(codes_of(r#""\q""#), ["E0005"]);

    // An unknown escape at the end of a line must not hide the line break from the string.
    let src = "\"a\\\nnext";
    assert_eq!(kinds(src), [T::Str, T::Newline, T::Ident, T::Eof]);
    assert_eq!(texts(src), ["\"a\\", "", "next", ""]);
    assert_eq!(codes_of(src), ["E0005", "E0005"]);
}

#[test]
fn l15_longest_match() {
    assert_eq!(
        kinds("**= <<= >>= ..= :: -> => ** .."),
        [
            T::StarStarEq,
            T::ShlEq,
            T::ShrEq,
            T::DotDotEq,
            T::ColonColon,
            T::Arrow,
            T::FatArrow,
            T::StarStar,
            T::DotDot,
            T::Eof
        ]
    );
    assert_eq!(kinds("a&&b||c"), [T::Ident, T::AndAnd, T::Ident, T::OrOr, T::Ident, T::Eof]);
}

#[test]
fn l17_newline_conditions() {
    // Condition 1: not inside ( or [.
    assert_eq!(kinds("f(a\nb)"), [T::Ident, T::LParen, T::Ident, T::Ident, T::RParen, T::Eof]);
    assert_eq!(kinds("{a\nb}"), [T::LBrace, T::Ident, T::Newline, T::Ident, T::RBrace, T::Eof]);
    // Condition 2: the previous token can end a statement.
    assert_eq!(kinds("a +\nb"), [T::Ident, T::Plus, T::Ident, T::Eof]);
    assert_eq!(kinds("a\n+b"), [T::Ident, T::Newline, T::Plus, T::Ident, T::Eof]);
    // Condition 3: the next token isn't `.`.
    assert_eq!(kinds("a\n.b()"), [T::Ident, T::Dot, T::Ident, T::LParen, T::RParen, T::Eof]);
    assert_eq!(kinds("a\n..b"), [T::Ident, T::Newline, T::DotDot, T::Ident, T::Eof]);
    // Comments between don't count.
    assert_eq!(kinds("a // c\n// d\n.b"), [T::Ident, T::Dot, T::Ident, T::Eof]);
}

#[test]
fn l18_one_newline_per_gap_none_at_start() {
    assert_eq!(kinds("\n\na\n\n\nb\n"), [T::Ident, T::Newline, T::Ident, T::Newline, T::Eof]);
}

#[test]
fn l19_brackets_close_innermost_of_their_kind() {
    // `)` closes the `(` and the `{` opened after it, so no bracket is open at the line break,
    // and it makes a NEWLINE.
    assert_eq!(
        kinds("(a{b)\nc"),
        [T::LParen, T::Ident, T::LBrace, T::Ident, T::RParen, T::Newline, T::Ident, T::Eof]
    );
    // An unmatched closer changes nothing.
    assert_eq!(kinds("(a]\nb)"), [T::LParen, T::Ident, T::RBracket, T::Ident, T::RParen, T::Eof]);
}

#[test]
fn int_and_float_values() {
    assert_eq!(int_value("1_000"), IntValue::Ok(1000));
    assert_eq!(int_value("0xff"), IntValue::Ok(255));
    assert_eq!(int_value("0b101"), IntValue::Ok(5));
    assert_eq!(int_value("0o17"), IntValue::Ok(15));
    assert_eq!(int_value("7_u32"), IntValue::Ok(7));
    assert_eq!(int_value("99999999999999999999999"), IntValue::TooLarge);
    assert_eq!(int_value("0x"), IntValue::Malformed);
    assert_eq!(float_value("1.5e-3"), 1.5e-3);
}

/// A unit suffix starts after the whole number, exponent and radix digits included, so a fix
/// that drops the unit keeps the number (`1e-3m` is `1e-3`, not `1`).
#[test]
fn a_suffix_starts_after_the_whole_number() {
    for (text, number, unit) in [
        ("1e-3m", "1e-3", "m"),
        ("2.5e3kg", "2.5e3", "kg"),
        ("0x1fcm", "0x1fc", "m"),
        ("10_000ms", "10_000", "ms"),
        ("3m", "3", "m"),
    ] {
        assert_eq!(super::split_suffix(text), (number, unit), "{text}");
    }
}

/// spec/lexical.md's keyword list (L11) is the lexer's, word for word and in order.
#[test]
fn the_keyword_list_is_the_specs() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/lexical.md");
    let spec = std::fs::read_to_string(path).expect("spec/lexical.md");
    let l11 = spec.find("**L11.**").expect("L11 in spec/lexical.md");
    let block = &spec[l11..];
    let start = block.find("```").expect("L11's code block") + 3;
    let end = start + block[start..].find("```").expect("the end of L11's code block");
    let listed: Vec<&str> = block[start..end].split_whitespace().collect();
    let lexer: Vec<&str> = T::KEYWORDS.iter().filter_map(|k| k.fixed_text()).collect();
    assert_eq!(listed, lexer, "L11's keywords and the lexer's differ");
    for word in &listed {
        assert_eq!(T::keyword(word).map(|k| k.fixed_text()), Some(Some(*word)), "{word}");
    }
}

/// L22: an f-string splits at its holes, a format spec ends a hole, brackets nest in one, and
/// `{{` `}}` are text.
#[test]
fn fstrings_split_at_their_holes() {
    assert_eq!(kinds(r#"f"a""#), [T::FString, T::Eof]);
    assert_eq!(kinds(r#"f"{{x}}""#), [T::FString, T::Eof]);
    assert_eq!(kinds(r#"f"w: {w:.1} kg""#), [T::FStringHead, T::Ident, T::FStringTail, T::Eof]);
    assert_eq!(
        kinds(r#"f"{a}, {b}""#),
        [T::FStringHead, T::Ident, T::FStringMid, T::Ident, T::FStringTail, T::Eof]
    );
    // `::` and brackets don't end a hole; a nested f-string is its own.
    assert_eq!(
        kinds(r#"f"{a::b(c: d)}""#),
        [
            T::FStringHead,
            T::Ident,
            T::ColonColon,
            T::Ident,
            T::LParen,
            T::Ident,
            T::Colon,
            T::Ident,
            T::RParen,
            T::FStringTail,
            T::Eof
        ]
    );
    assert_eq!(
        kinds(r#"f"{f"{x}"}""#),
        [T::FStringHead, T::FStringHead, T::Ident, T::FStringTail, T::FStringTail, T::Eof]
    );
    // F-string holes follow L19's typed bracket recovery, not scalar nesting depth: `)` closes
    // the `(` and the `[` opened after it, so the following `}` ends the hole.
    assert_eq!(
        kinds(r#"f"{([1) }""#),
        [T::FStringHead, T::LParen, T::LBracket, T::Int, T::RParen, T::FStringTail, T::Eof]
    );
    assert!(codes_of(r#"f"{([1) }""#).is_empty());

    // An unmatched closer changes nothing, just as it does outside an f-string hole.
    assert_eq!(
        kinds(r#"f"{(] : 1) }""#),
        [
            T::FStringHead,
            T::LParen,
            T::RBracket,
            T::Colon,
            T::Int,
            T::RParen,
            T::FStringTail,
            T::Eof
        ]
    );
    assert!(codes_of(r#"f"{(] : 1) }""#).is_empty());

    let src = "f\"a\\\nnext";
    assert_eq!(kinds(src), [T::FString, T::Newline, T::Ident, T::Eof]);
    assert_eq!(texts(src), ["f\"a\\", "", "next", ""]);
    assert_eq!(codes_of(src), ["E0005", "E0005"]);
    let (spec, text) = super::fstring_segment(":>8.2}, kg{{\"x\"}}{");
    assert_eq!((spec.as_deref(), text.as_str()), (Some(">8.2"), ", kg{\"x\"}"));
    assert_eq!(super::string_value(r#""a\tb\"""#), "a\tb\"");
    for bad in [r#"f"{x"#, "f\"{x\n}\"", r#"f"a}b""#, r#"f"{x:.2"#] {
        let lexed = super::lex(FileId(0), bad);
        assert!(lexed.diagnostics.iter().any(|d| d.code.as_str() == "E0005"), "{bad}");
    }
}
