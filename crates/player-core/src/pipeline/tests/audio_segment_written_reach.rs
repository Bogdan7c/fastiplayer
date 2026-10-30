//! Boundary `audio_segment_written_reach`: докуда дошёл PCM текущего отрезка звука.
//!
//! Session спрашивает это перед forward extension живого скраба (дефект D2): очистка
//! output-а выбросит записанный PCM, а demux его повторно не прочитает.

use super::*;
use crate::pipeline::audio_segment_start::AudioSegmentWrittenReach;

/// Pipeline с выбранным звуком и fake output-ом, в который можно писать PCM.
fn pipeline_with_selected_audio_output() -> PlaybackPipeline {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(TrackId::new(2));
    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(0.0)));
    pipeline
}

/// Пишет 100 мс stereo PCM (4800 frames) packet-а с началом `packet_start`, как session.
fn write_100ms_packet(pipeline: &mut PlaybackPipeline, packet_start: Duration) {
    let samples = vec![0.25_f32; 9_600];
    let status =
        pipeline.write_audio_output_samples(&samples, AudioOutputWriteIntent::DirectDecodedPcm);
    assert!(matches!(status, AudioOutputRoutingStatus::Written(_)));
    pipeline.note_audio_packet_written(
        packet_start,
        samples.len(),
        audio_output_spec_for_tests(48_000, 2),
    );
}

#[test]
fn absent_audio_track_has_nothing_to_lose() {
    let pipeline = PlaybackPipeline::default();

    assert_eq!(
        pipeline.audio_segment_written_reach(),
        AudioSegmentWrittenReach::NoSelectedAudio
    );
}

#[test]
fn selected_audio_without_written_pcm_reports_empty_segment() {
    let pipeline = pipeline_with_selected_audio_output();

    assert_eq!(
        pipeline.audio_segment_written_reach(),
        AudioSegmentWrittenReach::NoPcmWritten
    );
}

#[test]
fn written_pcm_reports_its_media_end() {
    let mut pipeline = pipeline_with_selected_audio_output();

    write_100ms_packet(&mut pipeline, Duration::from_secs(8));
    write_100ms_packet(&mut pipeline, Duration::from_millis(8_100));

    assert_eq!(
        pipeline.audio_segment_written_reach(),
        AudioSegmentWrittenReach::EndsAt(Duration::from_millis(8_200))
    );
}

#[test]
fn seek_clear_of_the_output_starts_an_empty_segment() {
    let mut pipeline = pipeline_with_selected_audio_output();
    write_100ms_packet(&mut pipeline, Duration::from_secs(8));
    let generation = pipeline.seek_generation();

    let clear_result = pipeline.clear_audio_output_for_seek(generation);

    assert!(matches!(clear_result, Some(Ok(_))));
    assert_eq!(
        pipeline.audio_segment_written_reach(),
        AudioSegmentWrittenReach::NoPcmWritten,
        "the ring is empty: a new segment loses nothing"
    );
}

#[test]
fn forgotten_end_of_written_pcm_is_reported_as_unknown() {
    let mut pipeline = pipeline_with_selected_audio_output();
    write_100ms_packet(&mut pipeline, Duration::from_secs(8));

    // Seek commit / смена скорости перепривязывают часы и забывают конец звука,
    // хотя PCM отрезка всё ещё лежит в output-е.
    pipeline.reanchor_audio_clock_media_mapping(Duration::from_secs(8), PlaybackRate::NORMAL);

    assert_eq!(
        pipeline.audio_segment_written_reach(),
        AudioSegmentWrittenReach::EndUnknown
    );
}

#[test]
fn reading_the_reach_changes_neither_segment_nor_written_end() {
    let mut pipeline = pipeline_with_selected_audio_output();
    write_100ms_packet(&mut pipeline, Duration::from_secs(8));
    let pending_before = pipeline.pending_audio_packet_len();

    let first_read = pipeline.audio_segment_written_reach();
    let second_read = pipeline.audio_segment_written_reach();

    assert_eq!(first_read, second_read);
    assert!(!pipeline.audio_segment_awaits_first_pcm());
    assert_eq!(
        pipeline.written_audio_end(),
        Some(Duration::from_millis(8_100))
    );
    assert_eq!(pipeline.pending_audio_packet_len(), pending_before);
    assert!(pipeline.has_audio_output());
}
