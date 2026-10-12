//! Play из пустого плеера: запускает очередь с `current` или с первого элемента (A15-live).
//!
//! Раньше Play без установленного media уходил прямо в player и переводил его в «Playing»
//! без источника (таймер тикал, звука и картинки нет). Теперь решение принимает владелец
//! очереди: есть что открывать — идёт обычный Play строки, нет — Play ничего не делает.

use super::PlaylistRuntime;
use super::row_interactions::RuntimeRowPlayOutcome;

impl PlaylistRuntime {
    /// Play, пока ничего не установлено: открыть `current` либо первый элемент очереди.
    ///
    /// `None` — этот путь не применим (нет controller-а, очередь пуста, media уже
    /// установлено или открывается): вызывающий код ведёт Play обычным маршрутом.
    /// Иначе это ровно Play строки: те же исходы, тот же strong install; состояние
    /// «Playing» player получит только после реальной установки.
    pub(crate) fn play_idle_queue_target(&mut self) -> Option<RuntimeRowPlayOutcome> {
        let target = self.controller.as_ref()?.idle_play_target()?;
        tracing::debug!(item_id = ?target, "Play from an empty player opens the queue element");
        Some(self.play_playlist_row(target))
    }
}

#[cfg(test)]
mod tests;
