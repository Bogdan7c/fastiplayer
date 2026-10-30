//! Классификация `TracksChanged`: что поменялось у выбранных дорожек.

use bytes::Bytes;
use media_core::{TrackId, TrackInfo, TrackKind};

use super::{
    ChangedSelectedDescriptions, SelectedDescriptionChange, SelectedTrackIds,
    classify_selected_description_change,
};

fn track(track_id: u32, kind: TrackKind, codec_id: &str) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(track_id),
        kind,
        codec_id: codec_id.to_owned(),
        codec_private: None,
        time_base: media_core::TimeBase::new(1, 90_000),
        duration: Some(std::time::Duration::from_secs(10)),
        sample_rate: (kind == TrackKind::Audio).then_some(48_000),
        channels: (kind == TrackKind::Audio).then_some(2),
        video: None,
        labels: media_core::TrackLabels::default(),
    }
}

fn video_and_audio() -> Vec<TrackInfo> {
    vec![
        track(1, TrackKind::Video, "V_MPEG4/ISO/AVC"),
        track(2, TrackKind::Audio, "A_AAC"),
    ]
}

const BOTH_SELECTED: SelectedTrackIds = SelectedTrackIds {
    video: Some(TrackId::new(1)),
    audio: Some(TrackId::new(2)),
};

fn same_topology(video: Option<TrackInfo>, audio: Option<TrackInfo>) -> SelectedDescriptionChange {
    SelectedDescriptionChange::SameTopology(Box::new(ChangedSelectedDescriptions { video, audio }))
}

#[test]
fn new_parameter_sets_change_only_the_video_description() {
    let previous = video_and_audio();
    let mut next = previous.clone();
    next[0].codec_private = Some(Bytes::from_static(b"avcC with 96x96 SPS"));

    assert_eq!(
        classify_selected_description_change(&previous, &next, BOTH_SELECTED),
        same_topology(Some(next[0].clone()), None)
    );
}

#[test]
fn audio_codec_change_changes_only_the_audio_description() {
    let previous = video_and_audio();
    let mut next = previous.clone();
    next[1].codec_id = "A_OPUS".to_owned();

    assert_eq!(
        classify_selected_description_change(&previous, &next, BOTH_SELECTED),
        same_topology(None, Some(next[1].clone()))
    );
}

/// Длительность и метки на decoder не влияют: выжимать decoder ради них нельзя.
#[test]
fn duration_and_labels_alone_are_not_a_decoder_description_change() {
    let previous = video_and_audio();
    let mut next = previous.clone();
    next[0].duration = Some(std::time::Duration::from_secs(99));
    next[1].labels.title = Some("Ukrainian dub".to_owned());

    assert_eq!(
        classify_selected_description_change(&previous, &next, BOTH_SELECTED),
        same_topology(None, None)
    );
}

#[test]
fn replaced_track_identity_is_a_topology_change() {
    let previous = video_and_audio();
    let mut next = previous.clone();
    next[0].id = TrackId::new(3);

    assert_eq!(
        classify_selected_description_change(&previous, &next, BOTH_SELECTED),
        SelectedDescriptionChange::TopologyChanged
    );
}

#[test]
fn added_audio_track_is_a_topology_change() {
    let previous = video_and_audio();
    let mut next = previous.clone();
    next.push(track(4, TrackKind::Audio, "A_AAC"));

    assert_eq!(
        classify_selected_description_change(&previous, &next, BOTH_SELECTED),
        SelectedDescriptionChange::TopologyChanged
    );
}

/// Субтитры не декодируются этими decoder-ами: их появление эстафете не мешает.
#[test]
fn subtitle_track_changes_do_not_block_the_handoff() {
    let previous = video_and_audio();
    let mut next = previous.clone();
    next.push(track(5, TrackKind::Subtitle, "S_TEXT/UTF8"));

    assert_eq!(
        classify_selected_description_change(&previous, &next, BOTH_SELECTED),
        same_topology(None, None)
    );
}

/// Звук есть, но не выбран (отключён ошибкой): прежний сброс выбирает заново.
#[test]
fn unselected_existing_kind_requires_the_full_reset() {
    let previous = video_and_audio();
    let next = previous.clone();
    let audio_not_selected = SelectedTrackIds {
        video: Some(TrackId::new(1)),
        audio: None,
    };

    assert_eq!(
        classify_selected_description_change(&previous, &next, audio_not_selected),
        SelectedDescriptionChange::TopologyChanged
    );
}

#[test]
fn audio_only_media_without_video_selection_is_still_the_same_topology() {
    let previous = vec![track(2, TrackKind::Audio, "A_AAC")];
    let mut next = previous.clone();
    next[0].sample_rate = Some(44_100);
    let audio_only = SelectedTrackIds {
        video: None,
        audio: Some(TrackId::new(2)),
    };

    assert_eq!(
        classify_selected_description_change(&previous, &next, audio_only),
        same_topology(None, Some(next[0].clone()))
    );
}
