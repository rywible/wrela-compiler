//! Console policy is Wrela; these small fixtures exercise data, input and save validation.
#[test]
fn console_and_platform_policy_tests_pass() {
    for package in ["console", "platform", "games/last-green/session", "ui"] {
        assert!(super::tests_pass(&wrela_tests::repo_root().join(package)) > 0, "{package}");
    }
}

#[test]
fn exporting_a_recovered_save_acknowledges_only_a_successful_reply() {
    let root = wrela_tests::repo_root();
    let dir = super::scratch("console-recovery-export");
    wrela_tests::copy_package(&root.join("games/last-green/session"), &dir);
    let mut source = std::fs::read_to_string(dir.join("main.wrela")).expect("session source");
    source.push_str(
        r#"
pub fn fixture_export(s: mut Session, other_slot: u32) {
    s = failed_recovery_fixture()
    let slot = if other_slot == 0 { "continue" } else { "imported" }
    let request = Request::Export { profile: s.active.clone(), slot: slot.to_string() }
    begin(mut s, request)
    if s.phase == Phase::ReadSlot {
        let file = SaveFile {
            game: String::from(GAME),
            version: 1,
            saved: 17,
            body: s.recovered.clone(),
        }
        answered(mut s, Ok(save(file)))
    }
    assert(s.phase == Phase::Reply)
}

pub fn fixture_reply(s: mut Session, succeeded: u32) -> u32 {
    let recovered = s.recovered.clone()
    let exported_recovery = match s.request {
        Some(Request::Export { profile, slot }) => profile == s.active && slot == "continue",
        _ => false,
    }
    if succeeded != 0 {
        answered(mut s, Ok(Vec::new()))
    } else {
        var reply = None
        match mut s.io {
            Some(p) => reply = p.poll(),
            None => {},
        }
        let Some(reply) = reply else { return 0 }
        assert(reply.is_err(), "the CPU host has no parent channel")
        answered(mut s, reply)
    }
    assert(s.phase == Phase::Idle)
    assert(s.recovered == recovered, "export must preserve recovery bytes")
    let request = Request::Prepare { profile: s.active.clone(), slot: String::new() }
    begin(mut s, request)
    if succeeded != 0 && exported_recovery {
        if s.phase == Phase::Backup { 1 } else { 0 }
    } else {
        if s.phase == Phase::Reply { 1 } else { 0 }
    }
}

pub fn fixture_backup(s: mut Session) {
    assert(s.phase == Phase::Backup)
    let stale = SaveFile {
        game: String::from(GAME),
        version: 1,
        saved: 17,
        body: save(
            CarryFixture {
                tower: engine::tower::Tower::new(2),
                at: 0,
                door: STAIR,
                going: 0xFFFFFFFE,
            },
        ),
    }
    answered(mut s, Ok(save(stale)))
    assert(s.phase == Phase::BackupSaved)
    let expected = SaveFile {
        game: String::from(GAME),
        version: 1,
        saved: s.saved,
        body: s.recovered.clone(),
    }
    let _ = std::host::request("fixture/expected", save(expected))
}
"#,
    );
    std::fs::write(dir.join("main.wrela"), source).expect("state fixture");
    let build = dir.join("build");
    wrela_tests::must_build(&dir, &build);
    let compiled = wrela_host::CpuBuild::load(&build).expect("CPU session build").recording();
    for (other_slot, succeeded) in [(0, 0), (0, 1), (1, 1)] {
        let mut host = compiled.start().expect("CPU session host");
        host.call_export("fixture_export", &[wrela_host::Value::I32(other_slot)])
            .expect("export recovery");
        assert_eq!(
            wrela_tests::one_u32(&mut host, "fixture_reply", &[wrela_host::Value::I32(succeeded)],),
            1,
            "other slot={other_slot}, successful reply={succeeded}",
        );
        if other_slot == 0 && succeeded == 1 {
            host.take_batches();
            host.call_export("fixture_backup", &[]).expect("back up before new game");
            let mut backup = Vec::new();
            let mut expected = Vec::new();
            for batch in host.take_batches() {
                for command in wrela_abi::stream::decode(&batch).expect("valid session commands") {
                    match command {
                        wrela_abi::stream::Command::StorageWrite {
                            path: "profiles/player-1/previous",
                            data,
                            ..
                        } => {
                            backup.push(data.to_vec());
                        }
                        wrela_abi::stream::Command::HostRequest {
                            service: "fixture/expected",
                            body,
                            ..
                        } => {
                            expected.push(body.to_vec());
                        }
                        _ => {}
                    }
                }
            }
            assert_eq!(expected.len(), 1, "one current recovery envelope");
            assert_eq!(backup, expected, "New game backs up the current recovered continuation");
        }
    }
}
