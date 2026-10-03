#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::redundant_clone
)]

use super::*;
use crate::row::FileKind;

fn row() -> RowSchema {
    RowSchema {
        rel_path: b"dir/file".to_vec(),
        kind: FileKind::Regular,
        dev: 1,
        ino: 2,
        size: 3,
        mtime_ns: 4,
        ctime_ns: 5,
        mode: 0o644,
        nlink: 1,
        link_target: None,
        blake3: None,
    }
}

/// One of every control message, Git reservations included.
fn every_control() -> Vec<Control> {
    vec![
        Control::Open {
            proto: PROTO_VERSION,
            wire_id: wire_id(),
            root: b"/src".to_vec(),
            state: b"/state".to_vec(),
        },
        Control::Start {
            authority: vec![1, 2, 3],
        },
        Control::Entry {
            entry: 7,
            row: row(),
        },
        Control::Refused {
            entry: Some(7),
            rel_path: b"dir/file".to_vec(),
            code: "SOURCE_CHANGED_AFTER_SNAPSHOT".to_owned(),
        },
        Control::Refused {
            entry: None,
            rel_path: b"odd".to_vec(),
            code: "PATH_NOT_PORTABLE".to_owned(),
        },
        Control::EngineTemporary {
            rel_path: b".bulkload-0123456789abcdef-1-2".to_vec(),
        },
        Control::WalkDone { entries: 9 },
        Control::Decide {
            entry: 7,
            decision: Decision::Skip,
        },
        Control::Decide {
            entry: 7,
            decision: Decision::Reuse,
        },
        Control::Decide {
            entry: 7,
            decision: Decision::Send,
        },
        Control::Decide {
            entry: 7,
            decision: Decision::WantManifest,
        },
        Control::Decide {
            entry: 7,
            decision: Decision::Refuse {
                code: "GIT_DESTINATION_OCCUPIED".to_owned(),
            },
        },
        Control::Manifest {
            entry: 7,
            root: [9; 32],
            chunks: vec![ChunkSpec {
                digest: [8; 32],
                size: 3,
            }],
        },
        Control::NeedChunks {
            entry: 7,
            indices: vec![0, 4, 5],
        },
        Control::End {
            entry: 7,
            root: [9; 32],
            chunks: 1,
            size: 3,
            racy: false,
        },
        Control::End {
            entry: 8,
            root: [9; 32],
            chunks: 1,
            size: 3,
            racy: true,
        },
        Control::Credit { bytes: 16 << 20 },
        Control::SourceDone {
            entries: 9,
            source_bytes_read: 3,
        },
        Control::SubOpen {
            sub: 1,
            kind: SubKind::GitPack {
                repo: b"git/repo".to_vec(),
                refs_digest: [4; 32],
            },
        },
        Control::GitHaves {
            sub: 1,
            haves: vec![vec![0xab; 20], vec![0xcd; 32]],
            done: true,
        },
        Control::GitSegmentStart {
            sub: 1,
            segment: 2,
            objects: 10,
            bytes: 1000,
        },
        Control::SegmentEnd {
            sub: 1,
            segment: 2,
            pack_digest: [5; 32],
            objects: 10,
            bytes: 1000,
        },
        Control::SegmentDurable { sub: 1, segment: 2 },
        Control::GitRefs {
            sub: 1,
            updates: vec![RefUpdate {
                name: b"refs/heads/main".to_vec(),
                old: None,
                new: vec![0xef; 20],
            }],
        },
        Control::GitCommitted {
            sub: 1,
            transaction_digest: [6; 32],
        },
        Control::GitResume {
            sub: 1,
            durable_segments: vec![0, 1],
        },
        Control::Held {
            entry: 4,
            held: true,
        },
    ]
}

#[test]
fn every_control_message_round_trips() {
    for control in every_control() {
        let bytes = control.encode().unwrap();
        assert_eq!(bytes[LENGTH_PREFIX_BYTES], TAG_CONTROL);
        let (decoded, consumed) = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, Frame::Control(control));
        assert_eq!(consumed, bytes.len());
    }
}

/// The control variant order is part of the wire: it must match the indices
/// `WIRE_SCHEMA` lists, since postcard encodes a variant by its index.
#[test]
fn control_variant_indices_match_the_schema() {
    for (index, control) in every_control().into_iter().enumerate() {
        let body = postcard::to_stdvec(&control).unwrap();
        let name = format!("{control:?}");
        let variant = name.split([' ', '{', '(']).next().unwrap();
        let listed = WIRE_SCHEMA
            .lines()
            .find(|line| line.starts_with("control ") && line.contains(&format!(" {variant}{{")))
            .unwrap_or_else(|| panic!("{variant} missing from the schema"));
        let number: u8 = listed.split(' ').nth(1).unwrap().parse().unwrap();
        assert_eq!(body[0], number, "{variant} (case {index})");
    }
}

#[test]
fn data_header_layout_is_pinned() {
    let header = DataHeader {
        entry: 0x0102_0304_0506_0708,
        index: 0x0a0b_0c0d,
        size: 3,
        offset: 0x1112_1314_1516_1718,
        digest: [0xee; 32],
    };
    let bytes = header.to_bytes();
    assert_eq!(&bytes[..8], &0x0102_0304_0506_0708_u64.to_le_bytes());
    assert_eq!(&bytes[8..12], &0x0a0b_0c0d_u32.to_le_bytes());
    assert_eq!(&bytes[12..16], &3_u32.to_le_bytes());
    assert_eq!(&bytes[16..24], &0x1112_1314_1516_1718_u64.to_le_bytes());
    assert_eq!(&bytes[24..56], &[0xee; 32]);
    assert_eq!(&bytes[56..], &[0; 8]);
    assert_eq!(DataHeader::from_bytes(&bytes).unwrap(), header);

    let frame = Frame::Data {
        header,
        payload: vec![1, 2, 3],
    };
    let wire = frame.encode().unwrap();
    assert_eq!(&wire[..4], &(1 + 64 + 3_u32).to_be_bytes());
    assert_eq!(wire[4], TAG_DATA);
    assert_eq!(&wire[5..69], &bytes);
    assert_eq!(&wire[69..], &[1, 2, 3]);
    assert_eq!(Frame::decode(&wire).unwrap(), (frame, wire.len()));
    assert_eq!(&data_prefix(&header).unwrap()[..], &wire[..69]);
}

#[test]
fn data_frames_refuse_bad_padding_lengths_and_oversize() {
    let header = DataHeader {
        entry: 1,
        index: 0,
        size: 2,
        offset: 0,
        digest: [1; 32],
    };
    let mut padded = header.to_bytes();
    padded[63] = 1;
    assert_eq!(
        DataHeader::from_bytes(&padded).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    // Declared size 2 with a 3-byte payload.
    let mut wire = data_prefix(&header).unwrap().to_vec();
    wire.extend_from_slice(&[1, 2, 3]);
    let len = u32::try_from(wire.len() - 4).unwrap();
    wire[..4].copy_from_slice(&len.to_be_bytes());
    assert_eq!(
        Frame::decode(&wire).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    let big = DataHeader {
        size: u32::try_from(MAX_DATA_PAYLOAD + 1).unwrap(),
        ..header
    };
    assert_eq!(
        data_prefix(&big).unwrap_err(),
        BulkloadRefusal::BudgetExceeded
    );
}

#[test]
fn pack_data_frames_round_trip() {
    let frame = Frame::PackData {
        header: PackDataHeader {
            sub: 3,
            segment: 4,
            offset: 5,
            size: 2,
        },
        payload: vec![9, 9],
    };
    let wire = frame.encode().unwrap();
    assert_eq!(wire[4], TAG_PACK_DATA);
    assert_eq!(Frame::decode(&wire).unwrap(), (frame, wire.len()));
}

#[test]
fn decodes_back_to_back_frames() {
    let first = Control::WalkDone { entries: 1 };
    let second = Control::Credit { bytes: 2 };
    let mut stream = first.encode().unwrap();
    stream.extend_from_slice(&second.encode().unwrap());
    let (decoded, consumed) = Frame::decode(&stream).unwrap();
    assert_eq!(decoded, Frame::Control(first));
    let (tail, _) = Frame::decode(&stream[consumed..]).unwrap();
    assert_eq!(tail, Frame::Control(second));
}

#[test]
fn refuses_truncated_unknown_and_oversized_frames() {
    let bytes = Control::WalkDone { entries: 1 }.encode().unwrap();
    assert_eq!(
        Frame::decode(&[0, 0]).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    assert_eq!(
        Frame::decode(&bytes[..bytes.len() - 1]).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    let mut unknown = bytes.clone();
    unknown[4] = 0x7f;
    assert_eq!(
        Frame::decode(&unknown).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    let mut empty = 0_u32.to_be_bytes().to_vec();
    empty.push(TAG_CONTROL);
    assert_eq!(
        Frame::decode(&empty).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    let len = u32::try_from(MAX_FRAME_BYTES + 1).unwrap();
    let mut huge = len.to_be_bytes().to_vec();
    huge.push(TAG_CONTROL);
    assert_eq!(
        Frame::decode(&huge).unwrap_err(),
        BulkloadRefusal::BudgetExceeded
    );
}

/// A v3/v4 frame (length, then a postcard `Frame { version, kind }` with no
/// tag byte) is refused, not misread: v5 is a hard cut (R-N59/R-N118).
#[test]
fn a_pre_v5_frame_is_refused() {
    // v4 `Frame { version: 4, kind: Done { rows: 7 } }`: version as a
    // postcard varint, variant index 3, rows 7.
    let body = [4_u8, 3, 7];
    let mut wire = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&body);
    assert!(Frame::decode(&wire).is_err());
}

/// #87: postcard ignores whatever follows a value, so a control body with
/// trailing bytes used to decode as valid. Every control message with any
/// trailing byte is refused, as a whole frame and as a body.
#[test]
fn a_control_body_with_trailing_bytes_is_refused() {
    for control in every_control() {
        let body = postcard::to_stdvec(&control).unwrap();
        assert_eq!(
            Frame::decode_body(TAG_CONTROL, &body).unwrap(),
            Frame::Control(control.clone())
        );
        for trailing in [&[0_u8][..], &[0xff], &[1, 2, 3]] {
            let mut padded = body.clone();
            padded.extend_from_slice(trailing);
            assert_eq!(
                Frame::decode_body(TAG_CONTROL, &padded),
                Err(BulkloadRefusal::FrameCodec),
                "{control:?} + {trailing:?}"
            );
            let mut wire = frame_header(TAG_CONTROL, padded.len()).unwrap().to_vec();
            wire.extend_from_slice(&padded);
            assert_eq!(Frame::decode(&wire), Err(BulkloadRefusal::FrameCodec));
        }
    }
}

/// `decode_exact` is the strict decode every record uses: the value alone
/// decodes, a value with any byte after it does not.
#[test]
fn decode_exact_refuses_a_remainder() {
    let spec = ChunkSpec {
        digest: [7; 32],
        size: 11,
    };
    let bytes = postcard::to_stdvec(&spec).unwrap();
    assert_eq!(decode_exact::<ChunkSpec>(&bytes).unwrap(), spec);
    let mut padded = bytes.clone();
    padded.push(0);
    assert_eq!(
        decode_exact::<ChunkSpec>(&padded),
        Err(BulkloadRefusal::FrameCodec)
    );
    assert_eq!(
        decode_exact::<ChunkSpec>(bytes.get(..bytes.len() - 1).unwrap()),
        Err(BulkloadRefusal::FrameCodec)
    );
}

/// `wire_id` is pinned: a change to `WIRE_SCHEMA` must be deliberate.
#[test]
fn wire_id_is_pinned() {
    assert_eq!(wire_id(), *blake3::hash(WIRE_SCHEMA.as_bytes()).as_bytes());
    assert_eq!(
        hex(&wire_id()),
        "070bb548f74916265d03283e2034dcbc0d43ae3c37dcb05729e0209a919e9e03",
        "WIRE_SCHEMA changed: update this pin and treat it as a wire change"
    );
}

/// The manifest root is pinned too: it is recorded in both stores.
#[test]
fn manifest_root_is_pinned_and_order_sensitive() {
    let a = ChunkSpec {
        digest: [1; 32],
        size: 10,
    };
    let b = ChunkSpec {
        digest: [2; 32],
        size: 20,
    };
    let root = manifest_root(&[a.clone(), b.clone()]);
    assert_ne!(root, manifest_root(&[b.clone(), a.clone()]));
    assert_ne!(
        root,
        manifest_root(&[
            a.clone(),
            ChunkSpec {
                size: 21,
                ..b.clone()
            }
        ])
    );
    assert_eq!(
        hex(&manifest_root(&[])),
        "f5bc2d70253417af6993d458a73c5352744c54cbf5bdddc8b6ccf57bb4d60440"
    );
    assert_eq!(
        hex(&root),
        "64b3353f45d18d612e1367b7acb75c06109d13d5edc9f60ab4e99e90db3c5f5c"
    );
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
