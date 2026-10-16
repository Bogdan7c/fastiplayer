//! Описание аудиодорожек для памяти выбора и план их восстановления.
//!
//! Id дорожки у плеера — счётчик внутри media и между запусками не сохраняется. Поэтому
//! запоминается ОПИСАНИЕ: сырой язык, название, кодек и номер среди аудиодорожек; при
//! открытии оно сверяется с реальным списком дорожек (`find_matching_track`).

use media_core::{TrackId, TrackKind, TrackLabels};
use player_core::TrackSummarySnapshot;
use playlist_state::{
    RememberedTrackHint, TrackHintCandidate, TrackHintError, TrackHintMatch, find_matching_track,
};

/// Что делать со звуком установленного media по запомненному описанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RememberedAudioPlan {
    /// Для этого видео звук не запоминался.
    NothingRemembered,
    /// Запомненной дорожки в media нет: остаётся выбор по умолчанию.
    TrackNotFound,
    /// Запомненная дорожка уже выбрана по умолчанию: команда не нужна.
    AlreadySelected,
    /// Выбрать эту дорожку до начала воспроизведения.
    Select(TrackId),
}

/// Описание дорожки по меткам контейнера (общее для аудио и встроенных субтитров).
pub(crate) fn hint_candidate_from_labels(
    labels: &TrackLabels,
    codec_id: &str,
    ordinal_among_kind: u32,
) -> TrackHintCandidate {
    TrackHintCandidate {
        language: labels
            .language
            .as_ref()
            .map(|language| language.as_str().to_owned()),
        title: labels.title.clone(),
        codec_id: codec_id.to_owned(),
        ordinal_among_kind,
    }
}

/// Аудиодорожки снимка плеера в порядке снимка вместе с описанием для сверки.
fn audio_candidates(tracks: &[TrackSummarySnapshot]) -> Vec<(TrackId, TrackHintCandidate)> {
    tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .enumerate()
        .map(|(ordinal, track)| {
            let ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
            (
                track.id,
                hint_candidate_from_labels(&track.labels, &track.codec_id, ordinal),
            )
        })
        .collect()
}

/// Описание аудиодорожки `track_id` для записи в память; `Ok(None)` — такой аудиодорожки нет.
pub(crate) fn audio_hint_for_track(
    tracks: &[TrackSummarySnapshot],
    track_id: TrackId,
) -> Result<Option<RememberedTrackHint>, TrackHintError> {
    let Some((_, candidate)) = audio_candidates(tracks)
        .into_iter()
        .find(|(id, _)| *id == track_id)
    else {
        return Ok(None);
    };
    RememberedTrackHint::new(
        candidate.language,
        candidate.title,
        candidate.codec_id,
        candidate.ordinal_among_kind,
    )
    .map(Some)
}

/// План восстановления звука: сверяет запомненное описание с реальными дорожками.
///
/// `currently_selected` — дорожка, которую плеер выбрал сам; если запомненная совпала с ней,
/// команда не нужна (нет лишнего сброса звукового тракта).
pub(crate) fn plan_audio_restore(
    remembered: Option<&RememberedTrackHint>,
    tracks: &[TrackSummarySnapshot],
    currently_selected: Option<TrackId>,
) -> RememberedAudioPlan {
    let Some(hint) = remembered else {
        return RememberedAudioPlan::NothingRemembered;
    };
    let candidates = audio_candidates(tracks);
    let descriptions: Vec<TrackHintCandidate> = candidates
        .iter()
        .map(|(_, candidate)| candidate.clone())
        .collect();
    match find_matching_track(hint, &descriptions) {
        TrackHintMatch::Exact(position) | TrackHintMatch::Approximate(position) => {
            match candidates.get(position) {
                Some((track_id, _)) if Some(*track_id) == currently_selected => {
                    RememberedAudioPlan::AlreadySelected
                }
                Some((track_id, _)) => RememberedAudioPlan::Select(*track_id),
                None => RememberedAudioPlan::TrackNotFound,
            }
        }
        TrackHintMatch::NotFound => RememberedAudioPlan::TrackNotFound,
    }
}
