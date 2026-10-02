//! Расчёт соседнего canonical target-а и проверка актуальности manual preview.

use crate::RepeatMode;

use super::{
    ManualNavigationDirection, ManualNavigationIntent, ManualNavigationPreview,
    ManualNavigationPreviewError, PlaylistQueue,
};

impl PlaylistQueue {
    /// Вычисляет соседний canonical index с manual repeat semantics D33.
    pub(super) fn manual_target_index(
        &self,
        current_index: Option<usize>,
        intent: ManualNavigationIntent,
    ) -> Option<usize> {
        let item_count = self.retained_item_count();
        match (current_index, intent.direction()) {
            (None, ManualNavigationDirection::Next) => Some(0),
            (None, ManualNavigationDirection::Previous) => None,
            (Some(index), ManualNavigationDirection::Next) if index + 1 < item_count => {
                Some(index + 1)
            }
            (Some(index), ManualNavigationDirection::Previous) if index > 0 => Some(index - 1),
            (Some(_), _) if intent.repeat_mode() == RepeatMode::RepeatQueue => {
                Some(match intent.direction() {
                    ManualNavigationDirection::Next => 0,
                    ManualNavigationDirection::Previous => item_count - 1,
                })
            }
            (Some(_), _) => None,
        }
    }

    /// Проверяет только structural/traversal base; metadata patch preview не invalidates.
    pub(super) fn validate_manual_preview(
        &self,
        preview: &ManualNavigationPreview,
    ) -> Result<(), ManualNavigationPreviewError> {
        let actual = self.revision_snapshot();
        let expected = preview.expected_revision;
        if expected.structural() == actual.structural()
            && expected.traversal() == actual.traversal()
        {
            Ok(())
        } else {
            Err(ManualNavigationPreviewError::QueueChanged { expected, actual })
        }
    }
}
