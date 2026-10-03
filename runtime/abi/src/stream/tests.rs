use super::*;

/// Little-endian words, for writing expected bytes by hand.
fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The golden batch: every command once. If this test fails, the format changed: bump
/// `VERSION`, and update both hosts and this test together.
#[test]
fn golden_bytes() {
    let batch = Encoder::new()
        .create_buffer(7, 16)
        .write_buffer(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8])
        .dispatch(0, [2, 1, 1], &[7], &[0xAA, 0xBB, 0xCC, 0xDD])
        .begin_screen_pass([0.0, 0.5, 1.0, 1.0])
        .draw(1, 3, 1, &[], &[1, 0, 0, 0, 2, 0, 0, 0])
        .present()
        .finish();
    let mut expected = Vec::new();
    expected.extend_from_slice(b"WRCS");
    expected.extend(words(&[VERSION, 152]));
    expected.extend(words(&[1, 8, 7, 16])); // CreateBuffer
    expected.extend(words(&[2, 20, 7, 4, 8])); // WriteBuffer
    expected.extend([1, 2, 3, 4, 5, 6, 7, 8]);
    expected.extend(words(&[3, 32, 0, 2, 1, 1, 1, 7, 4])); // Dispatch
    expected.extend([0xAA, 0xBB, 0xCC, 0xDD]);
    expected.extend(words(&[4, 16, 0, 0x3F00_0000, 0x3F80_0000, 0x3F80_0000])); // BeginScreenPass
    expected.extend(words(&[5, 28, 1, 3, 1, 0, 8, 1, 2])); // Draw
    expected.extend(words(&[6, 0])); // Present
    assert_eq!(batch, expected);
    assert_eq!(batch.len(), HEADER_LEN + 152);
}

#[test]
fn decodes_what_it_encodes() {
    let batch = Encoder::new()
        .create_buffer(1, 64)
        .dispatch(2, [4, 2, 1], &[1, 3], &[9, 9, 9, 9])
        .begin_screen_pass([0.25, 0.5, 0.75, 1.0])
        .draw(0, 3, 2, &[1], &[])
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
                buffers: vec![1, 3],
                uniforms: &[9, 9, 9, 9]
            },
            Command::BeginScreenPass { clear: [0.25, 0.5, 0.75, 1.0] },
            Command::Draw {
                pipeline: 0,
                vertices: 3,
                instances: 2,
                buffers: vec![1],
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
fn rejects_another_version() {
    let mut batch = Encoder::new().present().finish();
    batch[4] = 9;
    assert_eq!(decode(&batch), Err(StreamError::WrongVersion { expected: VERSION, got: 9 }));
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
            buffers: vec![],
            uniforms: &[]
        })
        .is_err()
    );
    assert!(s.step(&Command::Present).is_err());
    s.step(&Command::BeginScreenPass { clear: [0.0; 4] }).expect("open");
    assert!(s.step(&Command::BeginScreenPass { clear: [0.0; 4] }).is_err());
    assert!(
        s.step(&Command::Dispatch { pipeline: 0, groups: [1; 3], buffers: vec![], uniforms: &[] })
            .is_err()
    );
    assert!(s.step(&Command::CreateBuffer { handle: 0, size: 4 }).is_err());
    assert_eq!(s.end_frame(), Err(StreamError::UnclosedPass));
    s.step(&Command::Present).expect("close");
    s.end_frame().expect("closed");
}
