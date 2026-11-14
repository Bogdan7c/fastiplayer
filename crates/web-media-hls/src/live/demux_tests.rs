//! Юнит-тесты демуксера компонента живого эфира (вынесены из `demux.rs` в 16d3).

use media_core::TrackKind;

use super::*;

fn video_track(id: u32, codec_id: &str, duration: Option<Duration>) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(id),
        kind: TrackKind::Video,
        codec_id: codec_id.to_owned(),
        codec_private: None,
        time_base: None,
        duration,
        sample_rate: None,
        channels: None,
        video: None,
        labels: media_core::TrackLabels::default(),
    }
}

#[test]
fn tracks_changed_is_driven_by_actual_config_not_segment_local_ids() {
    let stable = vec![video_track(7, "h264", None)];
    let same_config = vec![video_track(99, "h264", Some(Duration::from_secs(6)))];
    let stabilized = stabilize_tracks(&stable, &same_config).expect("same topology");
    assert_eq!(stabilized, stable);

    let changed_config = vec![video_track(100, "h265", Some(Duration::from_secs(6)))];
    let stabilized = stabilize_tracks(&stable, &changed_config).expect("same track kind");
    assert_ne!(stabilized, stable);

    let audio_topology = TrackInfo {
        kind: TrackKind::Audio,
        labels: media_core::TrackLabels::default(),
        ..video_track(1, "aac", None)
    };
    assert!(stabilize_tracks(&stable, &[audio_topology]).is_err());
}

#[test]
fn key_cache_is_reused_only_inside_one_accepted_snapshot_generation() {
    let edge = Duration::from_secs(18);
    let generation = SourceGeneration::new(4);
    assert!(!key_cache_requires_reset(
        edge, generation, edge, generation
    ));
    assert!(key_cache_requires_reset(
        edge,
        generation,
        Duration::from_secs(24),
        generation,
    ));
    assert!(key_cache_requires_reset(
        edge,
        generation,
        edge,
        SourceGeneration::new(5),
    ));
}
