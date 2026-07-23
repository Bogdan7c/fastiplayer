use std::io::Cursor;
use std::io::Write;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{DemuxFactory, DemuxInput, DemuxInputCapability};
use media_core::{DemuxReadEvent, Demuxer, Packet, PacketKeyframe, TrackKind};
use source_core::CancellationToken;
use source_core::LocalFileSource;

use crate::{FlvDemuxError, FlvDemuxFactory, FlvDemuxOptions, FlvDemuxer, FlvLimit};

mod f4f_tests;

#[test]
fn factory_keeps_flv_and_f4f_input_shapes_exact_and_excludes_f4v() {
    let factory = FlvDemuxFactory::new(FlvDemuxOptions::default()).expect("factory valid");
    let flv = &factory.descriptor().containers[0];
    let f4f = &factory.descriptor().containers[1];
    assert_eq!(flv.container.as_str(), "flv");
    assert!(flv.supports_input(DemuxInputCapability::SeekableBytes));
    assert!(flv.supports_input(DemuxInputCapability::StreamingBytes));
    assert!(!flv.supports_input(DemuxInputCapability::OrderedSegments));
    assert_eq!(f4f.container.as_str(), "f4f");
    assert!(f4f.supports_input(DemuxInputCapability::OrderedSegments));
    assert!(!f4f.supports_input(DemuxInputCapability::SeekableBytes));
    assert!(
        factory
            .descriptor()
            .containers
            .iter()
            .flat_map(|registration| &registration.extensions)
            .all(|extension| extension.as_str() != "f4v")
    );
}

#[test]
fn progressive_flv_emits_h264_and_aac_packets_with_signed_cts() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(8, 0, &aac_sequence()),
        flv_tag(9, 1_000, &legacy_avc_frame(-40, true)),
        flv_tag(8, 1_000, &aac_frame(&[1, 2, 3])),
    ]);
    let mut demuxer = open_raw(bytes);
    assert_eq!(demuxer.tracks().len(), 1);
    let mut packets = Vec::new();
    for _ in 0..8 {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    assert_eq!(demuxer.tracks().len(), 2);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].kind, TrackKind::Video);
    assert_eq!(packets[0].pts.as_millis(), 960);
    assert_eq!(packets[0].keyframe, PacketKeyframe::Keyframe);
    assert_eq!(packets[1].kind, TrackKind::Audio);
}

#[test]
fn identical_config_is_noop_but_changed_config_precedes_dependent_packet_once() {
    let first = avcc(30);
    let changed = avcc(31);
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&first)),
        flv_tag(9, 10, &legacy_avc_sequence(&first)),
        flv_tag(9, 20, &legacy_avc_sequence(&changed)),
        flv_tag(9, 30, &legacy_avc_frame(0, true)),
    ]);
    let mut demuxer = open_raw(bytes);
    let mut changes = 0;
    let mut packet_seen = false;
    for _ in 0..6 {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::TracksChanged(_) => {
                assert!(!packet_seen);
                changes += 1;
            }
            DemuxReadEvent::Packet(_) => {
                packet_seen = true;
                break;
            }
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    assert!(packet_seen);
    assert_eq!(changes, 1);
}

#[test]
fn malformed_config_preserves_last_valid_decoder_configuration() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(9, 10, &legacy_avc_sequence(&[0, 1, 2])),
        flv_tag(9, 20, &legacy_avc_frame(0, true)),
    ]);
    let mut demuxer = open_raw(bytes);
    let error = demuxer
        .next_event()
        .expect_err("malformed replacement config must be visible");
    assert!(error.to_string().contains("configuration"));
    let packet = demuxer
        .next_event()
        .expect("old valid config remains usable after malformed replacement");
    assert!(matches!(packet, DemuxReadEvent::Packet(_)));
    assert_eq!(
        demuxer.tracks()[0].codec_private.as_deref(),
        Some(avcc(30).as_slice())
    );
}

#[test]
fn sequence_end_is_not_eof_and_requires_new_sequence_start() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(9, 1, &[0x17, 2, 0, 0, 0]),
        flv_tag(9, 2, &legacy_avc_frame(0, true)),
    ]);
    let mut demuxer = open_raw(bytes);
    let error = demuxer
        .next_event()
        .expect_err("packet after end must fail");
    assert!(error.to_string().contains("SequenceEnd"));
}

#[test]
fn truncated_tag_does_not_become_clean_eof() {
    let mut bytes = flv_file(vec![flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)))]);
    bytes.extend_from_slice(&[9, 0, 0, 10, 0, 0]);
    let mut demuxer = open_raw(bytes);
    let error = demuxer
        .next_event()
        .expect_err("truncation must remain visible");
    assert!(error.to_string().contains("recovery") || error.to_string().contains("short read"));
}

#[test]
fn enhanced_vp8_sequence_and_keyframe_use_codec_core_validation() {
    let mut sequence = vec![0x90];
    sequence.extend_from_slice(b"vp08");
    sequence.extend_from_slice(&[1, 0, 0, 0]);
    sequence.extend_from_slice(&[0, 0, 0b1000_0000, 1, 1, 1, 0, 0]);
    let mut frame = vec![0x91];
    frame.extend_from_slice(b"vp08");
    frame.extend_from_slice(&vp8_keyframe());
    let bytes = flv_file(vec![flv_tag(9, 0, &sequence), flv_tag(9, 10, &frame)]);
    let mut demuxer = open_raw(bytes);
    assert_eq!(demuxer.tracks()[0].codec_id, "V_VP8");
    let packet = loop {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::EndOfStream => panic!("packet expected"),
            _ => {}
        }
    };
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn enhanced_sequence_start_maps_all_selected_single_track_fourccs() {
    let cases = [
        (b"vp09", enhanced_vp_configuration(0, 10), "V_VP9"),
        (b"av01", vec![0x81, 0, 0x0c, 0], "V_AV1"),
        (b"avc1", avcc(30), "V_MPEG4/ISO/AVC"),
        (b"hvc1", hvcc(), "V_MPEGH/ISO/HEVC"),
    ];
    for (fourcc, configuration, expected_codec_id) in cases {
        let mut payload = vec![0x90];
        payload.extend_from_slice(fourcc);
        payload.extend_from_slice(&configuration);
        let demuxer = open_raw(flv_file(vec![flv_tag(9, 0, &payload)]));
        assert_eq!(demuxer.tracks()[0].codec_id, expected_codec_id);
    }
}

#[test]
fn enhanced_multitrack_modex_mpeg2ts_vvc_and_unknown_are_typed_rejections() {
    let cases = [
        vec![0x96, b'v', b'p', b'0', b'9'],
        vec![0x97, b'v', b'p', b'0', b'9'],
        vec![0x95, b'v', b'p', b'0', b'9'],
        vec![0x90, b'v', b'v', b'c', b'1'],
        vec![0x90, b'x', b'x', b'x', b'x'],
    ];
    for payload in cases {
        let error = FlvDemuxer::open(
            DemuxInput::byte_stream(Box::new(Cursor::new(flv_file(vec![flv_tag(
                9, 0, &payload,
            )])))),
            false,
            CancellationToken::new(),
            FlvDemuxOptions::default(),
        )
        .err()
        .expect("unsupported Enhanced semantics must fail");
        assert!(matches!(
            error,
            crate::FlvDemuxError::UnsupportedCodec { .. }
        ));
    }
}

#[test]
fn selected_legacy_audio_mappings_remain_exact() {
    let cases = [
        (vec![0x2f, 0xff, 0xfb], "A_MP3"),
        (vec![0x0d, 0x80], "A_PCM_U8"),
        (vec![0x3f, 0, 0], "A_PCM_S16LE"),
        (vec![0x1f, 0], "A_ADPCM_SWF"),
    ];
    for (payload, expected_codec_id) in cases {
        let demuxer = open_raw(flv_file(vec![flv_tag(8, 0, &payload)]));
        assert_eq!(demuxer.tracks()[0].codec_id, expected_codec_id);
    }
}

#[test]
fn platform_endian_pcm16_is_rejected_instead_of_guessed() {
    let error = FlvDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(flv_file(vec![flv_tag(
            8,
            0,
            &[0x0f, 0, 0],
        )])))),
        false,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("ambiguous PCM16 must fail");
    assert!(error.to_string().contains("неоднозначен"));
}

#[test]
fn vod_seek_scans_actual_tags_and_commits_decode_safe_anchor() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(9, 0, &legacy_avc_frame(0, true)),
        flv_tag(9, 1_000, &legacy_avc_frame(0, true)),
        flv_tag(9, 2_000, &legacy_avc_frame(0, true)),
    ]);
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(&bytes).expect("write fixture");
    file.flush().expect("flush fixture");
    let source = LocalFileSource::open(file.path()).expect("local source");
    let mut demuxer = FlvDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        false,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("seekable FLV opens");
    let result = demuxer
        .seek(Duration::from_millis(1_500))
        .expect("decode-safe seek");
    assert_eq!(result.actual_position.as_duration(), Duration::from_secs(1));
    let packet = loop {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::EndOfStream => panic!("anchor packet expected"),
            _ => {}
        }
    };
    assert_eq!(packet.pts, Duration::from_secs(1));
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn failed_seek_restores_source_cursor_and_parser_configuration() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(9, 500, &legacy_avc_frame(0, false)),
    ]);
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(&bytes).expect("write fixture");
    file.flush().expect("flush fixture");
    let source = LocalFileSource::open(file.path()).expect("local source");
    let mut demuxer = FlvDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        false,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("seekable FLV opens");
    assert!(demuxer.seek(Duration::from_secs(1)).is_err());
    let packet = loop {
        match demuxer
            .next_event()
            .expect("state restored after failed seek")
        {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::EndOfStream => panic!("original packet must remain readable"),
            _ => {}
        }
    };
    assert_eq!(packet.pts, Duration::from_millis(500));
    assert_eq!(packet.keyframe, PacketKeyframe::NotKeyframe);
}

#[test]
fn cancellation_before_open_is_typed_and_does_not_parse() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = FlvDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(flv_file(Vec::new())))),
        false,
        cancellation,
        FlvDemuxOptions::default(),
    )
    .err()
    .expect("cancelled open must fail");
    assert!(matches!(error, crate::FlvDemuxError::Cancelled));
}

#[test]
fn live_short_reads_preserve_partial_tag_state() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(9, 25, &legacy_avc_frame(0, true)),
    ]);
    let reader = ChunkedReader {
        bytes,
        cursor: 0,
        maximum_chunk: 2,
    };
    let mut demuxer = FlvDemuxer::open(
        DemuxInput::byte_stream(Box::new(reader)),
        false,
        CancellationToken::new(),
        FlvDemuxOptions::default(),
    )
    .expect("short-read FLV opens");
    let packet = loop {
        match demuxer.next_event().expect("event") {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::EndOfStream => panic!("packet expected"),
            _ => {}
        }
    };
    assert_eq!(packet.pts, Duration::from_millis(25));
}

#[test]
fn bounded_recovery_waits_for_fresh_config_and_proven_keyframe() {
    let first_config = flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)));
    let packet_before_config = flv_tag(9, 90, &legacy_avc_frame(0, false));
    let fresh_config = flv_tag(9, 100, &legacy_avc_sequence(&avcc(30)));
    let non_keyframe = flv_tag(9, 110, &legacy_avc_frame(0, false));
    let keyframe = flv_tag(9, 120, &legacy_avc_frame(0, true));
    let mut bytes = flv_file(vec![first_config]);
    bytes.extend_from_slice(&[0xff; 11]);
    bytes.extend_from_slice(&packet_before_config);
    bytes.extend_from_slice(&fresh_config);
    bytes.extend_from_slice(&non_keyframe);
    bytes.extend_from_slice(&keyframe);
    let mut demuxer = open_raw(bytes);
    let first_event = demuxer.next_event().expect("recovery event");
    assert!(matches!(first_event, DemuxReadEvent::TracksChanged(_)));
    let packet = loop {
        match demuxer.next_event().expect("post-recovery event") {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::EndOfStream => panic!("recovered keyframe expected"),
            _ => {}
        }
    };
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
    assert_eq!(packet.pts, Duration::from_millis(120));
}

/// Packet skipping до fresh config не превращается в unbounded next_event loop.
#[test]
fn recovery_gate_packet_skipping_obeys_byte_budget() {
    let mut bytes = flv_file(vec![flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)))]);
    bytes.extend_from_slice(&[0xff; 11]);
    for timestamp in 1..=20 {
        bytes.extend_from_slice(&flv_tag(9, timestamp, &legacy_avc_frame(0, false)));
    }
    let options = FlvDemuxOptions {
        recovery_bytes: FlvLimit::new(96, "recovery_bytes").expect("non-zero limit"),
        ..FlvDemuxOptions::default()
    };
    let mut demuxer = open_raw_with_options(bytes, options);
    let error = demuxer
        .next_event()
        .expect_err("missing fresh config must exhaust recovery budget");
    assert!(matches!(
        error.downcast_ref::<FlvDemuxError>(),
        Some(FlvDemuxError::RecoveryGateBudgetExhausted {
            limit_bytes: 96,
            ..
        })
    ));
}

/// Seek anchor восстанавливает независимые post-u32 epochs video и audio clocks.
#[test]
fn rollover_seek_preserves_video_and_audio_timestamp_epochs() {
    let rollover_epoch_ms = 1_u64 << 32;
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(8, 0, &aac_sequence()),
        flv_tag(9, u32::MAX - 100, &legacy_avc_frame(0, true)),
        flv_tag(8, u32::MAX - 90, &aac_frame(&[1])),
        flv_tag(9, 50, &legacy_avc_frame(0, true)),
        flv_tag(8, 60, &aac_frame(&[2])),
        flv_tag(9, 100, &legacy_avc_frame(0, true)),
    ]);
    let mut demuxer = open_seekable(bytes, FlvDemuxOptions::default());
    let result = demuxer
        .seek(Duration::from_millis(rollover_epoch_ms + 55))
        .expect("post-rollover seek");
    assert_eq!(
        result.actual_position.as_duration(),
        Duration::from_millis(rollover_epoch_ms + 50)
    );

    let video = next_packet_of_kind(&mut demuxer, TrackKind::Video);
    let audio = next_packet_of_kind(&mut demuxer, TrackKind::Audio);
    assert_eq!(video.pts, Duration::from_millis(rollover_epoch_ms + 50));
    assert_eq!(audio.pts, Duration::from_millis(rollover_epoch_ms + 60));
}

/// Tag budget без EOS/covering anchor возвращает typed error и откатывает cursor.
#[test]
fn seek_scan_budget_exhaustion_is_typed_and_transactional() {
    let bytes = flv_file(vec![
        flv_tag(9, 0, &legacy_avc_sequence(&avcc(30))),
        flv_tag(9, 0, &legacy_avc_frame(0, true)),
        flv_tag(9, 500, &legacy_avc_frame(0, false)),
        flv_tag(9, 1_500, &legacy_avc_frame(0, true)),
    ]);
    let options = FlvDemuxOptions {
        seek_scan_tags: FlvLimit::new(2, "seek_scan_tags").expect("non-zero limit"),
        ..FlvDemuxOptions::default()
    };
    let mut demuxer = open_seekable(bytes, options);
    let error = demuxer
        .seek(Duration::from_secs(1))
        .expect_err("scan budget must fail before covering target");
    assert!(matches!(
        error.downcast_ref::<FlvDemuxError>(),
        Some(FlvDemuxError::SeekScanBudgetExhausted {
            scanned_tags: 2,
            ..
        })
    ));
    let packet = next_packet_of_kind(&mut demuxer, TrackKind::Video);
    assert_eq!(packet.pts, Duration::ZERO);
}

/// Failed candidates возвращают все consumed bytes, включая начало valid nested tag-а.
#[test]
fn recovery_replays_junk_candidates_of_every_short_header_length() {
    for junk_length in 1..=10 {
        let mut bytes = flv_file(vec![flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)))]);
        bytes.extend(std::iter::repeat_n(0xff, junk_length));
        bytes.extend_from_slice(&flv_tag(9, 10, &legacy_avc_sequence(&avcc(30))));
        bytes.extend_from_slice(&flv_tag(9, 20, &legacy_avc_frame(0, true)));
        let packet = next_packet_of_kind(&mut open_raw(bytes), TrackKind::Video);
        assert_eq!(packet.pts, Duration::from_millis(20), "junk={junk_length}");
    }
}

/// Declared payload может проглотить valid tag; transactional replay возвращает его scan-у.
#[test]
fn recovery_finds_valid_tag_inside_corrupt_declared_size_candidate() {
    let fresh_config = flv_tag(9, 10, &legacy_avc_sequence(&avcc(30)));
    let keyframe = flv_tag(9, 20, &legacy_avc_frame(0, true));
    let mut corrupt = raw_tag_header(9, fresh_config.len() + keyframe.len() + 1_024, 1);
    corrupt.extend_from_slice(&fresh_config);
    corrupt.extend_from_slice(&keyframe);
    let mut bytes = flv_file(vec![flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)))]);
    bytes.extend_from_slice(&corrupt);
    let packet = next_packet_of_kind(&mut open_raw(bytes), TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(20));
}

/// Valid tag, начавшийся в четырёх consumed PreviousTagSize bytes, не теряется.
#[test]
fn recovery_finds_valid_tag_inside_corrupt_previous_size() {
    let fresh_config = flv_tag(9, 10, &legacy_avc_sequence(&avcc(30)));
    let keyframe = flv_tag(9, 20, &legacy_avc_frame(0, true));
    let mut corrupt = raw_tag_header(18, 1, 1);
    corrupt.push(0);
    corrupt.extend_from_slice(&fresh_config[..4]);
    corrupt.extend_from_slice(&fresh_config[4..]);
    corrupt.extend_from_slice(&keyframe);
    let mut bytes = flv_file(vec![flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)))]);
    bytes.extend_from_slice(&corrupt);
    let packet = next_packet_of_kind(&mut open_raw(bytes), TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(20));
}

/// False FLV signature внутри payload не мешает найти доказанный nested tag boundary.
#[test]
fn recovery_ignores_false_signature_payload_before_nested_tag() {
    let fresh_config = flv_tag(9, 10, &legacy_avc_sequence(&avcc(30)));
    let keyframe = flv_tag(9, 20, &legacy_avc_frame(0, true));
    let false_signature = b"FLV\x01\x05\x00\x00\x00\x09";
    let mut corrupt = raw_tag_header(18, false_signature.len() + fresh_config.len(), 1);
    corrupt.extend_from_slice(false_signature);
    corrupt.extend_from_slice(&fresh_config);
    corrupt.extend_from_slice(&keyframe[..4]);
    corrupt.extend_from_slice(&keyframe[4..]);
    let mut bytes = flv_file(vec![flv_tag(9, 0, &legacy_avc_sequence(&avcc(30)))]);
    bytes.extend_from_slice(&corrupt);
    let packet = next_packet_of_kind(&mut open_raw(bytes), TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(20));
}

/// Untrusted AMF numeric overflow/NaN/inf игнорируются без panic и saturating anchors.
#[test]
fn amf_numeric_extremes_are_ignored_without_panics() {
    for extreme in [f64::MAX, f64::NAN, f64::INFINITY, u64::MAX as f64] {
        let payload = on_metadata_numeric_payload(extreme, &[extreme], &[extreme]);
        let metadata = crate::metadata::parse_on_metadata(&payload, FlvDemuxOptions::default())
            .expect("metadata parser remains fallible")
            .expect("onMetaData event");
        assert_eq!(metadata.duration, None);
        assert!(metadata.anchors.is_empty());
    }
}

/// Legacy wire identities fail before a decoder can observe ambiguous bytes.
#[test]
fn legacy_wire_validation_rejects_invalid_packet_and_frame_types() {
    assert!(matches!(
        crate::codec::parse_audio_tag(&Bytes::from_static(&[0xaf, 2])),
        Err(FlvDemuxError::UnsupportedCodec { .. })
    ));
    assert!(matches!(
        crate::codec::parse_video_tag(&Bytes::from_static(&[0x17, 3, 0x65]), None),
        Err(FlvDemuxError::UnsupportedCodec { .. })
    ));
    for frame_header in [0x07, 0x37, 0x47, 0x57, 0x67, 0x77] {
        assert!(matches!(
            crate::codec::parse_video_tag(&Bytes::from(vec![frame_header, 1, 0, 0, 0, 0x65]), None,),
            Err(FlvDemuxError::UnsupportedCodec { .. })
        ));
    }
    assert!(matches!(
        crate::codec::parse_audio_tag(&Bytes::from_static(&[0x2f])),
        Err(FlvDemuxError::MalformedTag { .. })
    ));
}

#[test]
fn bounded_amf_metadata_retains_duration_title_and_keyframe_index() {
    let payload = on_metadata_payload();
    let metadata = crate::metadata::parse_on_metadata(&payload, FlvDemuxOptions::default())
        .expect("metadata parse")
        .expect("onMetaData event");
    assert_eq!(metadata.duration, Some(Duration::from_secs(3)));
    assert_eq!(
        metadata.media_metadata.tags.title.as_deref(),
        Some("Fixture")
    );
    assert_eq!(metadata.anchors.len(), 2);
    assert_eq!(metadata.anchors[1].timestamp, Duration::from_secs(2));
    assert_eq!(metadata.anchors[1].byte_offset, 400);
}

fn open_raw(bytes: Vec<u8>) -> FlvDemuxer {
    open_raw_with_options(bytes, FlvDemuxOptions::default())
}

fn open_raw_with_options(bytes: Vec<u8>, options: FlvDemuxOptions) -> FlvDemuxer {
    FlvDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        false,
        CancellationToken::new(),
        options,
    )
    .expect("generated FLV opens")
}

fn open_seekable(bytes: Vec<u8>, options: FlvDemuxOptions) -> FlvDemuxer {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(&bytes).expect("write fixture");
    file.flush().expect("flush fixture");
    let source = LocalFileSource::open(file.path()).expect("local source");
    FlvDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        false,
        CancellationToken::new(),
        options,
    )
    .expect("generated seekable FLV opens")
}

fn next_packet_of_kind(demuxer: &mut FlvDemuxer, kind: TrackKind) -> Packet {
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.kind == kind => return packet,
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("packet {kind:?} expected"),
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("in-memory fixture cannot be temporarily unavailable")
            }
        }
    }
}

fn flv_file(tags: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00".to_vec();
    for tag in tags {
        bytes.extend_from_slice(&tag);
    }
    bytes
}

fn flv_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let payload_size = u32::try_from(payload.len()).expect("fixture payload fits u32");
    let mut bytes = Vec::new();
    bytes.push(tag_type);
    bytes.extend_from_slice(&payload_size.to_be_bytes()[1..]);
    bytes.extend_from_slice(&timestamp.to_be_bytes()[1..]);
    bytes.push(timestamp.to_be_bytes()[0]);
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(payload);
    let tag_size = u32::try_from(11 + payload.len()).expect("fixture tag fits u32");
    bytes.extend_from_slice(&tag_size.to_be_bytes());
    bytes
}

fn raw_tag_header(tag_type: u8, payload_size: usize, timestamp: u32) -> Vec<u8> {
    let payload_size = u32::try_from(payload_size).expect("fixture payload fits u32");
    let mut bytes = Vec::with_capacity(11);
    bytes.push(tag_type);
    bytes.extend_from_slice(&payload_size.to_be_bytes()[1..]);
    bytes.extend_from_slice(&timestamp.to_be_bytes()[1..]);
    bytes.push(timestamp.to_be_bytes()[0]);
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes
}

fn avcc(level: u8) -> Vec<u8> {
    vec![1, 66, 0, level, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 1, 0x68]
}

fn legacy_avc_sequence(configuration: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x17, 0, 0, 0, 0];
    payload.extend_from_slice(configuration);
    payload
}

fn legacy_avc_frame(composition_offset: i32, keyframe: bool) -> Vec<u8> {
    let frame_header = if keyframe { 0x17 } else { 0x27 };
    let offset = composition_offset & 0x00ff_ffff;
    vec![
        frame_header,
        1,
        ((offset >> 16) & 0xff) as u8,
        ((offset >> 8) & 0xff) as u8,
        (offset & 0xff) as u8,
        0,
        0,
        0,
        2,
        0x65,
        0,
    ]
}

fn aac_sequence() -> Vec<u8> {
    vec![0xaf, 0, 0x12, 0x10]
}

fn aac_frame(frame: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xaf, 1];
    payload.extend_from_slice(frame);
    payload
}

fn vp8_keyframe() -> Vec<u8> {
    let first_partition_size = 7_u32;
    let frame_tag = first_partition_size << 5;
    let mut packet = vec![
        frame_tag as u8,
        (frame_tag >> 8) as u8,
        (frame_tag >> 16) as u8,
        0x9d,
        0x01,
        0x2a,
    ];
    packet.extend_from_slice(&320_u16.to_le_bytes());
    packet.extend_from_slice(&180_u16.to_le_bytes());
    packet
}

fn enhanced_vp_configuration(profile: u8, level: u8) -> Vec<u8> {
    vec![1, 0, 0, 0, profile, level, 0x80, 1, 1, 1, 0, 0]
}

fn hvcc() -> Vec<u8> {
    let mut record = vec![
        1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 120, 0xf0, 0, 0xfc, 0xfd, 0xf8, 0xf8, 0, 0, 0x0f, 0,
    ];
    record[2] = 0x40;
    record
}

struct ChunkedReader {
    bytes: Vec<u8>,
    cursor: usize,
    maximum_chunk: usize,
}

impl std::io::Read for ChunkedReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.bytes.len().saturating_sub(self.cursor);
        let count = remaining.min(output.len()).min(self.maximum_chunk);
        output[..count].copy_from_slice(&self.bytes[self.cursor..self.cursor + count]);
        self.cursor += count;
        Ok(count)
    }
}

fn on_metadata_payload() -> Vec<u8> {
    on_metadata_numeric_payload(3.0, &[0.0, 2.0], &[100.0, 400.0])
}

fn on_metadata_numeric_payload(
    duration: f64,
    keyframe_times: &[f64],
    keyframe_positions: &[f64],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_amf_string_value(&mut bytes, "onMetaData");
    bytes.push(8);
    bytes.extend_from_slice(&3_u32.to_be_bytes());
    push_amf_key(&mut bytes, "duration");
    push_amf_number(&mut bytes, duration);
    push_amf_key(&mut bytes, "title");
    push_amf_string_value(&mut bytes, "Fixture");
    push_amf_key(&mut bytes, "keyframes");
    bytes.push(3);
    push_amf_key(&mut bytes, "times");
    push_amf_number_array(&mut bytes, keyframe_times);
    push_amf_key(&mut bytes, "filepositions");
    push_amf_number_array(&mut bytes, keyframe_positions);
    bytes.extend_from_slice(&[0, 0, 9]);
    bytes.extend_from_slice(&[0, 0, 9]);
    bytes
}

fn push_amf_key(output: &mut Vec<u8>, value: &str) {
    let length = u16::try_from(value.len()).expect("fixture key fits u16");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
}

fn push_amf_string_value(output: &mut Vec<u8>, value: &str) {
    output.push(2);
    push_amf_key(output, value);
}

fn push_amf_number(output: &mut Vec<u8>, value: f64) {
    output.push(0);
    output.extend_from_slice(&value.to_bits().to_be_bytes());
}

fn push_amf_number_array(output: &mut Vec<u8>, values: &[f64]) {
    output.push(10);
    output.extend_from_slice(
        &u32::try_from(values.len())
            .expect("fixture array fits u32")
            .to_be_bytes(),
    );
    for value in values {
        push_amf_number(output, *value);
    }
}
