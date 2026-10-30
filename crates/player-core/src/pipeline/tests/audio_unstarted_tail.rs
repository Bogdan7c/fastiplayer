//! Boundary «в output-е лежит незапущенный хвост звука» (`audio_output_has_unstarted_tail`).
//!
//! По нему session запускает EOF-хвост, даже когда в очереди ещё лежит сжатый звук:
//! без `play` уровень output-а не падает и остаток не декодируется (вечный `Draining`).

use super::*;

/// Выбранная аудиодорожка стенда.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// Сжатый packet выбранной дорожки (ждёт декода в очереди).
fn queued_audio_packet() -> PendingAudioPacket {
    PendingAudioPacket::new_unbounded(
        AUDIO_TRACK,
        Duration::from_secs(1),
        None,
        None,
        0,
        Bytes::from_static(b"compressed-audio"),
    )
}

/// Pipeline с выбранным звуком и output-ом заданного уровня, звук ещё не запускали.
fn pipeline_with_output(buffer_level_ms: f64) -> PlaybackPipeline {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(AUDIO_TRACK);
    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(buffer_level_ms)));
    pipeline
}

#[test]
fn absent_output_has_no_tail() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(AUDIO_TRACK);

    assert!(!pipeline.audio_output_has_unstarted_tail());
}

#[test]
fn output_without_selected_audio_has_no_tail() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(300.0)));

    assert!(!pipeline.audio_output_has_unstarted_tail());
}

/// Пустой output — не хвост: запуск без звука подменил бы часы.
#[test]
fn empty_output_is_not_a_tail_even_with_queued_audio() {
    let mut pipeline = pipeline_with_output(0.0);
    pipeline.enqueue_pending_audio_packet(queued_audio_packet());

    assert!(!pipeline.audio_output_has_unstarted_tail());
}

/// Главный случай: сжатый звук в очереди не мешает признать незапущенный хвост.
#[test]
fn buffered_output_is_an_unstarted_tail_despite_queued_audio() {
    let mut pipeline = pipeline_with_output(1_500.0);
    pipeline.enqueue_pending_audio_packet(queued_audio_packet());

    assert!(pipeline.audio_output_has_unstarted_tail());
    assert!(
        matches!(
            pipeline.audio_eof_drain_state(),
            AudioEofDrainState::PendingPackets { queued_packets: 1 }
        ),
        "the old EOF state still reports the queued packet"
    );
}

/// Запрос никого не запускает и не трогает очередь; после `play` хвост уже запущен.
#[test]
fn query_changes_nothing_and_play_marks_the_tail_started() {
    let mut pipeline = pipeline_with_output(400.0);
    pipeline.enqueue_pending_audio_packet(queued_audio_packet());

    assert!(pipeline.audio_output_has_unstarted_tail());
    assert!(pipeline.audio_output_has_unstarted_tail());
    assert_eq!(pipeline.pending_audio_packet_len(), 1);
    assert!(pipeline.audio_output_needs_play_request());

    let play_result = pipeline.play_audio_output().expect("output exists");
    assert!(play_result.is_ok());
    assert!(!pipeline.audio_output_has_unstarted_tail());
    assert_eq!(pipeline.pending_audio_packet_len(), 1);
}

/// Пауза (заморозка ожиданием источника) снова делает хвост незапущенным.
#[test]
fn paused_output_with_buffered_audio_is_an_unstarted_tail_again() {
    let mut pipeline = pipeline_with_output(400.0);
    let play_result = pipeline.play_audio_output().expect("output exists");
    assert!(play_result.is_ok());
    assert!(!pipeline.audio_output_has_unstarted_tail());

    let pause_result = pipeline
        .pause_audio_output_and_capture_clock()
        .expect("output exists");
    assert!(pause_result.is_ok());

    assert!(pipeline.audio_output_has_unstarted_tail());
}
