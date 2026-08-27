//! Staged manifest-selection marker и его complete-before-commit publication.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use media_core::{DemuxSeekRequest, DemuxSeekResult, TrackInfo};

use super::{HlsComponentDemuxer, HlsComponentFactory};
use crate::diagnostics::{
    HlsManifestComponentRole, HlsManifestSeekDiagnosticPhase, HlsManifestSegmentSeekMarker,
};
use crate::seek::{HlsSeekIndex, SharedHlsSeekIndex};

/// Commit-side evidence survives component composition without mutating shared state early.
pub(crate) struct HlsStagedSelectionCommit {
    seek_index: SharedHlsSeekIndex,
    anchor: Option<crate::seek::HlsSeekAnchor>,
    marker: Option<HlsManifestSegmentSeekMarker>,
}

impl HlsStagedSelectionCommit {
    /// Совершает index publication перед marker-ом в уже авторизованной транзакции.
    pub(crate) fn commit(mut self) {
        if let Some(anchor) = self.anchor.take() {
            self.seek_index.lock().commit_proven_anchor(anchor);
        }
        if let Some(marker) = self.marker.take() {
            marker.emit();
        }
    }
}

impl HlsComponentFactory {
    /// Полностью готовит positioned replacement, не меняя active component/composite.
    pub(crate) fn prepare_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        self.prepare_seek_replacement_for_phase(
            request,
            stable_public_tracks,
            HlsManifestSeekDiagnosticPhase::Preview,
        )
    }

    /// Общий exact-anchor prepare сохраняет явную preview/final diagnostic semantics.
    pub(super) fn prepare_seek_replacement_for_phase(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
        phase: HlsManifestSeekDiagnosticPhase,
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        let anchor = self.seek_index.lock().anchor_for_worker(request)?;
        let preview = HlsSeekIndex::result_for_anchor(request, anchor);
        let replacement_index =
            SharedHlsSeekIndex::new(self.policy.maximum_seek_index_entries.get());
        let mut replacement = HlsComponentDemuxer::open_from_restart_anchor(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            replacement_index,
            self.active_read_control.clone(),
            anchor,
            self.seek_cancellation.clone(),
        )?;
        replacement.public_tracks = stable_public_tracks.to_vec();
        let replacement_tracks = replacement.current.tracks().to_vec();
        replacement.refresh_track_mapping(&replacement_tracks)?;
        replacement.position_replacement_at_anchor(anchor)?;
        replacement.seek_index = self.seek_index.clone();
        replacement.committed_selection_marker = Some(HlsManifestSegmentSeekMarker::new(
            phase,
            HlsManifestComponentRole::from_tracks(stable_public_tracks)?,
            self.policy.seek_landing_policy,
            self.generation,
            request.timestamp,
            anchor,
        ));
        Ok((replacement, preview))
    }
}

impl HlsComponentDemuxer {
    /// Забирает marker только после того, как outer owner авторизовал atomic commit.
    pub(crate) fn take_committed_selection_marker(
        &mut self,
    ) -> Option<HlsManifestSegmentSeekMarker> {
        self.committed_selection_marker.take()
    }

    /// Забирает staged packet proof вместе с replacement, не мутируя active shared index.
    pub(crate) fn take_staged_shared_seek_anchor(&mut self) -> Option<crate::seek::HlsSeekAnchor> {
        self.staged_shared_seek_anchor.take()
    }

    /// Новый manifest candidate становится видим preview только после commit authority.
    pub(super) fn stage_shared_seek_anchor(&mut self, anchor: crate::seek::HlsSeekAnchor) {
        self.staged_shared_seek_anchor = Some(anchor);
    }

    /// Публикует staged index proof и safe marker как одну commit-side operation.
    pub(crate) fn commit_staged_selection(&mut self) {
        if let Some(anchor) = self.take_staged_shared_seek_anchor() {
            self.seek_index.lock().commit_proven_anchor(anchor);
        }
        if let Some(marker) = self.take_committed_selection_marker() {
            marker.emit();
        }
    }

    /// Выносит commit evidence из component перед final composite assembly.
    pub(crate) fn take_staged_selection_commit(&mut self) -> HlsStagedSelectionCommit {
        HlsStagedSelectionCommit {
            seek_index: self.seek_index.clone(),
            anchor: self.take_staged_shared_seek_anchor(),
            marker: self.take_committed_selection_marker(),
        }
    }

    /// Factory stage не публикует marker до решения outer transaction owner-а.
    pub(super) fn stage_committed_selection_marker(
        &mut self,
        marker: HlsManifestSegmentSeekMarker,
    ) {
        self.committed_selection_marker = Some(marker);
    }

    /// Восстанавливает public composite target после internal audio alignment seek-а.
    pub(crate) fn retarget_committed_selection_marker(&mut self, requested_target: Duration) {
        self.committed_selection_marker = self
            .committed_selection_marker
            .take()
            .map(|marker| marker.with_requested_target(requested_target));
    }

    /// Делает подготовленный source authoritative и публикует selection ровно перед swap.
    pub(super) fn commit_prepared_replacement(
        &mut self,
        mut replacement: HlsComponentDemuxer,
        result: DemuxSeekResult,
    ) -> Result<DemuxSeekResult> {
        replacement.activate_committed_read()?;
        replacement.commit_staged_selection();
        *self = replacement;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::MediaTime;

    use super::HlsStagedSelectionCommit;
    use crate::plan::{HlsManifestSeekPoint, HlsSegmentRestartCoordinate};
    use crate::seek::{HlsSeekAnchor, HlsSeekAnchorKind, SharedHlsSeekIndex};

    fn anchor() -> HlsSeekAnchor {
        let restart_segment = HlsSegmentRestartCoordinate { segment_index: 3 };
        HlsSeekAnchor {
            epoch_index: 1,
            restart_segment,
            manifest_segment: HlsManifestSeekPoint {
                media_sequence: 53,
                discontinuity_sequence: 2,
                manifest_segment_index: 3,
                epoch_index: 1,
                restart_segment,
                timeline_start: Duration::from_secs(30),
                timeline_end: Duration::from_secs(40),
            },
            timeline_origin: Duration::from_secs(30),
            epoch_timestamp_origin: Duration::ZERO,
            position: MediaTime::from_millis(30_033),
            decode_position: MediaTime::from_secs(30),
            kind: HlsSeekAnchorKind::VideoRandomAccessPoint,
        }
    }

    #[test]
    fn dropped_staged_anchor_does_not_mutate_shared_preview_index() {
        let seek_index = SharedHlsSeekIndex::new(8);
        let staged = HlsStagedSelectionCommit {
            seek_index: seek_index.clone(),
            anchor: Some(anchor()),
            marker: None,
        };
        drop(staged);
        assert_eq!(seek_index.lock().initial_anchor(true), None);
    }

    #[test]
    fn authorized_staged_anchor_becomes_visible_exactly_at_commit() {
        let seek_index = SharedHlsSeekIndex::new(8);
        HlsStagedSelectionCommit {
            seek_index: seek_index.clone(),
            anchor: Some(anchor()),
            marker: None,
        }
        .commit();
        assert_eq!(seek_index.lock().initial_anchor(true), Some(anchor()));
    }
}
