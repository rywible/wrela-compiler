//! Regression cases from the lowering audit.

#[test]
fn debug_constants_check_nan_creation() {
    let dir = crate::package(
        "lower-constants-sweep/debug-nans",
        r#"
const VALUE: f32 = divide(0.0, 0.0)

fn divide(a: f32, b: f32) -> f32 { a / b }

pub fn read() -> f32 { VALUE }
"#,
    );
    let release = wrela_driver::build(&dir);
    assert!(!release.has_errors(), "{:?}", release.diagnostics);
    let debug = wrela_driver::build_debug(&dir);
    let errors: Vec<_> = debug.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{:?}", debug.diagnostics);
    assert_eq!(errors[0].code.as_str(), "E0704");
    assert!(errors[0].message.contains("a float operation created a NaN"), "{:?}", errors[0]);
}

#[test]
fn computed_box_constants_preserve_presence() {
    let dir = crate::package(
        "lower-constants-sweep/box-presence",
        r#"
use std::serialize::{LoadError, Serialize, load, save}

struct Empty: Copy + Serialize {}

const EMPTY_BYTE: Box<u8> = empty_box()
const EMPTY_VEC: Box<Vec<u32>> = empty_box()
const EMPTY_UNIT: Box<Empty> = empty_box()
const FULL_UNIT: Box<()> = Box::new(())
const FULL_EMPTY_STRUCT: Box<Empty> = Box::new(Empty {})

fn empty_box<T: Serialize>() -> Box<T> {
    let loaded: Result<Box<T>, LoadError> = load(Vec::from([0]))
    take loaded.unwrap()
}

@test
fn presence() {
    assert(EMPTY_BYTE.is_empty())
    assert(EMPTY_VEC.is_empty())
    assert(EMPTY_UNIT.is_empty())
    assert(!FULL_UNIT.is_empty())
    assert(!FULL_EMPTY_STRUCT.is_empty())
    assert(FULL_UNIT.get() == ())
    assert(save(EMPTY_BYTE) == Vec::from([0]))
    assert(save(EMPTY_VEC) == Vec::from([0]))
    assert(save(EMPTY_UNIT) == Vec::from([0]))
    assert(save(FULL_EMPTY_STRUCT) == Vec::from([1]))
}
"#,
    );
    assert_eq!(crate::tests_pass(&dir), 1);
    assert_eq!(crate::tests_pass(&dir), 1, "cached constants preserve the same presence");
}

#[test]
fn computed_values_obey_the_aggregate_depth_limit() {
    let chain = r#"
enum Chain {
    End,
    Next(Box<Chain>),
}

fn chain(n: u32) -> Chain {
    var next = Chain::End
    for _ in 0..n {
        next = Chain::Next(Box::new(take next))
    }
    next
}
"#;
    // One outer box, 127 pairs of enum and box, and the terminal enum: 256 aggregates.
    let accepted = crate::package(
        "lower-constants-sweep/depth-limit",
        &format!(
            "{chain}\nconst CHAIN: Box<Chain> = Box::new(chain(127))\n\
             pub fn kept() -> bool {{ !CHAIN.is_empty() }}"
        ),
    );
    let built = wrela_driver::build(&accepted);
    assert!(!built.has_errors(), "{:?}", built.diagnostics);
    assert!(built.consts.computed > 0);
    let cached = wrela_driver::build(&accepted);
    assert!(!cached.has_errors(), "{:?}", cached.diagnostics);
    assert_eq!(cached.consts.computed, 0);
    assert!(cached.consts.cached > 0);

    // 128 pairs and the terminal enum: one aggregate beyond the limit.
    let rejected = crate::package(
        "lower-constants-sweep/depth-over-limit",
        &format!(
            "{chain}\nconst CHAIN: Chain = chain(128)\n\
             pub fn kept() -> bool {{ match CHAIN {{ Chain::End => true, Chain::Next(_) => false }} }}"
        ),
    );
    let built = wrela_driver::build(&rejected);
    let errors: Vec<_> = built.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{:?}", built.diagnostics);
    assert_eq!(errors[0].code.as_str(), "E0704");
    assert!(errors[0].message.contains("256 aggregates"), "{:?}", errors[0]);
}
