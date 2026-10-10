//! Что очередь отдаёт приложению для кнопки «Повторить» (B9).
//!
//! Две вещи принадлежат очереди, а не UI:
//! - **факт ручного отказа открытия**: пользователь сам выбрал элемент (клик, Play,
//!   Next/Previous), и тот не открылся. Только controller знает, что отказ был ручным, а не
//!   шагом автоматического пропуска (тот сообщает итог цепочки и ошибку в центре не
//!   показывает). Факт — одноразовое событие, как `drain_automatic_queue_notices`;
//! - **переоткрытие упавшего воспроизведения**: элемент, который играл и оборвался
//!   (потеря сети посреди просмотра), открывается заново с позиции обрыва тем же путём
//!   Play строки, что и Undo Clear (B7).

use std::time::Duration;

use playlist_core::PlaylistItemId;

use super::controller::{ActiveMediaReuse, ControllerPlayItemOutcome, StablePlaybackIntent};
use super::row_interactions::RuntimeRowPlayOutcome;
use super::{PlaylistRuntime, StartupPosition, TransportActionOrigin};

/// Одноразовый почтовый ящик «ручное открытие этого элемента не удалось».
#[derive(Debug, Default)]
pub(super) struct ManualOpenFailureMailbox {
    failed_item: Option<PlaylistItemId>,
}

impl ManualOpenFailureMailbox {
    /// Запоминает отказ; более новый отказ заменяет старый (показывается только последний).
    pub(super) fn note(&mut self, item_id: PlaylistItemId) {
        self.failed_item = Some(item_id);
    }

    fn take(&mut self) -> Option<PlaylistItemId> {
        self.failed_item.take()
    }
}

impl PlaylistRuntime {
    /// Забирает элемент, который пользователь выбрал вручную и который не открылся.
    ///
    /// `None` — ручных отказов с прошлого вызова не было (автоматический пропуск сюда не
    /// попадает). Вызывающий код сразу после отчёта об отказе открытия забирает событие и
    /// показывает ошибку в центре; нетронутое событие не должно переживать кадр.
    pub(crate) fn take_manual_open_failure(&mut self) -> Option<PlaylistItemId> {
        self.manual_open_failure.take()
    }

    /// Открыть заново элемент, воспроизведение которого оборвалось, начиная с `position`.
    ///
    /// Возвращает тот же исход Play строки, что исполняет клик по строке
    /// (`apply_playlist_row_play`), поэтому guard, «занято» и повторный отказ работают штатно.
    /// `None` — media очереди нет (очередь ещё не загружена или элемент уже не играл).
    pub(crate) fn reopen_failed_active_item_from(
        &mut self,
        position: Duration,
    ) -> Option<RuntimeRowPlayOutcome> {
        // Нулевая позиция — обычное открытие с начала; ненулевая применяется как есть:
        // правило «у конца — с начала» (B2) относится к автоматическому resume, а не к
        // осознанному «продолжить с места обрыва».
        let start_position = if position.is_zero() {
            StartupPosition::KeepStart
        } else {
            StartupPosition::RestoreExact(position)
        };
        self.reopen_failed_active_item(start_position)
    }

    fn reopen_failed_active_item(
        &mut self,
        start_position: StartupPosition,
    ) -> Option<RuntimeRowPlayOutcome> {
        let controller = self.controller.as_mut()?;
        let item_id = controller.active_media()?.item_id()?;
        self.supersede_competing_openers_for_row_play();
        let controller = self.controller.as_mut()?;
        let outcome: ControllerPlayItemOutcome = controller.play_item_reusing_active(
            item_id,
            TransportActionOrigin::Ui,
            StablePlaybackIntent::Playing,
            start_position,
            ActiveMediaReuse::ForceReopen,
        );
        Some(RuntimeRowPlayOutcome::Controller(outcome))
    }
}

#[cfg(test)]
mod tests;
