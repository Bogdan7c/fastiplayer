use std::error::Error;
use std::num::NonZeroUsize;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget, OrderedSegmentKind};
use media_core::{DemuxReadEvent, Demuxer};
use source_core::{CancellationToken, LocalFileSource};

use super::ordered_segments::{decode_base64_fixture, generated_segments, open_ordered, segment};
use super::{SymphoniaDemuxFactory, TemporaryMediaFile};
use crate::{DemuxError, DemuxerOptions};

/// Находит positions четырёх байтов box type в generated corpus.
fn box_type_positions(bytes: &[u8], box_type: &[u8; 4]) -> Vec<usize> {
    bytes
        .windows(4)
        .enumerate()
        .filter_map(|(position, candidate)| (candidate == box_type).then_some(position))
        .filter(|&position| position >= 4)
        .collect()
}

/// Меняет fixed-width v1 `tfdt`, не затрагивая box boundaries.
fn set_tfdt_base_decode_time(bytes: &mut [u8], occurrence: usize, decode_time: u64) {
    let positions = box_type_positions(bytes, b"tfdt");
    let type_position = positions[occurrence];
    assert_eq!(bytes[type_position + 4], 1, "generated tfdt должен быть v1");
    bytes[type_position + 8..type_position + 16].copy_from_slice(&decode_time.to_be_bytes());
}

/// Возвращает declared box size по position его type.
fn box_size(bytes: &[u8], type_position: usize) -> usize {
    u32::from_be_bytes(
        bytes[type_position - 4..type_position]
            .try_into()
            .expect("box size field"),
    ) as usize
}

/// Вставляет authoritative `mehd` в existing `mvex`, обновляя размеры `mvex` и `moov`.
fn insert_mehd_duration(init: &mut Vec<u8>, fragment_duration: u32) {
    let moov_type_position = box_type_positions(init, b"moov")[0];
    let mvex_type_position = box_type_positions(init, b"mvex")[0];
    let mvex_end = mvex_type_position - 4 + box_size(init, mvex_type_position);

    let mut mehd = Vec::with_capacity(16);
    mehd.extend_from_slice(&16_u32.to_be_bytes());
    mehd.extend_from_slice(b"mehd");
    mehd.extend_from_slice(&[0, 0, 0, 0]);
    mehd.extend_from_slice(&fragment_duration.to_be_bytes());
    init.splice(mvex_end..mvex_end, mehd);

    for type_position in [moov_type_position, mvex_type_position] {
        let expanded_size =
            u32::try_from(box_size(init, type_position) + 16).expect("expanded test box fits u32");
        init[type_position - 4..type_position].copy_from_slice(&expanded_size.to_be_bytes());
    }
}

/// Открывает generated fixture как seekable local source без extension hint.
fn open_local(bytes: &[u8]) -> (TemporaryMediaFile, Box<dyn Demuxer + Send>) {
    let fixture = TemporaryMediaFile::new("bin", bytes);
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory"),
        ))
        .expect("register Symphonia");
    let source = LocalFileSource::open(&fixture.path).expect("open local fMP4 source");
    let sniff_budget = DemuxSniffBudget::new(
        NonZeroUsize::new(4_096).expect("sniff bytes"),
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    )
    .expect("sniff budget");
    let demuxer = registry
        .open(
            DemuxInput::byte_source(Box::new(source)),
            DemuxHints::none(),
            sniff_budget,
            CancellationToken::never_cancelled(),
        )
        .expect("open local generated fMP4");
    (fixture, demuxer)
}

/// Читает следующий packet, пропуская metadata/track lifecycle events.
fn next_packet(demuxer: &mut dyn Demuxer) -> media_core::Packet {
    loop {
        match demuxer.next_event().expect("read generated fMP4") {
            DemuxReadEvent::Packet(packet) => return packet,
            DemuxReadEvent::MediaMetadataChanged(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("packet ожидался до clean EOF"),
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("finite fMP4 не должен публиковать readiness")
            }
        }
    }
}

/// Проверяет, что finite truncated source дошёл до structural parse error, а не ложного EOS.
fn assert_structural_truncation(media_fragment: &[u8]) {
    let (init, _, _) = generated_segments();
    let open_result = open_ordered(
        vec![
            segment(1, OrderedSegmentKind::Initialization, init),
            segment(
                2,
                OrderedSegmentKind::Media,
                Bytes::copy_from_slice(media_fragment),
            ),
        ],
        CancellationToken::never_cancelled(),
        None,
    );
    let mut demuxer = match open_result {
        Ok(demuxer) => demuxer,
        Err(error) => {
            let mut current: Option<&(dyn Error + 'static)> = Some(&error);
            let mut found_structural_reason = false;
            while let Some(chain_error) = current {
                if chain_error.to_string().contains("unexpected end of atom") {
                    found_structural_reason = true;
                    break;
                }
                current = chain_error.source();
            }
            assert!(found_structural_reason, "open-time truncation: {error:?}");
            return;
        }
    };

    for _ in 0..8 {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(_))
            | Ok(DemuxReadEvent::MediaMetadataChanged(_))
            | Ok(DemuxReadEvent::TracksChanged(_)) => {}
            Ok(DemuxReadEvent::EndOfStream) => panic!("truncation не должна выглядеть как EOS"),
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                panic!("finite source не должен публиковать readiness")
            }
            Err(error) => {
                assert!(
                    error
                        .downcast_ref::<DemuxError>()
                        .is_some_and(|demux_error| matches!(demux_error, DemuxError::Parse(_))),
                    "truncation должна остаться structural parse error: {error:#}"
                );
                return;
            }
        }
    }
    panic!("truncated source не завершился bounded error-ом");
}

#[test]
fn tfdt_nonzero_gap_preserves_exact_packet_dts() {
    let (init, first_media, second_media) = generated_segments();
    let mut second_media = second_media.to_vec();
    set_tfdt_base_decode_time(&mut second_media, 0, 4_096);
    let mut demuxer = open_ordered(
        vec![
            segment(10, OrderedSegmentKind::Initialization, init),
            segment(20, OrderedSegmentKind::Media, first_media),
            segment(30, OrderedSegmentKind::Media, Bytes::from(second_media)),
        ],
        CancellationToken::never_cancelled(),
        None,
    )
    .expect("open gapped ordered fMP4");

    let first = next_packet(demuxer.as_mut());
    let second = next_packet(demuxer.as_mut());
    assert_eq!(first.track_dts.expect("first DTS").units.get(), 0);
    assert_eq!(second.track_dts.expect("second DTS").units.get(), 4_096);
    assert_eq!(second.track_pts.expect("second PTS").units.get(), 4_096);
}

#[test]
fn fragmented_audio_seek_rolls_gap_target_back_by_timestamp() {
    let mut fixture = decode_base64_fixture();
    set_tfdt_base_decode_time(&mut fixture, 1, 4_096);
    let (_fixture_guard, mut demuxer) = open_local(&fixture);

    let seeked = demuxer
        .seek(Duration::from_millis(300))
        .expect("seek target внутри tfdt gap");

    assert_eq!(seeked.actual_position.as_duration(), Duration::ZERO);
    assert_eq!(
        seeked
            .actual_track_timestamp
            .expect("raw seek timestamp")
            .units
            .get(),
        0
    );
}

#[test]
fn fragmented_audio_seek_uses_original_fixture_flags_without_rewrite() {
    let fixture = decode_base64_fixture();
    let (_fixture_guard, mut demuxer) = open_local(&fixture);

    let seeked = demuxer
        .seek(Duration::from_millis(150))
        .expect("audio seek не должен требовать video RAP flags");

    assert_eq!(
        seeked.actual_position.as_duration(),
        Duration::from_millis(128)
    );
    assert_eq!(
        seeked
            .actual_track_timestamp
            .expect("raw seek timestamp")
            .units
            .get(),
        1_024
    );
}

#[test]
fn fragmented_duration_is_unknown_without_authority_and_uses_mehd_when_present() {
    let (init, first_media, second_media) = generated_segments();
    let unknown = open_ordered(
        vec![
            segment(1, OrderedSegmentKind::Initialization, init.clone()),
            segment(2, OrderedSegmentKind::Media, first_media.clone()),
            segment(3, OrderedSegmentKind::Media, second_media.clone()),
        ],
        CancellationToken::never_cancelled(),
        None,
    )
    .expect("open duration-unknown fMP4");
    assert_eq!(unknown.duration(), None);
    assert!(
        unknown
            .tracks()
            .iter()
            .all(|track| track.duration.is_none())
    );

    let mut authoritative_init = init.to_vec();
    insert_mehd_duration(&mut authoritative_init, 3_000);
    let authoritative = open_ordered(
        vec![
            segment(
                1,
                OrderedSegmentKind::Initialization,
                Bytes::from(authoritative_init),
            ),
            segment(2, OrderedSegmentKind::Media, first_media),
            segment(3, OrderedSegmentKind::Media, second_media),
        ],
        CancellationToken::never_cancelled(),
        None,
    )
    .expect("open mehd-authoritative fMP4");
    assert_eq!(authoritative.duration(), Some(Duration::from_secs(3)));
}

#[test]
fn clean_eof_and_truncated_fragment_structures_are_distinct() {
    let (_, first_media, _) = generated_segments();
    let moof_type = box_type_positions(&first_media, b"moof")[0];
    let traf_type = box_type_positions(&first_media, b"traf")[0];
    let trun_type = box_type_positions(&first_media, b"trun")[0];
    let mdat_type = box_type_positions(&first_media, b"mdat")[0];

    assert_structural_truncation(&first_media[..moof_type + 2]);
    assert_structural_truncation(&first_media[..traf_type + 6]);
    assert_structural_truncation(&first_media[..trun_type + 10]);
    assert_structural_truncation(&first_media[..mdat_type + 12]);
}

#[test]
fn local_full_fixture_still_reaches_clean_eof() {
    let fixture = decode_base64_fixture();
    let (_fixture_guard, mut demuxer) = open_local(&fixture);
    let mut packet_count = 0;

    for _ in 0..16 {
        match demuxer.next_event().expect("read complete fMP4") {
            DemuxReadEvent::Packet(_) => packet_count += 1,
            DemuxReadEvent::EndOfStream => {
                assert_eq!(packet_count, 3);
                return;
            }
            DemuxReadEvent::MediaMetadataChanged(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("finite local fMP4 не должен публиковать readiness")
            }
        }
    }
    panic!("complete fixture не достиг clean EOF");
}
