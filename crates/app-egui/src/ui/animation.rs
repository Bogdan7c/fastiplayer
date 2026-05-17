//! Минимальное состояние UI-анимаций.
//!
//! Сейчас animation boundary хранит только параметры видимого состояния. Позже
//! сюда можно добавить easing/таймеры, не меняя timeline event mapping.

use media_core::TimelineSnapshot;

/// Состояние визуальных анимаций controls/video overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationState {
    /// Нужно ли затемнять stale video frame вне active scrub.
    pub dim_stale_frame: bool,
}

impl AnimationState {
    /// Строит animation state из нейтрального timeline snapshot-а.
    ///
    /// Active scrub обновляет target на timeline, но не меняет яркость cached
    /// video frame-а, чтобы preview latency не выглядела как мигание видео.
    #[must_use]
    pub const fn from_timeline(timeline: &TimelineSnapshot) -> Self {
        Self {
            dim_stale_frame: timeline.stale_frame && !timeline.scrubbing,
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

#[cfg(test)]
mod tests {
    use media_core::{MediaDuration, TimelineSnapshot};

    use super::AnimationState;

    /// Проверяет, что обычный seek всё ещё получает stale-затемнение.
    #[test]
    fn stale_seek_dims_video_frame() {
        let mut timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(30));
        timeline.stale_frame = true;

        let animation_state = AnimationState::from_timeline(&timeline);

        assert!(animation_state.dim_stale_frame);
    }

    /// Проверяет, что interactive scrub не меняет яркость cached video frame-а.
    #[test]
    fn active_scrub_keeps_video_brightness_stable() {
        let mut timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(30));
        timeline.scrubbing = true;
        timeline.stale_frame = true;

        let animation_state = AnimationState::from_timeline(&timeline);

        assert!(!animation_state.dim_stale_frame);
    }
}
