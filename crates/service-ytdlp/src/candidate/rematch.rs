//! Service-owned compatibility rules для fresh candidate rematch-а.

use web_media_core::{
    AudioTrackDescriptor, CandidateDescriptor, ContainerIdentity, ContentProbedDescriptor,
    ContentProbedTrackEvidence, ContentProbedVideoHints, DynamicRange, NormalizedTransport,
    StreamLayout, VideoTrackDescriptor,
};

use super::model::{YtDlpNormalizedCandidate, YtDlpVideoColorEvidence};

/// Сохраняет обычный semantic rematch, но не использует слабый unknown layout как physical ID.
pub(super) fn semantic_layout_rematch_compatible(
    selected: &CandidateDescriptor,
    current: &CandidateDescriptor,
) -> bool {
    selected.layout() == current.layout()
        && (!matches!(selected.layout(), StreamLayout::ContentProbed(_))
            || selected.identity().format() == current.identity().format())
}

/// Разрешает только metadata drift одного physical single-resource format-а.
///
/// Обычный semantic rematch остаётся первичным и допускает refresh-stable смену
/// extractor format ID. Эта более узкая ветка нужна только когда один из
/// descriptor-ов `ContentProbed`: exact format ID становится physical anchor-ом,
/// а `Unknown` evidence разрешает повторно доказать фактические дорожки runtime-ом.
pub(super) fn content_probe_rematch_compatible(
    selected: &CandidateDescriptor,
    selected_video_color: Option<YtDlpVideoColorEvidence>,
    candidate: &YtDlpNormalizedCandidate,
) -> bool {
    let current = candidate.descriptor();
    if selected.identity().source() != current.identity().source()
        || selected.identity().generation() == current.identity().generation()
        || selected.identity().format() != current.identity().format()
        || selected_video_color != candidate.video_color_evidence()
        || candidate.component_count() != 1
    {
        return false;
    }

    single_resource_layouts_compatible(selected.layout(), current.layout())
}

/// Сравнивает только single-resource shapes и требует `ContentProbed` хотя бы с одной стороны.
fn single_resource_layouts_compatible(selected: &StreamLayout, current: &StreamLayout) -> bool {
    match (selected, current) {
        (StreamLayout::ContentProbed(left), StreamLayout::ContentProbed(right)) => {
            content_probed_descriptors_compatible(left, right)
        }
        (StreamLayout::ContentProbed(probed), StreamLayout::Muxed(concrete))
        | (StreamLayout::Muxed(concrete), StreamLayout::ContentProbed(probed)) => {
            content_probed_matches_concrete(
                probed,
                concrete.transport(),
                concrete.container(),
                Some(concrete.video()),
                Some(concrete.audio()),
            )
        }
        (StreamLayout::ContentProbed(probed), StreamLayout::VideoOnly(concrete))
        | (StreamLayout::VideoOnly(concrete), StreamLayout::ContentProbed(probed)) => {
            content_probed_matches_concrete(
                probed,
                concrete.transport(),
                concrete.container(),
                Some(concrete.video()),
                None,
            )
        }
        (StreamLayout::ContentProbed(probed), StreamLayout::AudioOnly(concrete))
        | (StreamLayout::AudioOnly(concrete), StreamLayout::ContentProbed(probed)) => {
            content_probed_matches_concrete(
                probed,
                concrete.transport(),
                concrete.container(),
                None,
                Some(concrete.audio()),
            )
        }
        _ => false,
    }
}

/// Сохраняет exact transport/container и допускает только compatible track evidence.
fn content_probed_descriptors_compatible(
    left: &ContentProbedDescriptor,
    right: &ContentProbedDescriptor,
) -> bool {
    left.transport() == right.transport()
        && left.container() == right.container()
        && left.probe_container() == right.probe_container()
        && video_evidence_compatible(
            left.video(),
            left.video_hints(),
            right.video(),
            right.video_hints(),
        )
        && audio_evidence_compatible(left.audio(), right.audio())
}

/// Сопоставляет deferred evidence с concrete muxed/video-only/audio-only topology.
fn content_probed_matches_concrete(
    probed: &ContentProbedDescriptor,
    transport: &NormalizedTransport,
    container: &ContainerIdentity,
    video: Option<&VideoTrackDescriptor>,
    audio: Option<&AudioTrackDescriptor>,
) -> bool {
    probed.transport() == transport
        && probed.container() == container
        && container.consistent_family().ok().flatten() == Some(probed.probe_container())
        && video_evidence_matches_concrete(probed.video(), probed.video_hints(), video)
        && audio_evidence_matches_concrete(probed.audio(), audio)
}

/// `Unknown` является runtime-reproof wildcard; declared/absent evidence остаётся exact.
fn video_evidence_compatible(
    left: &ContentProbedTrackEvidence<VideoTrackDescriptor>,
    left_hints: ContentProbedVideoHints,
    right: &ContentProbedTrackEvidence<VideoTrackDescriptor>,
    right_hints: ContentProbedVideoHints,
) -> bool {
    match (left, right) {
        (ContentProbedTrackEvidence::Unknown, ContentProbedTrackEvidence::Unknown) => {
            video_hints_compatible(left_hints, right_hints)
        }
        (ContentProbedTrackEvidence::Unknown, ContentProbedTrackEvidence::Absent) => {
            video_hints_allow_absent(left_hints)
        }
        (ContentProbedTrackEvidence::Absent, ContentProbedTrackEvidence::Unknown) => {
            video_hints_allow_absent(right_hints)
        }
        (ContentProbedTrackEvidence::Unknown, ContentProbedTrackEvidence::Declared(track)) => {
            video_hints_match_track(left_hints, track)
        }
        (ContentProbedTrackEvidence::Declared(track), ContentProbedTrackEvidence::Unknown) => {
            video_hints_match_track(right_hints, track)
        }
        (ContentProbedTrackEvidence::Absent, ContentProbedTrackEvidence::Absent) => true,
        (
            ContentProbedTrackEvidence::Declared(left),
            ContentProbedTrackEvidence::Declared(right),
        ) => left == right,
        (ContentProbedTrackEvidence::Absent, ContentProbedTrackEvidence::Declared(_))
        | (ContentProbedTrackEvidence::Declared(_), ContentProbedTrackEvidence::Absent) => false,
    }
}

/// Audio `Unknown` не несёт дополнительных hints; остальные evidence states exact.
fn audio_evidence_compatible(
    left: &ContentProbedTrackEvidence<AudioTrackDescriptor>,
    right: &ContentProbedTrackEvidence<AudioTrackDescriptor>,
) -> bool {
    match (left, right) {
        (ContentProbedTrackEvidence::Unknown, _)
        | (_, ContentProbedTrackEvidence::Unknown)
        | (ContentProbedTrackEvidence::Absent, ContentProbedTrackEvidence::Absent) => true,
        (
            ContentProbedTrackEvidence::Declared(left),
            ContentProbedTrackEvidence::Declared(right),
        ) => left == right,
        (ContentProbedTrackEvidence::Absent, ContentProbedTrackEvidence::Declared(_))
        | (ContentProbedTrackEvidence::Declared(_), ContentProbedTrackEvidence::Absent) => false,
    }
}

/// Проверяет evidence одной deferred video дорожки против concrete topology.
fn video_evidence_matches_concrete(
    evidence: &ContentProbedTrackEvidence<VideoTrackDescriptor>,
    hints: ContentProbedVideoHints,
    concrete: Option<&VideoTrackDescriptor>,
) -> bool {
    match (evidence, concrete) {
        (ContentProbedTrackEvidence::Unknown, Some(track)) => video_hints_match_track(hints, track),
        (ContentProbedTrackEvidence::Unknown, None) => video_hints_allow_absent(hints),
        (ContentProbedTrackEvidence::Absent, None) => true,
        (ContentProbedTrackEvidence::Declared(expected), Some(actual)) => expected == actual,
        (ContentProbedTrackEvidence::Absent, Some(_))
        | (ContentProbedTrackEvidence::Declared(_), None) => false,
    }
}

/// Проверяет evidence одной deferred audio дорожки против concrete topology.
fn audio_evidence_matches_concrete(
    evidence: &ContentProbedTrackEvidence<AudioTrackDescriptor>,
    concrete: Option<&AudioTrackDescriptor>,
) -> bool {
    match (evidence, concrete) {
        (ContentProbedTrackEvidence::Unknown, _) => true,
        (ContentProbedTrackEvidence::Absent, None) => true,
        (ContentProbedTrackEvidence::Declared(expected), Some(actual)) => expected == actual,
        (ContentProbedTrackEvidence::Absent, Some(_))
        | (ContentProbedTrackEvidence::Declared(_), None) => false,
    }
}

/// Два набора unknown video hints совместимы, если известные значения не конфликтуют.
fn video_hints_compatible(left: ContentProbedVideoHints, right: ContentProbedVideoHints) -> bool {
    optional_evidence_compatible(
        left.width().map(web_media_core::VideoWidth::pixels),
        right.width().map(web_media_core::VideoWidth::pixels),
    ) && optional_evidence_compatible(left.height(), right.height())
        && optional_evidence_compatible(left.frame_rate(), right.frame_rate())
        && optional_evidence_compatible(left.bitrate(), right.bitrate())
        && dynamic_range_compatible(left.dynamic_range(), right.dynamic_range())
}

/// Unknown hints ограничивают concrete track только теми полями, которые были известны.
fn video_hints_match_track(hints: ContentProbedVideoHints, track: &VideoTrackDescriptor) -> bool {
    optional_hint_matches(
        hints.width().map(web_media_core::VideoWidth::pixels),
        track.width_pixels(),
    ) && optional_hint_matches(hints.height(), track.height())
        && optional_hint_matches(hints.frame_rate(), track.frame_rate())
        && optional_hint_matches(hints.bitrate(), track.bitrate())
        && (hints.dynamic_range() == DynamicRange::Unknown
            || hints.dynamic_range() == track.dynamic_range())
}

/// Unknown video может уточниться до absent только без противоречащих visual hints.
fn video_hints_allow_absent(hints: ContentProbedVideoHints) -> bool {
    hints.width().is_none()
        && hints.height().is_none()
        && hints.frame_rate().is_none()
        && hints.bitrate().is_none()
        && hints.dynamic_range() == DynamicRange::Unknown
}

/// `None` означает отсутствие evidence, а не конфликт двух известных значений.
fn optional_evidence_compatible<T: Copy + Eq>(left: Option<T>, right: Option<T>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        (None, _) | (_, None) => true,
    }
}

/// Известный hint обязан совпасть; отсутствие hint-а не создаёт fake constraint.
fn optional_hint_matches<T: Copy + Eq>(hint: Option<T>, actual: Option<T>) -> bool {
    match hint {
        Some(hint) => actual == Some(hint),
        None => true,
    }
}

/// Unknown dynamic range является отсутствием evidence; два известных значения exact.
fn dynamic_range_compatible(left: DynamicRange, right: DynamicRange) -> bool {
    left == DynamicRange::Unknown || right == DynamicRange::Unknown || left == right
}
