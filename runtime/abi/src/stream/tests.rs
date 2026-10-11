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
        words(&[6, 0]),                     // Present
        words(&[7, 4, 7]),                  // DestroyBuffer
        words(&[8, 20, 1, 4, 2, 8, 12]),    // CopyBuffer
        words(&[9, 16, 8, 2, 1, 0]),        // CreateTexture
        words(&[10, 32, 8, 0, 0, 2, 1, 8]), // WriteTexture
        vec![1, 2, 3, 4, 5, 6, 7, 8],
        words(&[11, 4, 8]),                                    // DestroyTexture
        words(&[12, 16, 10, 1, 0, 1]),                         // CreateSampler
        words(&[13, 4, 10]),                                   // DestroySampler
        words(&[14, 36, 8, 0, 0, 0, 0, f(1.0), 9, 0, f(1.0)]), // BeginPass
        words(&[15, 0]),                                       // EndPass
        words(&[16, 32, 0, 3, 4, 1, 5, 0, 0, 0]),              // DispatchIndirect
        words(&[17, 20, 1, 3, 16, 0, 0]),                      // DrawIndirect
        words(&[18, 16, 1, 3, 0, 8]),                          // ReadBuffer
        words(&[19, 20, 2, 11]),                               // StorageRead
        b"saves/slot1\0".to_vec(),
        words(&[20, 28, 3, 7, 5]), // StorageWrite
        b"saves/a\0".to_vec(),
        vec![1, 2, 3, 4, 5, 0, 0, 0],
        words(&[21, 24, 4, 14]), // Fetch
        b"data/level.bin\0\0".to_vec(),
        words(&[23, 28, 5, 11, 2]), // Post
        b"studio/edit\0".to_vec(),
        vec![123, 125, 0, 0],
        words(&[24, 32, 1, 3, 0, 12, 3, 16, 0, 0]), // DrawIndexedIndirect
        words(&[25, 12, 7]),                        // Label
        b"terrain\0".to_vec(),
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
