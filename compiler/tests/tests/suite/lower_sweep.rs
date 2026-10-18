//! Regressions found while reviewing lowering's current code.

#[test]
fn exported_entry_points_obey_their_role() {
    for (name, source) in [
        ("client", "@server\npub fn frame(time: f32, width: u32, height: u32) {}\n"),
        ("server", "@client\npub fn serve(time: f32) {}\n"),
    ] {
        let dir = super::scratch(&format!("lower-sweep/entry-role-{name}"));
        std::fs::write(dir.join("main.wrela"), source).expect("write source");
        let checked = wrela_driver::check(&dir);
        let errors: Vec<_> = checked.diagnostics.iter().filter(|d| d.is_error()).collect();
        assert_eq!(errors.len(), 1, "{:?}", checked.diagnostics);
        assert_eq!(errors[0].code, wrela_diag::codes::E0708);
        assert!(errors[0].message.contains("runs only in a"));
    }
}

#[test]
fn exports_cannot_take_the_hosts_state() {
    let dir = super::package(
        "lower-sweep/taken-state",
        "struct State { stuff: Vec<u32> }

pub fn init() -> State { State { stuff: Vec::from([7]) } }

pub fn frame(state: mut State, time: f32, width: u32, height: u32) {}

pub fn consume(state: take State) -> u32 { state.stuff.len() }
",
    );
    let checked = wrela_driver::check(&dir);
    let errors: Vec<_> = checked.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{:?}", checked.diagnostics);
    assert_eq!(errors[0].code, wrela_diag::codes::E0703);
    assert!(errors[0].message.contains("must be borrowed or `mut`, not `take`"));
}

#[test]
fn named_function_arguments_keep_their_role() {
    let dir = super::package(
        "lower-sweep/function-argument-role",
        "@server
fn server_only(x: u32) -> u32 { x }

fn apply(f: fn(u32) -> u32, x: u32) -> u32 { f(x) }

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = apply(server_only, width)
}
",
    );
    let checked = wrela_driver::check(&dir);
    let errors: Vec<_> = checked.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{:?}", checked.diagnostics);
    assert_eq!(errors[0].code, wrela_diag::codes::E0708);
    assert!(errors[0].message.contains("runs only in a server build"));
}

#[test]
fn job_closures_keep_copies_from_their_creation() {
    let dir = super::package(
        "lower-sweep/job-closure-copies",
        "use std::job::Step

fn apply(f: fn() -> u32) -> u32 { f() }

@job
fn compute(seed: take u32) -> u32 {
    var n = seed
    let captured = || n
    n += 1
    yield
    assert(captured() == apply(captured))
    captured()
}

@test
fn keeps_copied_capture() {
    var job = compute.start(7)
    match job.resume() {
        Step::Yielded => {},
        Step::Done(_) => panic(\"the job must yield first\"),
    }
    match job.resume() {
        Step::Yielded => panic(\"the job must finish\"),
        Step::Done(n) => assert(n == 7),
    }
}
",
    );
    assert_eq!(super::tests_pass(&dir), 1);
}
