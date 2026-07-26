//! Codec/container-neutral завершение request-owned receipts seek transaction-а.

use media_core::MediaTime;

use crate::PlayerError;

use super::PlayerSession;

impl PlayerSession {
    /// Публикует success только после единственного authoritative seek commit-а.
    pub(super) fn complete_pending_seek_receipts(
        &mut self,
        position: MediaTime,
        seek_generation: u64,
    ) {
        self.complete_unclaimed_staged_position(seek_generation);
        self.finish_exact_timeline_seek(position);
        self.finish_installed_position_restore(seek_generation);
    }

    /// Передаёт одну typed terminal failure всем receipts текущей seek transaction.
    pub(super) fn fail_pending_seek_receipts(&mut self, error: PlayerError) {
        self.fail_unclaimed_staged_position(error.clone());
        self.fail_pending_exact_timeline_seek(error.clone());
        self.fail_pending_installed_position_restore(error);
    }

    /// После owner turn-а отсеивает receipts, потерявшие exact media identity.
    pub(crate) fn reconcile_pending_seek_receipt_identities(&mut self) {
        self.reconcile_exact_timeline_seek_identity();
        self.reconcile_installed_position_restore_identity();
    }
}
