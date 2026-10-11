use wrela_host::CpuHost;

#[test]
fn frontend_perf_string_append_preserves_bytes_growth_and_ownership() {
    let dir =
        wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join("frontend-perf-string-append");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch package");
    std::fs::write(
        dir.join("main.wrela"),
        r#"
use std::serialize::{Reader, Writer}

pub fn run() -> u32 {
    var text = String::new()
    text.push_str("")
    assert(text.is_empty())
    text.push_str("a")
    text.push_str("é🦀\0")
    let original = text.clone()
    for _i in 0..128 {
        text.push_str("β")
    }
    assert(text.len() == 264)
    assert(text.as_str().slice(from: 0, to: 8) == "aé🦀\0")
    for i in 0..128 {
        assert(text.as_str().slice(from: 8 + i * 2, to: 10 + i * 2) == "β")
    }
    assert(original == "aé🦀\0")
    text.clear()
    text.push_str(original.as_str().slice(from: 1, to: 7))
    text.push_str("")
    assert(text == "é🦀")
    let suffix = String::from(" τέλος")
    text.push_str(suffix)
    assert(suffix == " τέλος")
    assert(text == "é🦀 τέλος")
    var writer = Writer::new()
    writer.write_str("é🦀\0")
    writer.write_str("")
    let bytes = take writer.into_bytes()
    assert(bytes.len() == 15)
    assert(bytes[0] == 7 && bytes[1] == 0 && bytes[2] == 0 && bytes[3] == 0)
    for i in 0..7 {
        assert(bytes[i + 4] == "é🦀\0".byte(i))
    }
    for i in 11..15 {
        assert(bytes[i] == 0)
    }
    var reader = Reader::new(take bytes)
    assert(reader.read_string().unwrap() == "é🦀\0")
    assert(reader.read_string().unwrap() == "")
    assert(reader.at_end())
    text.len()
}

pub fn frame(time: f32, width: u32, height: u32) {}
"#,
    )
    .expect("write source");
    let built = wrela_tests::build(&dir).expect("build string append regression");
    let out = dir.join("build");
    built.write_to(&out).expect("write build");
    let mut host = CpuHost::load(&out).expect("load string append regression");
    assert_eq!(wrela_tests::one_u32(&mut host, "run", &[]), 17);
}
