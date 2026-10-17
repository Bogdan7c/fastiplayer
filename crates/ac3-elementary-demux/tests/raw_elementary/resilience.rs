//! Мусор, повреждения, обрывы и typed ошибки.

use std::num::NonZeroUsize;

use ac3_elementary_demux::{
    Ac3ElementaryDemuxError, Ac3ElementaryDemuxOptions, Ac3ElementaryDemuxer, Ac3ElementaryLimit,
};
use media_core::Demuxer;
use source_core::CancellationToken;

use crate::support::{
    FIXTURE_FRAMES, STEREO_AC3, STEREO_FRAME_BYTES, drain_packets, frame_start, next_packet,
    open_direct, seekable_input, stream_input,
};

fn open_error(bytes: &[u8], options: Ac3ElementaryDemuxOptions) -> Ac3ElementaryDemuxError {
    Ac3ElementaryDemuxer::open(
        seekable_input(bytes),
        CancellationToken::never_cancelled(),
        options,
    )
    .err()
    .expect("open must fail")
}

/// Мусор перед первым кадром (обрезанное начало) пропускается; время с нуля.
#[test]
fn garbage_prefix_is_skipped_and_time_starts_at_first_frame() {
    for input in [true, false] {
        let mut bytes = STEREO_AC3[100..STEREO_FRAME_BYTES].to_vec();
        bytes.extend_from_slice(STEREO_AC3);
        let prefix = STEREO_FRAME_BYTES - 100;
        let source = if input {
            seekable_input(&bytes)
        } else {
            stream_input(&bytes)
        };
        let mut demuxer = open_direct(source);
        let packets = drain_packets(&mut demuxer);
        assert_eq!(packets.len(), FIXTURE_FRAMES);
        assert_eq!(packets[0].pts, frame_start(0));
        assert_eq!(packets[0].byte_offset, Some(prefix as u64));
        assert_eq!(packets[0].data.as_ref(), &STEREO_AC3[..STEREO_FRAME_BYTES]);
    }
}

/// Оборванный последний кадр: все целые кадры отданы, затем чистый EOF.
#[test]
fn truncated_tail_keeps_every_whole_frame() {
    for cut in [1, 7, 300, STEREO_FRAME_BYTES - 1] {
        let bytes = &STEREO_AC3[..15 * STEREO_FRAME_BYTES + cut];
        let mut demuxer = open_direct(seekable_input(bytes));
        let packets = drain_packets(&mut demuxer);
        assert_eq!(packets.len(), 15, "cut {cut}");
        assert_eq!(packets[14].pts, frame_start(14));
    }
}

/// Испорченный кадр посреди файла выпадает, остальные идут на своих местах во времени.
#[test]
fn corrupted_frame_mid_stream_resyncs_and_keeps_timeline() {
    let mut bytes = STEREO_AC3.to_vec();
    let corrupted = 5 * STEREO_FRAME_BYTES;
    bytes[corrupted] = 0x00;
    bytes[corrupted + 1] = 0x00;
    let mut demuxer = open_direct(seekable_input(&bytes));
    let packets = drain_packets(&mut demuxer);
    assert_eq!(packets.len(), FIXTURE_FRAMES - 1);
    assert_eq!(packets[4].pts, frame_start(4));
    // Кадр 5 потерян, но кадр 6 играет в своё время, а не на 32 мс раньше.
    assert_eq!(packets[5].byte_offset, Some(6 * STEREO_FRAME_BYTES as u64));
    assert_eq!(packets[5].pts, frame_start(6));
}

/// Мусор длиннее предела resync-а посреди потока — typed `SyncLost` после целых кадров.
#[test]
fn long_garbage_mid_stream_is_typed_sync_loss() {
    let mut bytes = STEREO_AC3[..2 * STEREO_FRAME_BYTES].to_vec();
    bytes.extend(std::iter::repeat_n(0x5A_u8, 20_000));
    bytes.extend_from_slice(STEREO_AC3);
    let options = Ac3ElementaryDemuxOptions {
        max_resync_bytes: Ac3ElementaryLimit::new(4096, "max_resync_bytes").expect("limit"),
        ..Ac3ElementaryDemuxOptions::default()
    };
    let mut demuxer = Ac3ElementaryDemuxer::open(
        seekable_input(&bytes),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("open");
    assert_eq!(next_packet(&mut demuxer).pts, frame_start(0));
    assert_eq!(next_packet(&mut demuxer).pts, frame_start(1));
    let error = demuxer.next_event().expect_err("sync loss");
    assert!(matches!(
        error.downcast_ref::<Ac3ElementaryDemuxError>(),
        Some(Ac3ElementaryDemuxError::SyncLost {
            resync_start_offset,
            ..
        }) if *resync_start_offset == 2 * STEREO_FRAME_BYTES as u64
    ));
}

#[test]
fn garbage_without_frames_is_typed_no_access_unit() {
    let error = open_error(&[0x33_u8; 10_000], Ac3ElementaryDemuxOptions::default());
    assert!(matches!(
        error,
        Ac3ElementaryDemuxError::NoAccessUnit {
            scanned_bytes: 10_000
        }
    ));
    let error = open_error(&[], Ac3ElementaryDemuxOptions::default());
    assert!(matches!(
        error,
        Ac3ElementaryDemuxError::NoAccessUnit { .. }
    ));
}

#[test]
fn oversized_leading_tag_is_typed_error() {
    let mut bytes = b"ID3\x04\x00\x00\x00\x01\x00\x00".to_vec();
    bytes.extend_from_slice(STEREO_AC3);
    let options = Ac3ElementaryDemuxOptions {
        max_leading_tag_bytes: Ac3ElementaryLimit::new(1024, "tag").expect("limit"),
        ..Ac3ElementaryDemuxOptions::default()
    };
    assert!(matches!(
        open_error(&bytes, options),
        Ac3ElementaryDemuxError::LeadingTagTooLarge {
            declared_bytes: 16_394,
            limit_bytes: 1024
        }
    ));
}

#[test]
fn cancelled_open_is_typed_cancellation() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = Ac3ElementaryDemuxer::open(
        seekable_input(STEREO_AC3),
        cancellation,
        Ac3ElementaryDemuxOptions::default(),
    )
    .err()
    .expect("cancelled");
    assert!(matches!(error, Ac3ElementaryDemuxError::Cancelled));
}

#[test]
fn zero_limit_is_rejected() {
    assert!(Ac3ElementaryLimit::new(0, "read_chunk_bytes").is_err());
    assert_eq!(
        Ac3ElementaryLimit::new(5, "x").expect("limit").get(),
        NonZeroUsize::new(5).expect("five").get()
    );
}

/// Маленькие порции чтения (кадр режется на части) дают те же пакеты.
#[test]
fn tiny_read_chunks_produce_identical_packets() {
    let options = Ac3ElementaryDemuxOptions {
        read_chunk_bytes: Ac3ElementaryLimit::new(97, "read_chunk_bytes").expect("limit"),
        ..Ac3ElementaryDemuxOptions::default()
    };
    let mut demuxer = Ac3ElementaryDemuxer::open(
        stream_input(STEREO_AC3),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("open");
    let packets = drain_packets(&mut demuxer);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    for (index, packet) in packets.iter().enumerate() {
        let start = index * STEREO_FRAME_BYTES;
        assert_eq!(
            packet.data.as_ref(),
            &STEREO_AC3[start..start + STEREO_FRAME_BYTES]
        );
    }
}
