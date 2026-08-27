//! Exact-lineage переходы ephemeral состояния URL sidebar.

use super::*;

/// Ephemeral pending/error state; active state читается только из Installed source.
#[derive(Debug, Default)]
pub(crate) struct UrlSidebarController {
    pub(super) pending_selection: Option<UrlSidebarPendingSelection>,
    pub(super) safe_error: Option<SafeErrorState>,
    pub(super) item_override: Option<ItemOverrideState>,
}

#[derive(Debug)]
pub(super) struct SafeErrorState {
    pub(super) generation: WebMediaStreamGeneration,
    pub(super) error: UrlSidebarSafeError,
}

#[derive(Debug)]
pub(super) struct ItemOverrideState {
    pub(super) source_lineage: u64,
    pub(super) item_id: Option<playlist_core::PlaylistItemId>,
    pub(super) preferred_height: Option<u32>,
}

impl UrlSidebarController {
    /// Новый Installed source завершает/инвалидирует весь ephemeral state прошлого поколения.
    pub(crate) fn record_installed_source(&mut self) {
        self.pending_selection = None;
        self.safe_error = None;
    }

    /// Публикует один typed pending selector для общего candidate/component reopen.
    pub(crate) fn record_switch_started(
        &mut self,
        pending_selection: UrlSidebarPendingSelection,
    ) -> Result<(), UrlSidebarTransitionError> {
        if self.pending_selection.is_some() {
            return Err(UrlSidebarTransitionError::Busy);
        }
        self.safe_error = None;
        self.pending_selection = Some(pending_selection);
        Ok(())
    }

    /// Pre-barrier failure снимает только matching pending selector.
    pub(crate) fn record_switch_failed(
        &mut self,
        expected_pending: &UrlSidebarPendingSelection,
        visible_generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) -> bool {
        let matching_pending = self
            .pending_selection
            .as_ref()
            .is_some_and(|pending| pending == expected_pending);
        if !matching_pending {
            return false;
        }
        self.pending_selection = None;
        self.safe_error = Some(SafeErrorState {
            generation: visible_generation,
            error,
        });
        true
    }

    /// Terminal failure допускает уже опубликованный Installed source, который
    /// штатно очистил projection, но никогда не стирает другой pending switch.
    pub(crate) fn record_switch_terminal_failed(
        &mut self,
        expected_pending: &UrlSidebarPendingSelection,
        visible_generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) -> bool {
        if self
            .pending_selection
            .as_ref()
            .is_some_and(|pending| pending != expected_pending)
        {
            return false;
        }
        self.pending_selection = None;
        self.safe_error = Some(SafeErrorState {
            generation: visible_generation,
            error,
        });
        true
    }

    /// Pre-start rejection не может стереть уже выполняющийся typed switch.
    pub(crate) fn record_switch_start_rejected(
        &mut self,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) -> bool {
        if self.pending_selection.is_some() {
            return false;
        }
        self.safe_error = Some(SafeErrorState { generation, error });
        true
    }

    /// Exact Installed публикует runtime-only item/source preference новой generation.
    #[cfg(test)]
    pub(crate) fn record_candidate_switch_installed(
        &mut self,
        installed_generation: WebMediaStreamGeneration,
        item_id: Option<playlist_core::PlaylistItemId>,
        preferred_height: Option<u32>,
    ) {
        self.pending_selection = None;
        self.safe_error = None;
        self.item_override = Some(ItemOverrideState {
            source_lineage: installed_generation.source,
            item_id,
            preferred_height,
        });
    }

    /// Component Installed снимает selector, не меняя candidate/item preference.
    pub(crate) fn record_component_switch_installed(&mut self) {
        self.pending_selection = None;
        self.safe_error = None;
    }
}
