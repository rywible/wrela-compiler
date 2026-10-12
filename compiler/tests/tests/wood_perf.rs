//! A small streamed-wood selection: real kernels and retained buffers through list changes.

use wrela_abi::stream::{Command, decode};
use wrela_host::{Host, Options, Value};

const FIXTURE: &str = r#"
pub struct Fixture {
    kinds: Vec<Kind>,
    chosen: Chosen,
    every: GpuBuffer<Instance>,
    metadata: GpuBuffer<u32>,
    metadata_values: Vec<u32>,
    lods: GpuBuffer<Lod>,
    depth: DepthTexture,
}

fn fixture_kind() -> Kind {
    let leaves = engine::plant::Leaves {}
    let bark = engine::plant::Bark {}
    Kind {
        plant: engine::plant::plant(leaves, bark),
        cards: buffer(1),
        card_count: 8,
        orders: buffer(1),
        limbs: buffer(1),
        limb_count: 1,
        top: 2.0,
        reach: 1.0,
        impostor: engine::impostor::Impostor {
            colour: std::gpu::texture::<Rgba8>(1, 1),
            normal: std::gpu::texture::<std::gpu::Rgba16Float>(1, 1),
            bake: engine::impostor::Bake { mid: vec3(0.0, 1.0, 0.0), half: 1.0, leaves, bark },
            bounds: buffer(1),
        },
        volume: engine::impostor::Cooked {
            grid: std::gpu::texture3d::<std::gpu::Rgba16Float>(1, 1, 1),
            grid_depth: std::gpu::texture3d::<std::gpu::Rgba16Float>(1, 1, 1),
        },
    }
}

pub fn init() -> Fixture {
    let kinds = Vec::from_fn(1, |_| fixture_kind())
    var chosen = Chosen::empty()
    chosen.lay_out(kinds, [u32(0); 0], [73], WAYS)
    let every = buffer_of(Vec::from_fn(256, |i| Instance {
        at: vec3(f32(i), 0.0, 10.0), scale: 1.0, ..NO_INSTANCE
    }))
    let metadata_values = Vec::filled(256, NO_KIND)
    let metadata = buffer_of(metadata_values)
    let lods = buffer_of([Lod {
        cards_to: 80.0, cards_limit: 80.0, impostor_px: 128.0, impostors_to: 4000.0,
        leaf: 0.1, top: 2.0, reach: 1.0, card_px: 12.0, cell: 1.0,
    }])
    Fixture {
        kinds, chosen, every, metadata, metadata_values, lods,
        depth: std::gpu::depth_texture(1, 1),
    }
}

pub fn frame(state: mut Fixture, time: f32, width: u32, height: u32) {
    let n = [73, 3, 2, 0, 40, 201][u32(time)]
    // The third frame selects fewer instances on the same layout. Its old counts must
    // be reset by run, rather than by a newly created count buffer in lay_out.
    if u32(time) != 2 {
        state.chosen.lay_out(state.kinds, [u32(0); 0], [n], WAYS)
    }
    if u32(time) == 1 {
        // Poison room outside the shrunken lists. No dispatch or draw may consume it.
        let poison = Instance { at: vec3(-999.0), scale: 777.0, ..NO_INSTANCE }
        write(mut state.chosen.instances, 160, [poison; 32])
        write(mut state.chosen.drawn, 160, [poison; 32])
    }
    for i in 0..256 {
        state.metadata_values[i] = if i < n { 0 } else { NO_KIND }
    }
    write(mut state.metadata, 0, state.metadata_values)
    let cam = engine::view::first_camera(vec3(0.0), vec3(0.0, 0.0, 1.0), 1.0, 1, 1)
    let c = Choose {
        cam, corner: 1.0, kinds: 1, shadow: 1, instances: 256,
        square: vec4(-1000.0, -1000.0, 1000.0, 1000.0),
    }
    let before = std::alloc::allocations()
    state.chosen.run(c, state.every, state.metadata, state.lods)
    state.chosen.occlude(cam, state.depth, state.every, state.lods)
    assert(std::alloc::allocations() == before, "selection allocated its count reset")
}

pub fn places(state: Fixture) -> u32 { state.chosen.places }
pub fn capacity(state: Fixture) -> u32 { state.chosen.instances.len() }
"#;

fn fixture() -> std::path::PathBuf {
    let root = wrela_tests::repo_root();
    let dir = wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join("wood-selection-perf");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fixture directory");
    // The production module is the fixture's main module so the test can drive Chosen's
    // private lifecycle directly. Only module paths change; all kernels and methods stay.
    let mut wood = std::fs::read_to_string(root.join("engine/wood.wrela")).expect("wood source");
    for module in ["impostor", "lit", "lod", "plant", "shadow", "vegetation", "view", "noise"] {
        wood = wood.replace(&format!("{module}::"), &format!("engine::{module}::"));
    }
    std::fs::write(dir.join("main.wrela"), format!("{wood}\n{FIXTURE}")).expect("fixture source");
    let manifest = format!(
        "[package]\nname = \"wood_selection_perf\"\n\n[dependencies]\nengine = {{ path = {:?} }}\n",
        root.join("engine")
    );
    std::fs::write(dir.join("wrela.toml"), manifest).expect("fixture manifest");
    wrela_tests::must_build(&dir, &dir.join("build"));
    dir.join("build")
}

fn number(host: &mut Host, export: &str) -> u32 {
    match host.call_export(export, &[]).expect("fixture state").as_slice() {
        [Value::I32(n)] => *n as u32,
        values => panic!("{export}: {values:?}"),
    }
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("word"))
}

#[test]
#[ignore = "needs a GPU"]
fn wood_selection_dispatches_only_current_lists_after_shrink_and_regrowth() {
    let dir = fixture();
    let manifest = wrela_tests::manifest(&dir);
    let options = Options { record: true, ..Options::default() };
    let mut host = Host::load_with(&dir, &options).expect("fixture GPU");
    let _ = host.take_batches();
    let initial_capacity = number(&mut host, "capacity");
    let mut first_instances = None;
    for (frame, n) in [73u32, 3, 2, 0, 40, 201].into_iter().enumerate() {
        host.frame(frame as f32, 1, 1).expect("selection frame");
        let active = (n.div_ceil(32) * 32 * 5).max(1);
        assert_eq!(number(&mut host, "places"), active, "frame {frame}'s active span");
        let capacity = number(&mut host, "capacity");
        if frame < 5 {
            assert_eq!(capacity, initial_capacity, "shrinking or regrowth discarded room");
        } else {
            assert!(capacity > initial_capacity, "larger regrowth did not gain room");
        }
        let mut gathered = None;
        let mut drawn = None;
        let mut arguments = None;
        let mut spans = Vec::new();
        for batch in host.take_batches() {
            for command in decode(&batch).expect("valid commands") {
                if let Command::Dispatch { pipeline, groups, bindings, .. } = command {
                    let name = manifest.pipelines[pipeline as usize].name.rsplit("::").next();
                    if name == Some("gather") {
                        spans.push(groups[0]);
                        gathered = Some(bindings[3]);
                    } else if name == Some("cull") {
                        spans.push(groups[0]);
                    } else if name == Some("hide") {
                        spans.push(groups[0]);
                        drawn = Some(bindings[1]);
                    } else if name == Some("draws_of") {
                        arguments = Some(bindings[3]);
                    }
                }
            }
        }
        assert_eq!(spans, [active.div_ceil(64); 3], "dispatch bounds in frame {frame}");
        let gathered = gathered.expect("gathered instances");
        let drawn = drawn.expect("drawn instances");
        let args = host.read_buffer(arguments.expect("draw arguments").handle).expect("arguments");
        for list in 0..5 {
            let expected = if list == 0 { n } else { 0 };
            assert_eq!(word(&args, list * 40 + 4), expected, "cards in list {list}");
            assert_eq!(word(&args, list * 40 + 24), expected, "limbs in list {list}");
        }
        let instances = host.read_buffer(gathered.handle).expect("gathered readback");
        let shown = host.read_buffer(drawn.handle).expect("drawn readback");
        let stride = gathered.size as usize / capacity as usize;
        let mut ids: Vec<_> =
            (0..n as usize).map(|i| f32::from_bits(word(&instances, i * stride)) as u32).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..n).collect::<Vec<_>>(), "a selected instance was stale or missing");
        for i in 0..n as usize {
            assert_eq!(
                &shown[i * stride..(i + 1) * stride],
                &instances[i * stride..(i + 1) * stride],
                "a card-list instance was hidden"
            );
        }
        if frame == 0 {
            first_instances = Some(gathered.handle);
        } else if frame == 1 {
            assert_eq!(
                Some(gathered.handle),
                first_instances,
                "shrink replaced the instance buffer"
            );
            for i in 160..192 {
                for bytes in [&instances, &shown] {
                    assert_eq!(
                        word(bytes, i * stride),
                        (-999.0f32).to_bits(),
                        "retained tail changed"
                    );
                }
            }
        }
    }
}
