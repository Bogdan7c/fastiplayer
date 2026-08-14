//! Runtime-only anchor для concrete failure до первого player `Installed`.

use crate::PlaylistItemId;

use super::{
    FailedManualNavigationTarget, ManualNavigationOrigin, ManualNavigationPreview,
    ManualNavigationPreviewError, ManualNavigationPreviewState, PlaylistQueue,
    ShuffleManualPreview,
};

impl PlaylistQueue {
    /// Создаёт runtime-only cursor на exact target, который уже завершился ошибкой.
    ///
    /// Target ещё нельзя считать committed current, но явные `Next`/`Previous`
    /// должны продолжаться относительно него, а `Retry` — повторять именно его.
    /// Метод не меняет canonical current, revision или factual shuffle history.
    pub fn begin_failed_manual_navigation(
        &self,
        target_item_id: PlaylistItemId,
    ) -> Result<ManualNavigationPreview, ManualNavigationPreviewError> {
        if self.canonical_index_of(target_item_id).is_none() {
            return Err(ManualNavigationPreviewError::TargetNotCommitted {
                item_id: target_item_id,
            });
        }
        let origin = match self.traversal_current() {
            Some(current) => ManualNavigationOrigin::CommittedItem {
                item_id: current.item_id(),
            },
            None => ManualNavigationOrigin::PersistedIdle,
        };
        let has_left_committed_origin = match origin {
            ManualNavigationOrigin::PersistedIdle => true,
            ManualNavigationOrigin::CommittedItem { item_id } => item_id != target_item_id,
        };
        let shuffle_preview = self.shuffle_traversal.as_ref().map(|traversal| {
            let mut preview = ShuffleManualPreview::new(traversal);
            preview.select_source_order_target(self, target_item_id);
            preview
        });
        Ok(ManualNavigationPreview {
            expected_revision: self.revision_snapshot(),
            origin,
            latest_target_item_id: target_item_id,
            has_left_committed_origin,
            state: ManualNavigationPreviewState::AwaitingUserAfterFailure(
                FailedManualNavigationTarget {
                    item_id: target_item_id,
                },
            ),
            shuffle_preview,
        })
    }
}
