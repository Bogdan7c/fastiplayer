//! BestPlayable-замена yt-dlp compound-а без общего runtime composition route-а.
//!
//! yt-dlp может выбрать (`requested_formats`) пару, которую runtime не открывает
//! одним маршрутом: реальный YouTube `616+251` — HLS video-only (`m3u8_native`) +
//! progressive HTTPS audio-only. Planner такую пару отклоняет
//! (`CandidateCapabilityRejection::ComponentRouteMismatch`), но тогда в ranking-е
//! остаются только atomic строки, и BestPlayable открыл бы немое VideoOnly.
//!
//! Этот модуль строит ровно одну bounded замену (никакого Cartesian перебора):
//! - звук — та же audio-only строка inventory, что в compound-е (язык/дорожка,
//!   выбранные yt-dlp, сохраняются);
//! - видео — planner-ranked video-only строка, которая открывается тем же маршрутом,
//!   что и этот звук; среди них предпочитается та же высота, что у video compound-а
//!   (меняется только transport, а не выбранное yt-dlp разрешение).
//!
//! Замена открывается тем же service-owned composed path-ом, что и ручной выбор
//! «видео + звук» в UI, поэтому дальше (stream model, catalog, audio languages)
//! она неотличима от обычного composed выбора.

use anyhow::Context;
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpComposedSelection,
    YtDlpNormalizedCandidate,
};
use web_media_core::{StreamLayout, TransportFamily, VideoHeight};

use super::OpenedWebCandidate;
use super::content_probe_fallback::{CandidateAttemptContext, CandidateOpenError};

/// Готовая к открытию route-compatible замена compound-а.
pub(super) struct CompoundRouteSubstitute {
    /// Service-owned composed intent (descriptor + обе component selections).
    composed: YtDlpComposedSelection,
    /// Normalized candidate с private request material обеих components.
    candidate: YtDlpNormalizedCandidate,
}

impl CompoundRouteSubstitute {
    /// Video parent selection — active selection composed open-а, как у ручного выбора.
    fn video_parent_selection(&self) -> YtDlpCandidateSelection {
        self.composed.video_parent_selection().clone()
    }
}

/// Одна попытка BestPlayable: planner-ranked строка либо route-compatible замена.
pub(super) enum BestPlayableAttempt<'candidate> {
    /// Candidate ровно в planner-owned порядке.
    Ranked(&'candidate YtDlpNormalizedCandidate),
    /// Замена отклонённого planner-ом compound-а.
    Substitute(&'candidate CompoundRouteSubstitute),
}

/// Результат успешной попытки в форме, общей для planner и composed веток.
pub(super) struct OpenedBestPlayable {
    /// Active exact selection (для замены — video parent selection).
    pub(super) candidate_selection: YtDlpCandidateSelection,
    /// Composed intent, если открылась замена.
    pub(super) composed_selection: Option<Box<YtDlpComposedSelection>>,
    /// Открытый runtime candidate.
    pub(super) opened_candidate: OpenedWebCandidate,
}

/// Ищет route-compatible замену для selected compound-а снимка.
///
/// `None`, если замена не нужна (compound отсутствует или открывается одним
/// маршрутом) либо невозможна (нет playable audio-строки compound-а или
/// совместимого video-only). Ошибки service composition не роняют open:
/// planner-ranked atomic строки остаются fallback-ом, а причина пишется в лог.
pub(super) fn find_compound_route_substitute(
    candidate_snapshot: &YtDlpCandidateSnapshot,
    ranked: &[&YtDlpNormalizedCandidate],
) -> Option<CompoundRouteSubstitute> {
    let compound = candidate_snapshot.selected()?.accepted()?;
    let StreamLayout::Separate { video, audio } = compound.descriptor().layout() else {
        return None;
    };
    let audio_family = audio.transport().family();
    if video
        .transport()
        .family()
        .shares_component_composition_route(audio_family)
    {
        // Compound открывается сам; если planner его отклонил, то по другой причине.
        return None;
    }
    let compound_selection = candidate_snapshot.selection_for(compound).ok()?;
    let audio_selection = candidate_snapshot.inventory_audio_of_compound(&compound_selection)?;
    // Звук обязан быть planner-approved сам по себе (кодек/контейнер/transport).
    if !ranked.iter().any(|candidate| {
        candidate.descriptor().identity() == audio_selection.exact_identity()
            && candidate.descriptor().semantic_identity() == audio_selection.semantic_identity()
    }) {
        return None;
    }
    let video_row = best_route_compatible_video(ranked, audio_family, video.video().height())?;
    match compose_substitute(candidate_snapshot, video_row, &audio_selection) {
        Ok(substitute) => Some(substitute),
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                "YtDlp compound route substitute could not be composed; using planner-ranked candidates"
            );
            None
        }
    }
}

/// Лучшая planner-ranked video-only строка, открываемая тем же маршрутом, что и звук.
///
/// Сначала ищется строка той же высоты, что video compound-а: замена меняет только
/// transport, а не выбранное yt-dlp разрешение (иначе auto-ranking молча понизил бы
/// 1080p до latency-first ступени). Если такой высоты нет — первая совместимая строка
/// planner-owned порядка.
fn best_route_compatible_video<'candidate>(
    ranked: &[&'candidate YtDlpNormalizedCandidate],
    audio_family: TransportFamily,
    compound_video_height: Option<VideoHeight>,
) -> Option<&'candidate YtDlpNormalizedCandidate> {
    let route_compatible_videos = || {
        ranked.iter().copied().filter(move |candidate| {
            matches!(
                candidate.descriptor().layout(),
                StreamLayout::VideoOnly(video)
                    if video.transport().family().shares_component_composition_route(audio_family)
            )
        })
    };
    compound_video_height
        .and_then(|expected_height| {
            route_compatible_videos()
                .find(|candidate| video_only_height(candidate) == Some(expected_height))
        })
        .or_else(|| route_compatible_videos().next())
}

/// Высота video-only строки; `None` для других layout-ов либо неизвестной высоты.
fn video_only_height(candidate: &YtDlpNormalizedCandidate) -> Option<VideoHeight> {
    match candidate.descriptor().layout() {
        StreamLayout::VideoOnly(video) => video.video().height(),
        _ => None,
    }
}

/// Собирает composed selection и fresh candidate через service-owned composition API.
fn compose_substitute(
    candidate_snapshot: &YtDlpCandidateSnapshot,
    video_row: &YtDlpNormalizedCandidate,
    audio_selection: &YtDlpCandidateSelection,
) -> anyhow::Result<CompoundRouteSubstitute> {
    let video_selection = candidate_snapshot
        .selection_for(video_row)
        .context("Route-compatible video row has no exact selection")?;
    let composed = candidate_snapshot
        .compose_inventory_av(&video_selection, audio_selection)
        .context("Route-compatible video and compound audio do not compose")?;
    // Rematch той же generation — exact match; он же отдаёт private request material.
    let (_, composed, candidate) = candidate_snapshot
        .rematch_composed(&composed)
        .context("Composed route substitute does not resolve in its own snapshot")?;
    Ok(CompoundRouteSubstitute {
        composed,
        candidate,
    })
}

/// Строит порядок попыток: замена встаёт перед первой неполной (single-component) строкой.
///
/// Полный A/V ранжируется раньше VideoOnly/AudioOnly (A/V completeness), поэтому замена
/// отклонённого compound-а занимает его место: после planner-ranked Muxed/Separate
/// кандидатов, но до немого video-only.
pub(super) fn ordered_best_playable_attempts<'candidate>(
    ranked: &[&'candidate YtDlpNormalizedCandidate],
    substitute: Option<&'candidate CompoundRouteSubstitute>,
) -> Vec<BestPlayableAttempt<'candidate>> {
    let mut attempts: Vec<BestPlayableAttempt<'candidate>> = ranked
        .iter()
        .copied()
        .map(BestPlayableAttempt::Ranked)
        .collect();
    if let Some(substitute) = substitute {
        let first_incomplete = ranked
            .iter()
            .position(|candidate| is_single_component_layout(candidate.descriptor().layout()))
            .unwrap_or(ranked.len());
        attempts.insert(
            first_incomplete,
            BestPlayableAttempt::Substitute(substitute),
        );
    }
    attempts
}

/// VideoOnly/AudioOnly — неполные layout-ы, проигрывающие полному A/V.
const fn is_single_component_layout(layout: &StreamLayout) -> bool {
    matches!(
        layout,
        StreamLayout::VideoOnly(_) | StreamLayout::AudioOnly(_)
    )
}

/// Открывает одну попытку, сохраняя typed retry classification `CandidateOpenError`.
pub(super) fn open_best_playable_attempt<IsCancelled>(
    attempt_context: &mut CandidateAttemptContext<'_, IsCancelled>,
    candidate_snapshot: &YtDlpCandidateSnapshot,
    attempt: &BestPlayableAttempt<'_>,
) -> Result<OpenedBestPlayable, CandidateOpenError>
where
    IsCancelled: Fn() -> bool,
{
    match attempt {
        BestPlayableAttempt::Ranked(candidate) => {
            let selection = candidate_snapshot
                .selection_for(candidate)
                .context("Planner-ranked YtDlp candidate does not have exact selection")?;
            let (candidate_selection, opened_candidate) =
                attempt_context.open(candidate, selection)?.into_parts();
            Ok(OpenedBestPlayable {
                candidate_selection,
                composed_selection: None,
                opened_candidate,
            })
        }
        BestPlayableAttempt::Substitute(substitute) => {
            tracing::info!(
                "YtDlp compound has no shared transport route; opening route-compatible video with the compound audio"
            );
            let (candidate_selection, opened_candidate) = attempt_context
                .open(&substitute.candidate, substitute.video_parent_selection())?
                .into_parts();
            Ok(OpenedBestPlayable {
                candidate_selection,
                composed_selection: Some(Box::new(substitute.composed.clone())),
                opened_candidate,
            })
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
#[path = "compound_route_substitute/tests.rs"]
mod tests;
