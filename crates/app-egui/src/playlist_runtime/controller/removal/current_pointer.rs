//! Undo удаления после смены текущего элемента (A9b).
//!
//! Владелец решил: если shuffle выключен, а среди удалённых строк не было играющей,
//! смена «что играет» (автопереход или ручной выбор другой строки) не должна убивать Undo.
//! Undo при этом возвращает только удалённые строки на прежние позиции, а pointer
//! «что играет» (traversal current и active lineage) остаётся из текущей очереди.
//!
//! Модуль владеет двумя решениями: «переживает ли Undo смену текущего» и «как коммитить
//! dirty после смены текущего, чтобы Undo сверялся только с тем, что реально его ломает».

use playlist_core::{PlaylistRemovalSnapshot, RemovalCurrentOutcome, RemovalSnapshotRestoreError};

use super::{ControllerDestructiveRemoval, ControllerRemovalKind};
use crate::playlist_runtime::controller::{PlaylistController, PlaylistDirtySignal};
use crate::playlist_runtime::view::PlaylistDirtyRevision;

impl ControllerDestructiveRemoval {
    /// Удаление не затронуло играющую строку, поэтому snapshot можно использовать только как
    /// источник удалённых строк. Clear исключён: он обнуляет текущее и сбрасывает player.
    pub(crate) fn removed_only_non_playing_rows(&self) -> bool {
        self.kind != ControllerRemovalKind::Clear
            && self.active_tombstone_lineage.is_none()
            && matches!(self.current_outcome, RemovalCurrentOutcome::Preserved(_))
    }
}

impl PlaylistController {
    /// Переживёт ли Undo этого удаления смену текущего элемента прямо сейчас.
    ///
    /// С shuffle порядок воспроизведения зависит от истории, которую snapshot откатил бы
    /// целиком, поэтому там прежнее правило: смена lineage сбрасывает Undo.
    pub(crate) fn undo_survives_current_change(
        &self,
        removal: &ControllerDestructiveRemoval,
    ) -> bool {
        !self.queue.shuffle_enabled() && removal.removed_only_non_playing_rows()
    }

    /// Восстанавливает строки: pointer «что играет» остаётся из текущей очереди, когда это
    /// разрешено (`undo_survives_current_change`); иначе откат целиком из snapshot-а.
    pub(super) fn restore_queue_for_undo(
        &mut self,
        removal: &ControllerDestructiveRemoval,
        snapshot: PlaylistRemovalSnapshot,
    ) -> Result<(), RemovalSnapshotRestoreError> {
        if self.undo_survives_current_change(removal) {
            self.queue.restore_removed_rows_keeping_current(snapshot)?;
        } else {
            self.queue.restore_removal_snapshot(snapshot)?;
        }
        Ok(())
    }

    /// Dirty после Installed. Если очередь изменилась только указателем current (без
    /// structural-изменения) и shuffle выключен, Undo удаления не блокируется: состав и
    /// порядок строк прежние, восстановление однозначно. Persistence получает сигнал как обычно.
    pub(crate) fn commit_install_dirty(
        &mut self,
        revision: PlaylistDirtyRevision,
        structural_changed: bool,
    ) -> PlaylistDirtySignal {
        if structural_changed || self.queue.shuffle_enabled() {
            return self.commit_dirty(revision);
        }
        self.dirty_revision = revision;
        let signal = PlaylistDirtySignal::new(revision);
        self.latest_dirty_signal = Some(signal);
        signal
    }
}
