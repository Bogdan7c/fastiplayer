//! S28C proof для current audio-container families.

use std::io::Cursor;
use std::num::NonZeroUsize;
use std::time::Duration;

use demux_api::{
    DemuxFactory, DemuxHints, DemuxInput, DemuxOpenError, DemuxProbeDecision, DemuxProbeRequest,
    DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, Demuxer, MediaDemuxError, Packet, TrackKind,
};
use source_core::{CancellationToken, LocalFileSource};

use super::audio_fixtures::{AudioContainerFixture, chained_ogg_opus, fixtures};
use super::{ContainerDetection, SymphoniaDemuxFactory, TemporaryMediaFile, detect_container};
use crate::DemuxerOptions;

/// Единый bounded sniff budget достаточен для всех compact S28C fixtures.
fn sniff_budget() -> DemuxSniffBudget {
    DemuxSniffBudget::new(
        NonZeroUsize::new(4_096).expect("S28C sniff bytes ненулевые"),
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    )
    .expect("S28C sniff budget валиден")
}

/// Создаёт registry с единственным production Symphonia factory.
fn registry() -> DemuxRegistry {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("S28C factory"),
        ))
        .expect("S28C factory registration");
    registry
}

/// Открывает generated fixture как seekable local byte source без extension hint-а.
fn open_local(fixture: &AudioContainerFixture) -> (TemporaryMediaFile, Box<dyn Demuxer + Send>) {
    let fixture_file = TemporaryMediaFile::new("bin", &fixture.bytes);
    let source = LocalFileSource::open(&fixture_file.path).expect("open S28C local fixture");
    let demuxer = registry()
        .open(
            DemuxInput::byte_source(Box::new(source)),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::new(),
        )
        .expect("open S28C local fixture by signature");
    (fixture_file, demuxer)
}

/// Открывает те же bytes как forward-only progressive input.
fn open_streaming(fixture: &AudioContainerFixture) -> Box<dyn Demuxer + Send> {
    registry()
        .open(
            DemuxInput::byte_stream(Box::new(Cursor::new(fixture.bytes.clone()))),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::new(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "open {} S28C forward-only fixture by signature: {error:#}",
                fixture.extension
            )
        })
}

/// Возвращает следующий packet, сохраняя lifecycle events наблюдаемыми.
fn next_packet(demuxer: &mut dyn Demuxer) -> Packet {
    loop {
        match demuxer.next_event().expect("read S28C packet") {
            DemuxReadEvent::Packet(packet) => return packet,
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("S28C fixture закончился до первого packet-а"),
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("blocking Symphonia reader не публикует temporary readiness")
            }
        }
    }
}

/// Дочитывает finite fixture и отличает clean EOF от structural failure.
fn read_to_clean_eof(demuxer: &mut dyn Demuxer) {
    loop {
        match demuxer
            .next_event()
            .expect("finite S28C fixture должен читаться")
        {
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => return,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("finite S28C fixture не должен ждать readiness")
            }
        }
    }
}

/// Проверяет exact static track, codec-private и первый packet contract.
fn assert_fixture_contract(fixture: &AudioContainerFixture, demuxer: &mut dyn Demuxer) {
    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("S28C audio track");
    assert_eq!(
        audio_track.codec_id, fixture.codec_id,
        "{}",
        fixture.extension
    );
    assert_eq!(audio_track.sample_rate, Some(fixture.sample_rate));
    assert_eq!(audio_track.channels, Some(1));
    assert_eq!(
        audio_track.codec_private.as_deref(),
        fixture.codec_private.as_deref(),
        "{} codec private",
        fixture.extension
    );
    let audio_track_id = audio_track.id;
    let packet = next_packet(demuxer);
    assert_eq!(packet.track_id, audio_track_id);
    assert_eq!(packet.kind, TrackKind::Audio);
    assert_eq!(packet.data.as_ref(), fixture.first_packet.as_slice());
    assert_eq!(
        packet.byte_offset, None,
        "{} generic Symphonia reader не должен выдумывать source offset",
        fixture.extension
    );
    assert_eq!(packet.track_pts.expect("packet PTS").units.get(), 0);
    assert_eq!(packet.track_dts.expect("packet DTS").units.get(), 0);
    assert_eq!(
        packet.track_duration.expect("packet duration").units.get(),
        fixture.first_packet_duration_units
    );
}

/// Все family открываются local по signature, публикуют exact packet contract и seek.
#[test]
fn local_audio_families_preserve_tracks_private_data_packets_duration_and_seek() {
    for fixture in fixtures() {
        let (_fixture_file, mut demuxer) = open_local(&fixture);
        assert!(matches!(demuxer.seekability(), DemuxSeekability::Seekable));
        assert_eq!(
            demuxer.duration().is_some(),
            fixture.duration_is_known,
            "{} known/unknown duration",
            fixture.extension
        );
        assert_fixture_contract(&fixture, demuxer.as_mut());
        let seek = demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .unwrap_or_else(|error| panic!("{} seek failed: {error:#}", fixture.extension));
        assert_eq!(seek.requested_position.as_duration(), Duration::ZERO);
        let replayed = next_packet(demuxer.as_mut());
        assert_eq!(replayed.data.as_ref(), fixture.first_packet.as_slice());
        read_to_clean_eof(demuxer.as_mut());
    }
}

/// Forward-only row сохраняет packet playback, но публикует typed non-seekable boundary.
#[test]
fn streaming_audio_families_reject_seek_and_continue_to_clean_eof() {
    for fixture in fixtures() {
        let mut demuxer = open_streaming(&fixture);
        assert!(matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable { .. }
        ));
        assert_eq!(
            demuxer.duration().is_some(),
            fixture.streaming_duration_is_known,
            "{} forward-only duration authority",
            fixture.extension
        );
        let seek_error = demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .expect_err("forward-only S28C input не должен seek-аться");
        assert!(matches!(
            seek_error.downcast_ref::<MediaDemuxError>(),
            Some(MediaDemuxError::SeekUnavailable { .. })
        ));
        assert_fixture_contract(&fixture, demuxer.as_mut());
        read_to_clean_eof(demuxer.as_mut());
    }
}

/// Content signature авторитетнее заведомо конфликтующего ISO BMFF hint-а.
#[test]
fn every_audio_family_sniffs_without_extension_and_overrides_conflicting_hint() {
    let factory = SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("S28C factory");
    for fixture in fixtures() {
        let conflicting_hints = DemuxHints::none().with_extension(
            DemuxSourceExtension::new("mp4").expect("conflicting extension валиден"),
        );
        let decision = factory.probe(DemuxProbeRequest {
            hints: &conflicting_hints,
            sniffed_bytes: &fixture.bytes,
            input_capability: demux_api::DemuxInputCapability::SeekableBytes,
            cancellation: &CancellationToken::never_cancelled(),
        });
        let DemuxProbeDecision::Match(matched) = decision else {
            panic!(
                "{} signature должна победить hint: {decision:?}",
                fixture.extension
            );
        };
        assert_eq!(matched.container.as_str(), fixture.container_id);
        assert_eq!(
            matched.hint_relationship,
            demux_api::DemuxHintRelationship::Disagrees
        );
    }
}

/// Узнанная signature с обрезанным body не превращается в clean EOS или no-match.
#[test]
fn malformed_audio_families_fail_during_probe_or_backend_open() {
    for fixture in fixtures() {
        let recognized_prefix_len = match fixture.container_id {
            "wave" | "aiff" => 12,
            "mpeg-audio" => 4,
            _ => 4,
        };
        let error = match registry().open(
            DemuxInput::byte_stream(Box::new(Cursor::new(
                fixture.bytes[..recognized_prefix_len].to_vec(),
            ))),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::new(),
        ) {
            Ok(_) => panic!("recognized truncated fixture должен fail-нуться"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                DemuxOpenError::ProbeRejected(_) | DemuxOpenError::FactoryRejected { .. }
            ),
            "{} malformed error: {error:?}",
            fixture.extension
        );
    }
}

/// Cancellation проверяется до sniff/open для каждой advertised family row.
#[test]
fn cancellation_before_sniff_rejects_every_audio_family() {
    for fixture in fixtures() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = match registry().open(
            DemuxInput::byte_stream(Box::new(Cursor::new(fixture.bytes))),
            DemuxHints::none(),
            sniff_budget(),
            cancellation,
        ) {
            Ok(_) => panic!("cancelled S28C open должен fail-нуться"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            DemuxOpenError::ProbeRejected(demux_api::DemuxProbeRejection::Cancelled)
        ));
    }
}

/// MPEG sniff различает три legal layer-а и reserved version/layer false positives.
#[test]
fn mpeg_audio_sniff_accepts_layers_one_two_three_and_rejects_reserved_headers() {
    for valid_header in [[0xff, 0xff], [0xff, 0xfd], [0xff, 0xfb]] {
        assert!(matches!(
            detect_container(&valid_header, &DemuxHints::none()),
            ContainerDetection::Match("mpeg-audio")
        ));
    }
    for reserved_header in [[0xff, 0xeb], [0xff, 0xf9]] {
        assert!(matches!(
            detect_container(&reserved_header, &DemuxHints::none()),
            ContainerDetection::NoMatch
        ));
    }
}

/// Factory descriptor связывает все S28C rows с exact generated evidence IDs.
#[test]
fn descriptor_lists_exact_s28c_audio_fixture_ids() {
    let factory = SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("S28C factory");
    let fixture_ids = factory
        .descriptor()
        .fixture_ids
        .iter()
        .map(|fixture_id| fixture_id.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "symphonia/s28c-ogg-opus",
        "symphonia/s28c-caf-pcm",
        "symphonia/s28c-wave-pcm",
        "symphonia/s28c-aiff-pcm",
        "symphonia/s28c-native-flac",
        "symphonia/s28c-mpeg-layer-1",
        "symphonia/s28c-mpeg-layer-2",
        "symphonia/s28c-mpeg-layer-3",
    ] {
        assert!(
            fixture_ids.contains(&expected),
            "missing fixture ID {expected}"
        );
    }
}

/// Chained Ogg physical stream публикует TracksChanged до packet-а новой chain.
#[test]
fn chained_ogg_maps_reset_required_to_tracks_changed_before_next_packet() {
    let fixture_file = TemporaryMediaFile::new("bin", &chained_ogg_opus());
    let source = LocalFileSource::open(&fixture_file.path).expect("open chained Ogg fixture");
    let mut demuxer = registry()
        .open(
            DemuxInput::byte_source(Box::new(source)),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::new(),
        )
        .expect("open chained Ogg by signature");
    let mut packets_before_reset = 0_usize;
    let mut reset_seen = false;
    let mut packets_after_reset = 0_usize;
    loop {
        match demuxer.next_event().expect("read chained Ogg") {
            DemuxReadEvent::Packet(_) if reset_seen => packets_after_reset += 1,
            DemuxReadEvent::Packet(_) => packets_before_reset += 1,
            DemuxReadEvent::TracksChanged(update) => {
                assert!(!reset_seen, "chained Ogg должен дать один reset");
                assert_eq!(packets_before_reset, 1);
                let audio_track = update
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Audio)
                    .expect("updated chained Ogg audio track");
                assert_eq!(audio_track.codec_id, "A_OPUS");
                reset_seen = true;
            }
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("finite chained Ogg не должен ждать readiness")
            }
        }
    }
    assert!(reset_seen);
    assert_eq!(packets_after_reset, 1);
}
