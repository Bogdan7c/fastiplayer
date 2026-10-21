//! Дедлайн «следующего видимого изменения позиции» и его память между кадрами.
//!
//! Зачем дедлайн: когда кадры не приходят (звук без видео, 24 fps фильм с длинной дорожкой),
//! время на экране иначе замирало бы до следующего кадра или страховочного таймера
//! (cpu-parity, сессия 04). Сам расчёт границы — чистая функция
//! [`next_visible_position_change`]; здесь живёт только то, что требует памяти:
//! - защита от холостого спина, когда snapshot ещё не догнал границу секунды/пикселя
//!   (позиция в snapshot-е обновляется только тиком worker-а);
//! - правило сессии 06b: если позиция за экраном ([`VisibleTimelinePosition::OffScreen`]),
//!   будить UI ради неё нечего, и дедлайна нет.

use std::time::{Duration, Instant};

use player_core::PlayerSnapshot;

use super::ui_frame_model::{
    QuantizedTimelinePosition, VisibleTimelinePosition, next_visible_position_change,
};

/// Нижняя граница ожидания, если дедлайн позиции прошёл, а snapshot не изменился.
const POSITION_STALE_BACKOFF: Duration = Duration::from_millis(8);

/// Память последнего дедлайна: позиция, для которой он считался, и сам дедлайн.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositionWakeMemory {
    position: QuantizedTimelinePosition,
    deadline: Instant,
}

/// Владелец дедлайна позиции; одно поле `RedrawRuntime`.
#[derive(Debug, Default)]
pub(super) struct PositionWakeTracker {
    /// Последний выданный дедлайн; `None`, пока позиция не видна или ещё не считалась.
    last: Option<PositionWakeMemory>,
}

/// Факты кадра, от которых зависит дедлайн позиции.
pub(super) struct PositionWakeFacts<'snapshot> {
    pub(super) snapshot: &'snapshot PlayerSnapshot,
    /// Позиция из UI-модели этого кадра (уже с учётом видимости панели).
    pub(super) position: VisibleTimelinePosition,
    /// Ширина дорожки ползунка с последнего прохода; `None` до первого прохода.
    pub(super) track_width_px: Option<u32>,
    pub(super) now: Instant,
}

impl PositionWakeTracker {
    /// Когда позиция в следующий раз изменит картинку; `None` — будить ради неё не нужно.
    ///
    /// Позиция за экраном → `None`, и память сбрасывается: после показа панели первый
    /// дедлайн будет свежей границей, а не backoff-ом по записи, сделанной до скрытия.
    pub(super) fn next_deadline(&mut self, facts: PositionWakeFacts<'_>) -> Option<Instant> {
        let VisibleTimelinePosition::Shown(position) = facts.position else {
            self.last = None;
            return None;
        };
        let computed =
            next_visible_position_change(facts.snapshot, facts.track_width_px, facts.now)?;
        let deadline = position_wake_with_backoff(
            self.last,
            position,
            computed,
            facts.snapshot.video_frame_duration_estimate,
            facts.now,
        );
        self.last = Some(PositionWakeMemory { position, deadline });
        Some(deadline)
    }
}

/// Дедлайн позиции с учётом «snapshot ещё не догнал границу».
///
/// Если предыдущий дедлайн уже прошёл, а квантованная позиция та же, значит worker ещё не
/// опубликовал новую позицию: ждём его тик (длительность кадра, не меньше
/// [`POSITION_STALE_BACKOFF`]), а не считанные миллисекунды до «той же» границы снова.
fn position_wake_with_backoff(
    previous: Option<PositionWakeMemory>,
    current: QuantizedTimelinePosition,
    computed: Instant,
    frame_duration: Duration,
    now: Instant,
) -> Instant {
    match previous {
        Some(previous) if previous.position == current && now >= previous.deadline => {
            now + frame_duration.max(POSITION_STALE_BACKOFF)
        }
        _ => computed,
    }
}

#[cfg(test)]
mod tests;
