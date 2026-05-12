//! Минимальное состояние UI-анимаций.
//!
//! Сейчас animation boundary хранит только параметры видимого состояния. Позже
//! сюда можно добавить easing/таймеры, не меняя timeline event mapping.

use media_core::TimelineSnapshot;

/// Состояние визуальных анимаций controls/video overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationState {
    /// Нужно ли затемнять stale video frame во время seek/scrub.
    pub dim_stale_frame: bool,
}

impl AnimationState {
    /// Строит animation state из нейтрального timeline snapshot-а.
    #[must_use]
    pub const fn from_timeline(timeline: &TimelineSnapshot) -> Self {
        Self {
            dim_stale_frame: timeline.stale_frame,
        }
    }
}

impl Default for AnimationState {
    /// По умолчанию UI не применяет transient-анимации.
    fn default() -> Self {
        Self {
            dim_stale_frame: false,
        }
    }
}
