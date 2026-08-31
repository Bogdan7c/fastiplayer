//! Узкая app projection extractor snapshot-а в provider-neutral semantic plane.
//!
//! Модуль не открывает transport и не меняет `ActiveMediaSource`. Он только
//! связывает существующий service snapshot с уже существующими catalog,
//! exact/semantic selection и presentation contracts перед N04 envelope.

use anyhow::{Context, Result, anyhow, bail};
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpLiveIntent, YtDlpPlaylistMetadata,
};
use web_media_core::{ExactSelectionIdentity, WebMediaPresentationKind, WebMediaSelection};
use web_media_playback_plan::PlanningCandidateSnapshot;

/// Neutral catalog/presentation projection до выбора runtime candidate-а.
pub(crate) struct ExtractorCatalogProjection {
    /// Existing neutral planning catalog; второй inventory не создаётся.
    catalog: PlanningCandidateSnapshot,
    /// Exact VOD/live lifecycle из extractor-owned public fields.
    presentation: WebMediaPresentationKind,
    /// Metadata того же immutable extraction generation.
    playlist_metadata: YtDlpPlaylistMetadata,
    /// Safe diagnostics count service-owned row-local planning rejections.
    planning_rejection_count: usize,
}

impl ExtractorCatalogProjection {
    /// Проецирует canonical service snapshot без повторной normalization.
    pub(crate) fn from_snapshot(snapshot: &YtDlpCandidateSnapshot) -> Result<Self> {
        let planning_projection = snapshot
            .planning_projection()
            .context("Не удалось выразить extractor candidates через neutral planner")?;
        let planning_rejection_count = planning_projection.rejections().len();
        let catalog = planning_projection.into_snapshot();
        snapshot
            .validate_planning_snapshot_alignment(&catalog)
            .context("Extractor service/catalog projections не соответствуют друг другу")?;

        Ok(Self {
            catalog,
            presentation: presentation_from_live_intent(snapshot.live_intent())?,
            playlist_metadata: snapshot.playlist_metadata().clone(),
            planning_rejection_count,
        })
    }

    /// Возвращает existing neutral catalog для capability planning.
    pub(crate) const fn catalog(&self) -> &PlanningCandidateSnapshot {
        &self.catalog
    }

    /// Возвращает число service-owned row-local planning rejections.
    pub(crate) const fn planning_rejection_count(&self) -> usize {
        self.planning_rejection_count
    }

    /// Добавляет exact active selection только после успешного candidate open.
    pub(crate) fn with_active_selection(
        self,
        active_selection: &YtDlpCandidateSelection,
    ) -> Result<ExtractorAdapterProjection> {
        let exact_identity = active_selection.exact_identity();
        let semantic_identity = active_selection.semantic_identity();
        let belongs_to_catalog = self.catalog.candidates().iter().any(|candidate| {
            candidate.descriptor().identity() == exact_identity
                && candidate.descriptor().semantic_identity() == semantic_identity
        });
        if !belongs_to_catalog {
            bail!("Active extractor selection отсутствует в neutral catalog");
        }
        let parent = ExactSelectionIdentity::new(exact_identity.clone(), semantic_identity.clone())
            .context("Active extractor selection нарушает source lineage")?;

        Ok(ExtractorAdapterProjection {
            catalog: self.catalog,
            selection: WebMediaSelection::candidate(parent),
            presentation: self.presentation,
            playlist_metadata: self.playlist_metadata,
        })
    }
}

/// Полная узкая projection после выбора active candidate-а.
pub(crate) struct ExtractorAdapterProjection {
    /// Existing neutral planning catalog.
    catalog: PlanningCandidateSnapshot,
    /// N01 provider-neutral exact active selection.
    selection: WebMediaSelection,
    /// N01 exact VOD/live presentation kind.
    presentation: WebMediaPresentationKind,
    /// Existing metadata без повторного process invocation.
    playlist_metadata: YtDlpPlaylistMetadata,
}

impl ExtractorAdapterProjection {
    /// Возвращает neutral catalog без второго representation.
    pub(crate) const fn catalog(&self) -> &PlanningCandidateSnapshot {
        &self.catalog
    }

    /// Возвращает neutral exact active selection.
    pub(crate) const fn selection(&self) -> &WebMediaSelection {
        &self.selection
    }

    /// Заменяет parent-only selection на canonical installed component selection.
    pub(crate) fn with_neutral_selection(mut self, selection: WebMediaSelection) -> Result<Self> {
        if selection.parent() != self.selection.parent() {
            bail!("Final neutral selection относится к другому extractor parent");
        }
        self.selection = selection;
        Ok(self)
    }

    /// Возвращает exact presentation lifecycle kind.
    pub(crate) const fn presentation(&self) -> WebMediaPresentationKind {
        self.presentation
    }

    /// Передаёт metadata того же extraction generation в prepared result.
    pub(crate) fn into_playlist_metadata(self) -> YtDlpPlaylistMetadata {
        self.playlist_metadata
    }
}

/// Fail-closed mapping official extractor live fields в N01 presentation kind.
fn presentation_from_live_intent(live_intent: YtDlpLiveIntent) -> Result<WebMediaPresentationKind> {
    match live_intent {
        YtDlpLiveIntent::Unspecified | YtDlpLiveIntent::NotLive => {
            Ok(WebMediaPresentationKind::Vod)
        }
        YtDlpLiveIntent::Live => Ok(WebMediaPresentationKind::Live),
        YtDlpLiveIntent::Upcoming => Err(anyhow!("Upcoming extractor media ещё не playable")),
        YtDlpLiveIntent::PostLive => Err(anyhow!("Post-live extractor media ещё не стало VOD")),
        YtDlpLiveIntent::Incompatible => Err(anyhow!(
            "Extractor live fields не образуют совместимый presentation intent"
        )),
    }
}

#[cfg(all(test, unix))]
mod tests;
