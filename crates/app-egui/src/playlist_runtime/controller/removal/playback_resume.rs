//! Что играло в момент Clear и как это вернуть при Undo (решение владельца B7).
//!
//! Владелец данных: `PlaylistController`. Он знает, какой элемент очереди играл (или открывался),
//! в каком устойчивом состоянии (Play/Pause) и не остановлена ли очередь пользователем. Позицию
//! знает только player, поэтому вызывающий код передаёт её явно (`PlaybackProgressAtClear`).
//! Снимок хранится рядом с removal-snapshot в `ControllerDestructiveRemoval`, а Undo запускает
//! возврат через СУЩЕСТВУЮЩИЙ путь открытия строки очереди (`play_item_with`), без обходов.

use std::time::Duration;

use playlist_core::PlaylistItemId;

use crate::playlist_runtime::controller::transport::{
    AppTransportDisposition, ControllerPlayItemOutcome, StablePlaybackIntent,
};
use crate::playlist_runtime::controller::{PlaylistController, StartupPosition};
use crate::playlist_runtime::identity::TransportActionOrigin;

/// Что player сообщил о позиции в момент Clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackProgressAtClear {
    /// Ничего осмысленного не играет (нет media, конец, ошибка): возвращать нечего.
    NotPlaying,
    /// Media загружено и стоит/идёт на этой позиции (Playing, Paused, Buffering, Seeking).
    At(Duration),
}

/// Снимок «что играло» на момент Clear. Хранится только в runtime-slot Undo, не сериализуется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClearPlaybackResume {
    item_id: PlaylistItemId,
    /// Играло или стояло на паузе: Undo повторяет то же устойчивое намерение.
    intent: StablePlaybackIntent,
    /// `KeepStart` для элемента, который ещё только открывался, и для нулевой позиции;
    /// иначе `RestoreExact` (политика B2 к Undo не применяется).
    start_position: StartupPosition,
}

impl PlaylistController {
    /// Фиксирует, что играло, ДО мутации очереди (Clear обнуляет active identity и pending).
    ///
    /// - очередь остановлена пользователем (`Stopped`) или нет элемента очереди -> `None`;
    /// - элемент ещё открывается (pending target) -> откроем его заново с начала, т. к. позиции
    ///   у него нет;
    /// - играет/стоит на паузе активный элемент очереди -> он же с точной позицией.
    ///
    /// Внешнее media (не из очереди) и detached-элемент без `item_id` возвращать нечем.
    pub(super) fn capture_playback_resume_for_clear(
        &self,
        progress: PlaybackProgressAtClear,
    ) -> Option<ClearPlaybackResume> {
        if self.transport_disposition == AppTransportDisposition::Stopped {
            return None;
        }
        let (item_id, start_position) =
            match self.pending_target.and_then(|pending| pending.item_id()) {
                Some(pending_item_id) => (pending_item_id, StartupPosition::KeepStart),
                None => {
                    let active_item_id = self.active_media.and_then(|active| active.item_id())?;
                    match progress {
                        PlaybackProgressAtClear::NotPlaying => return None,
                        PlaybackProgressAtClear::At(position) if position.is_zero() => {
                            (active_item_id, StartupPosition::KeepStart)
                        }
                        PlaybackProgressAtClear::At(position) => {
                            (active_item_id, StartupPosition::RestoreExact(position))
                        }
                    }
                }
            };
        // Строка уже должна быть в очереди; иначе после Undo её не к чему привязывать.
        self.queue.item(item_id)?;
        Some(ClearPlaybackResume {
            item_id,
            intent: self.stable_playback_intent,
            start_position,
        })
    }

    /// Запускает возврат воспроизведения после успешного Undo Clear через обычный Play row.
    ///
    /// Не открывает ничего сам: возвращает тот же `ControllerPlayItemOutcome`, который app
    /// исполняет для клика по строке (strong open с позицией), поэтому cancel/supersede/guard и
    /// ошибки открытия работают штатно.
    pub(crate) fn resume_playback_after_clear_undo(
        &mut self,
        resume: ClearPlaybackResume,
    ) -> ControllerPlayItemOutcome {
        self.play_item_with(
            resume.item_id,
            TransportActionOrigin::Ui,
            resume.intent,
            resume.start_position,
        )
    }
}
