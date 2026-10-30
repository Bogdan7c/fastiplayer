//! Учёт номеров описаний дорожек: шлагбаум очереди видео, переход звука, сбросы.

use super::*;
use crate::pipeline::track_description::MAX_PENDING_TRACK_DESCRIPTIONS;

const VIDEO_TRACK: TrackId = TrackId::new(1);
const AUDIO_TRACK: TrackId = TrackId::new(2);

fn video_description(codec_id: &str) -> TrackInfo {
    source_slot_track(VIDEO_TRACK, TrackKind::Video, codec_id)
}

fn audio_description(codec_id: &str) -> TrackInfo {
    source_slot_track(AUDIO_TRACK, TrackKind::Audio, codec_id)
}

fn video_packet(pts_ms: u64, description: TrackDescriptionEpoch) -> PendingVideoPacket {
    PendingVideoPacket::new(
        VIDEO_TRACK,
        Duration::from_millis(pts_ms),
        0,
        Bytes::from_static(b"video"),
        true,
    )
    .with_track_description(description)
}

fn admitted(admission: TrackDescriptionAdmission) -> TrackDescriptionEpoch {
    match admission {
        TrackDescriptionAdmission::Admitted(epoch) => epoch,
        TrackDescriptionAdmission::QueueFull => panic!("description must be admitted"),
    }
}

/// Без смен ничего не меняется: очередь отдаёт packet-ы как раньше.
#[test]
fn without_track_changes_the_gate_is_open_and_no_handoff_is_pending() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_video_packet(video_packet(0, TrackDescriptionEpoch::INITIAL));

    assert_eq!(
        pipeline
            .front_sendable_video_packet()
            .map(|packet| packet.pts),
        Some(Duration::ZERO)
    );
    assert!(pipeline.pending_video_track_handoff().is_none());
    assert!(!pipeline.front_video_packet_has_unknown_track_description());
}

/// Packet нового описания закрывает шлагбаум, пока decoder не передан.
#[test]
fn packet_of_a_newer_description_waits_for_the_decoder_handoff() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_video_packet(video_packet(0, TrackDescriptionEpoch::INITIAL));
    let next = admitted(pipeline.admit_video_track_description(video_description("V_VP9")));
    pipeline.enqueue_pending_video_packet(video_packet(40, next));

    // Старый packet ещё уходит старому decoder-у.
    assert_eq!(
        pipeline
            .pop_pending_video_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::ZERO)
    );
    // Новый — нет: для send loop очередь пуста, а передача ждёт своё описание.
    assert!(pipeline.front_sendable_video_packet().is_none());
    assert_eq!(
        pipeline
            .pending_video_track_handoff()
            .map(|track| track.codec_id.as_str()),
        Some("V_VP9")
    );
    assert_eq!(pipeline.pending_video_packet_len(), 1, "the packet is kept");

    pipeline.complete_video_track_handoff();

    assert_eq!(
        pipeline
            .front_sendable_video_packet()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(40))
    );
    assert!(pipeline.pending_video_track_handoff().is_none());
}

/// Две смены подряд: каждая граница передаёт decoder своему описанию по порядку.
#[test]
fn consecutive_changes_hand_the_decoder_over_one_description_at_a_time() {
    let mut pipeline = PlaybackPipeline::default();
    let second = admitted(pipeline.admit_video_track_description(video_description("V_SECOND")));
    pipeline.enqueue_pending_video_packet(video_packet(0, second));
    let third = admitted(pipeline.admit_video_track_description(video_description("V_THIRD")));
    pipeline.enqueue_pending_video_packet(video_packet(40, third));

    assert_eq!(
        pipeline
            .pending_video_track_handoff()
            .map(|track| track.codec_id.as_str()),
        Some("V_SECOND")
    );
    pipeline.complete_video_track_handoff();
    pipeline.pop_pending_video_packet_front();

    assert_eq!(
        pipeline
            .pending_video_track_handoff()
            .map(|track| track.codec_id.as_str()),
        Some("V_THIRD")
    );
}

/// Описание без единого своего packet-а (сразу сменилось ещё раз) пропускается.
#[test]
fn description_without_packets_is_skipped_by_the_next_handoff() {
    let mut pipeline = PlaybackPipeline::default();
    admitted(pipeline.admit_video_track_description(video_description("V_SKIPPED")));
    let latest = admitted(pipeline.admit_video_track_description(video_description("V_LATEST")));
    pipeline.enqueue_pending_video_packet(video_packet(0, latest));

    assert_eq!(
        pipeline
            .pending_video_track_handoff()
            .map(|track| track.codec_id.as_str()),
        Some("V_LATEST")
    );
    pipeline.complete_video_track_handoff();
    assert!(pipeline.pending_video_track_handoff().is_none());
    assert!(pipeline.can_admit_video_track_description());
}

/// Очередь описаний ограничена: страница-враг не копит их без конца.
#[test]
fn description_queue_is_bounded_per_kind() {
    let mut pipeline = PlaybackPipeline::default();
    for index in 0..MAX_PENDING_TRACK_DESCRIPTIONS {
        admitted(pipeline.admit_video_track_description(video_description(&format!("V_{index}"))));
    }

    let last_admitted = pipeline.admitted_video_track_description();

    assert!(!pipeline.can_admit_video_track_description());
    assert_eq!(
        pipeline.admit_video_track_description(video_description("V_OVERFLOW")),
        TrackDescriptionAdmission::QueueFull
    );
    // Отказ ничего не меняет: новые packet-ы помечаются прежним номером.
    assert_eq!(pipeline.admitted_video_track_description(), last_admitted);
    // Звук считается отдельно.
    assert!(pipeline.can_admit_audio_track_description());
}

/// После seek flush decoder сразу берёт последнее описание, без выжимания.
#[test]
fn settle_after_flush_takes_the_latest_description_and_opens_the_gate() {
    let mut pipeline = PlaybackPipeline::default();
    admitted(pipeline.admit_video_track_description(video_description("V_OLDER")));
    let latest = admitted(pipeline.admit_video_track_description(video_description("V_LATEST")));

    assert_eq!(
        pipeline
            .settle_video_track_descriptions_after_flush()
            .map(|track| track.codec_id),
        Some("V_LATEST".to_owned())
    );
    pipeline.enqueue_pending_video_packet(video_packet(0, latest));
    assert!(pipeline.front_sendable_video_packet().is_some());
    assert!(
        pipeline
            .settle_video_track_descriptions_after_flush()
            .is_none(),
        "nothing left to settle"
    );
}

/// Звук переходит на новое описание ровно на первом своём packet-е.
#[test]
fn audio_description_is_taken_once_at_its_first_packet() {
    let mut pipeline = PlaybackPipeline::default();
    let next = admitted(pipeline.admit_audio_track_description(audio_description("A_OPUS")));

    assert!(
        pipeline
            .take_audio_track_description_for_packet(TrackDescriptionEpoch::INITIAL)
            .is_none(),
        "a packet of the old description keeps the old decoder"
    );
    assert_eq!(
        pipeline
            .take_audio_track_description_for_packet(next)
            .map(|track| track.codec_id),
        Some("A_OPUS".to_owned())
    );
    assert!(
        pipeline
            .take_audio_track_description_for_packet(next)
            .is_none(),
        "the next packet of the same description does not switch again"
    );
}

/// Замена списка эстафетой не трогает чужое: decoders, очереди и выбор дорожек.
#[test]
fn replacing_the_track_list_keeps_queues_and_selection() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(AUDIO_TRACK);
    pipeline.enqueue_pending_video_packet(video_packet(0, TrackDescriptionEpoch::INITIAL));
    let generation = pipeline.seek_generation();

    pipeline.replace_track_list_keeping_decoders(vec![
        video_description("V_VP9"),
        audio_description("A_OPUS"),
    ]);

    assert_eq!(pipeline.tracks().len(), 2);
    assert_eq!(pipeline.pending_video_packet_len(), 1);
    assert_eq!(pipeline.selected_audio_track_id(), Some(AUDIO_TRACK));
    assert_eq!(pipeline.seek_generation(), generation);
}

/// Старый сброс и сброс media обнуляют учёт: всё снова одного описания.
#[test]
fn full_resets_forget_pending_descriptions() {
    let mut pipeline = PlaybackPipeline::default();
    admitted(pipeline.admit_video_track_description(video_description("V_VP9")));
    admitted(pipeline.admit_audio_track_description(audio_description("A_OPUS")));

    pipeline.apply_demux_track_list_update(vec![video_description("V_VP9")]);

    assert_eq!(
        pipeline.admitted_video_track_description(),
        TrackDescriptionEpoch::INITIAL
    );
    assert_eq!(
        pipeline.admitted_audio_track_description(),
        TrackDescriptionEpoch::INITIAL
    );

    admitted(pipeline.admit_video_track_description(video_description("V_AV1")));
    pipeline.reset_media_slots();
    assert_eq!(
        pipeline.admitted_video_track_description(),
        TrackDescriptionEpoch::INITIAL
    );
}
