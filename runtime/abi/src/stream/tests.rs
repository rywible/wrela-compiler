use super::*;

/// Little-endian words, for writing expected bytes by hand.
fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The golden batch: every command once. If this test fails, the format changed: bump
/// `VERSION`, and update both hosts and this test together.
#[test]
fn golden_bytes() {
    let batch = crate::vectors::golden_batch();
    let f = |x: f32| x.to_bits();
    let mut expected = Vec::new();
    expected.extend_from_slice(b"WRCS");
    let body: Vec<u8> = [
        words(&[1, 8, 7, 16]),    // CreateBuffer
        words(&[2, 20, 7, 4, 8]), // WriteBuffer
        vec![1, 2, 3, 4, 5, 6, 7, 8],
        words(&[3, 40, 0, 2, 1, 1, 1, 7, 0, 16, 4]), // Dispatch
        vec![0xAA, 0xBB, 0xCC, 0xDD],
        words(&[4, 16, 0, f(0.5), f(1.0), f(1.0)]), // BeginScreenPass
        words(&[5, 28, 1, 3, 1, 0, 8]),             // Draw
        vec![1, 0, 0, 0, 2, 0, 0, 0],
        words(&[6, 0]),                           // Present
        words(&[7, 4, 7]),                        // DestroyBuffer
        words(&[8, 20, 1, 4, 2, 8, 12]),          // CopyBuffer
        words(&[9, 28, 8, 2, 1, 0, 0, 1, 1]),     // CreateTexture
        words(&[10, 40, 8, 0, 0, 0, 0, 2, 1, 8]), // WriteTexture
        vec![1, 2, 3, 4, 5, 6, 7, 8],
        words(&[11, 4, 8]),                  // DestroyTexture
        words(&[12, 24, 10, 1, 0, 1, 0, 1]), // CreateSampler
        words(&[13, 4, 10]),                 // DestroySampler
        words(&[14, 56, 1, 0, 8, 0, 0, 0, 0, 0, 0, f(1.0), 9, 0, 0, f(1.0)]), // BeginPass
        words(&[15, 0]),                     // EndPass
        words(&[16, 32, 0, 3, 4, 1, 5, 0, 0, 0]), // DispatchIndirect
        words(&[17, 20, 1, 3, 16, 0, 0]),    // DrawIndirect
        words(&[18, 16, 1, 3, 0, 8]),        // ReadBuffer
        words(&[19, 20, 2, 11]),             // StorageRead
        b"saves/slot1\0".to_vec(),
        words(&[20, 28, 3, 7, 5]), // StorageWrite
        b"saves/a\0".to_vec(),
        vec![1, 2, 3, 4, 5, 0, 0, 0],
        words(&[21, 24, 4, 14]), // Fetch
        b"data/level.bin\0\0".to_vec(),
        words(&[23, 28, 5, 11, 2]), // Post
        b"notes/write\0".to_vec(),
        vec![123, 125, 0, 0],
        words(&[24, 32, 1, 3, 0, 12, 3, 16, 0, 0]), // DrawIndexedIndirect
        words(&[25, 12, 7]),                        // Label
        b"terrain\0".to_vec(),
        words(&[26, 16, 6, 8]), // Prepare
        b"../floor".to_vec(),
        words(&[27, 12, 8]), // Unprepare
        b"../floor".to_vec(),
        words(&[28, 12, 7, 4]), // HandOver
        b"next".to_vec(),
        words(&[29, 28, 8, 9, 3]), // HostRequest
        b"test/echo\0\0\0".to_vec(),
        vec![3, 2, 1, 0],
        words(&[30, 16, 9, 7]), // Timed
        b"shadows\0".to_vec(),
        words(&[31, 48, 8, 1, 0, 0, 0, 12, 0, 3, 4, 4, 2, 2]), // CopyTexture
        words(&[32, 40, 8, 0, 0, 1, 1, 2, 2, 3, 256, 256]),    // CopyTextureToBuffer
        words(&[33, 40, 3, 0, 256, 8, 2, 0, 0, 0, 4, 4]),      // CopyBufferToTexture
        words(&[34, 16, 1, 2, 3, 4]),                          // SetScissor
        words(&[35, 24, f(0.0), f(0.0), f(64.0), f(32.0), f(0.0), f(1.0)]), // SetViewport
        words(&[36, 44, 1, 3, 0, 12, 6, 2, 1, u32::MAX, 4, 0, 0]), // DrawIndexed
        words(&[37, 32, 1, 3, 16, 4, NONE, 0, 0, 0]),          // MultiDrawIndirect
        words(&[38, 44, 1, 3, 0, 12, 5, 0, 2, 6, 8, 0, 0]),    // MultiDrawIndexedIndirect
        words(&[39, 8, 1, 1]),                                 // ConfigureScreen
    ]
    .concat();
    expected.extend(words(&[VERSION, body.len() as u32]));
    expected.extend(&body);
    assert_eq!(batch, expected);
}

#[test]
fn decodes_what_it_encodes() {
    let b = |h| Binding::range(h, 0, 64);
    let batch = Encoder::new()
        .create_buffer(1, 64)
        .dispatch(2, [4, 2, 1], &[b(1), b(3)], &[9, 9, 9, 9])
        .begin_screen_pass([0.25, 0.5, 0.75, 1.0])
        .draw(0, 3, 2, &[b(1)], &[])
        .present()
        .finish();
    let cmds = decode(&batch).expect("valid");
    assert_eq!(
        cmds,
        vec![
            Command::CreateBuffer { handle: 1, size: 64 },
            Command::Dispatch {
                pipeline: 2,
                groups: [4, 2, 1],
                bindings: vec![b(1), b(3)],
                uniforms: &[9, 9, 9, 9]
            },
            Command::BeginScreenPass { clear: [0.25, 0.5, 0.75, 1.0] },
            Command::Draw {
                pipeline: 0,
                vertices: 3,
                instances: 2,
                bindings: vec![b(1)],
                uniforms: &[]
            },
            Command::Present,
        ]
    );
    let mut seq = Sequencer::new();
    for c in &cmds {
        seq.step(c).expect("in order");
    }
}

#[test]
fn texture_write_pads_only_the_command_payload() {
    let batch = Encoder::new()
        .create_texture(1, 1, 1, TextureFormat::R16Float, false)
        .write_texture(1, [0, 0], [1, 1], &[0x00, 0x3c])
        .destroy_texture(1)
        .finish();
    let cmds = decode(&batch).expect("a two-byte texture write is word-aligned");
    assert_eq!(
        cmds[1],
        Command::WriteTexture {
            handle: 1,
            level: 0,
            layer: 0,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            data: &[0x00, 0x3c]
        }
    );
    assert_eq!(cmds[2], Command::DestroyTexture { handle: 1 });
    let mut checker = crate::check::Checker::new(&crate::Manifest::new("game.wasm"));
    for cmd in cmds {
        checker.check(&cmd).expect("exact texel bytes reach the checker");
    }
}

#[test]
fn malformed_host_service_has_the_browser_decoder_error() {
    let mut batch = Encoder::new().host_request(1, "x", &[1, 2, 3]).finish();
    batch[HEADER_LEN + COMMAND_HEADER_LEN + 12] = 0xff;
    assert_eq!(
        decode(&batch).expect_err("a service isn't UTF-8"),
        StreamError::BadPayload {
            opcode: Opcode::HostRequest,
            at: HEADER_LEN,
            why: "the service isn't UTF-8",
        }
    );
}

/// Every command the golden batch holds decodes to what was encoded.
#[test]
fn the_golden_batch_round_trips() {
    let batch = crate::vectors::golden_batch();
    let cmds = decode(&batch).expect("valid");
    assert_eq!(cmds.len(), Opcode::ALL.len());
    for (c, op) in cmds.iter().zip(Opcode::ALL) {
        assert_eq!(c.opcode(), op);
    }
}

#[test]
fn byte_runs_keep_their_own_padding() {
    for text_len in 1..=8 {
        for data_len in 0..=8 {
            let text = "abcdefgh"[..text_len].to_owned();
            let data = vec![0x97; data_len];
            let batch = Encoder::new()
                .storage_read(1, &text)
                .storage_write(2, &text, &data)
                .fetch(3, &text)
                .post(4, &text, &data)
                .label(&text)
                .finish();
            assert_eq!(
                decode(&batch).expect("padded runs decode"),
                vec![
                    Command::StorageRead { request: 1, path: &text },
                    Command::StorageWrite { request: 2, path: &text, data: &data },
                    Command::Fetch { request: 3, url: &text },
                    Command::Post { request: 4, url: &text, body: &data },
                    Command::Label { name: &text },
                ]
            );
            let mut at = HEADER_LEN;
            for words in [2, 3, 2, 3, 1] {
                let payload = at + COMMAND_HEADER_LEN;
                let text_at = payload + words * 4;
                let end = text_at + padded(text_len);
                assert!(batch[text_at + text_len..end].iter().all(|&b| b == 0));
                if words == 3 {
                    assert!(batch[end + data_len..end + padded(data_len)].iter().all(|&b| b == 0));
                }
                at = payload + word(&batch, at + 4) as usize;
            }
            assert_eq!(at, batch.len());
        }
    }
}

#[test]
fn rejects_another_version() {
    let mut batch = Encoder::new().present().finish();
    batch[4] = (VERSION + 1) as u8;
    assert_eq!(
        decode(&batch),
        Err(StreamError::WrongVersion { expected: VERSION, got: VERSION + 1 })
    );
}

#[test]
fn rejects_malformed_batches() {
    assert!(matches!(decode(b"WRC"), Err(StreamError::TooShort { .. })));
    let mut bad_magic = Encoder::new().present().finish();
    bad_magic[0] = b'X';
    assert!(matches!(decode(&bad_magic), Err(StreamError::BadMagic(_))));
    let mut bad_len = Encoder::new().present().finish();
    bad_len[8] = 99;
    assert!(matches!(decode(&bad_len), Err(StreamError::BodyLength { .. })));
    let mut unknown = Encoder::new().present().finish();
    unknown[12] = 42;
    assert!(matches!(decode(&unknown), Err(StreamError::UnknownOpcode { opcode: 42, .. })));
    let zero = Encoder::new().create_buffer(1, 4).finish();
    let mut zero = zero;
    zero[24] = 0; // size 4 -> 0
    assert!(matches!(decode(&zero), Err(StreamError::BadPayload { .. })));
    // A uniform length that disagrees with the payload.
    let mut draw = Encoder::new().draw(0, 3, 1, &[], &[1, 2, 3, 4]).finish();
    draw[12 + 8 + 16] = 8;
    assert!(matches!(decode(&draw), Err(StreamError::BadPayload { .. })));
}

#[test]
fn sequencing_rules() {
    let mut s = Sequencer::new();
    assert!(
        s.step(&Command::Draw {
            pipeline: 0,
            vertices: 3,
            instances: 1,
            bindings: vec![],
            uniforms: &[]
        })
        .is_err()
    );
    assert!(s.step(&Command::Present).is_err());
    s.step(&Command::BeginScreenPass { clear: [0.0; 4] }).expect("open");
    assert!(s.step(&Command::BeginScreenPass { clear: [0.0; 4] }).is_err());
    assert!(
        s.step(&Command::Dispatch { pipeline: 0, groups: [1; 3], bindings: vec![], uniforms: &[] })
            .is_err()
    );
    assert!(s.step(&Command::CreateBuffer { handle: 0, size: 4 }).is_err());
    assert_eq!(s.end_frame(), Err(StreamError::UnclosedPass));
    s.step(&Command::Present).expect("close");
    s.end_frame().expect("closed");
}

/// The format table's render target costs and alignments are WebGPU's ("Texture Format
/// Capabilities", the render target pixel byte cost and component alignment columns), written
/// out here from the specification rather than from the table.
#[test]
fn the_format_tables_target_costs_are_webgpus() {
    use TextureFormat as F;
    let spec = [
        (F::Rgba8, 8, 1),
        (F::Rgba16Float, 8, 2),
        (F::R32Float, 4, 4),
        (F::Rg32Float, 8, 4),
        (F::Rgba32Float, 16, 4),
        (F::R8, 1, 1),
        (F::Rg8, 2, 1),
        (F::Rgb10a2, 8, 4),
        (F::Rg11b10Float, 8, 4),
        (F::R32Uint, 4, 4),
        (F::Rg32Uint, 8, 4),
        (F::R16Float, 2, 2),
        (F::Rg16Float, 4, 2),
    ];
    for (f, cost, align) in spec {
        let i = f.info();
        assert_eq!((i.target_cost, i.target_align), (cost, align), "{}", f.name());
    }
}

/// The colour attachment limit is WebGPU's algorithm: the running total aligned to each
/// format's component alignment, then its cost added, so target order matters.
#[test]
fn colour_attachment_bytes_follow_webgpus_algorithm() {
    use TextureFormat as F;
    let refused = [F::Rgba32Float, F::Rgba8, F::Rgba8, F::Rgba16Float];
    assert_eq!(color_attachment_bytes(&refused), 40);
    let accepted = [F::Rgba16Float, F::Rg16Float, F::R8, F::R32Uint];
    assert_eq!(color_attachment_bytes(&accepted), 20);
    assert_eq!(color_attachment_bytes(&[F::R8, F::R32Float]), 8);
    assert_eq!(color_attachment_bytes(&[F::R32Float, F::R8]), 5);
}

/// What each capability needs, as WebGPU's core and its features say.
#[test]
fn each_formats_capabilities_name_the_feature_they_need() {
    use TextureFormat as F;
    for f in [F::R32Float, F::Rg32Float, F::Rgba32Float] {
        assert_eq!(f.info().blendable, Can::With(FormatFeature::Float32Blendable), "{}", f.name());
        assert_eq!(f.info().filterable, Can::With(FormatFeature::Float32Filterable));
        assert!(f.storable() && f.renderable() && !f.filterable());
    }
    let rg11 = F::Rg11b10Float.info();
    assert_eq!(rg11.renderable, Can::With(FormatFeature::Rg11b10Renderable));
    assert_eq!(rg11.storable, Can::No);
    for f in [F::R32Uint, F::Rg32Uint] {
        assert!(f.is_uint() && !f.blendable() && !f.filterable());
    }
    for f in [F::Bc1, F::Bc4, F::Bc5, F::Bc7, F::Etc2Rgb8, F::Astc4x4] {
        assert!(f.is_compressed() && f.filterable() && !f.renderable() && !f.storable());
        assert_eq!(f.block(), 4);
    }
    assert_eq!(F::Bc7.bytes_of(10, 6), 3 * 2 * 16);
    // Every name is WebGPU's spelling and appears once.
    let names: std::collections::HashSet<_> = F::ALL.iter().map(|f| f.name()).collect();
    assert_eq!(names.len(), F::ALL.len());
}
