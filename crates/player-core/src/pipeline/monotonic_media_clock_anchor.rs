//! Anchor внутреннего monotonic media clock для media без audio clock.

use std::time::{Duration, Instant};

use crate::PlaybackRate;

/// Anchor внутреннего monotonic media clock для media без audio clock.
#[derive(Debug, Clone, Copy)]
pub(super) struct MonotonicMediaClockAnchor {
    /// Media position, которая соответствовала `anchored_at`.
    media_position: Duration,

    /// Монотонный момент, от которого считается fallback media time.
    anchored_at: Instant,

    /// Playback rate, с которым wall-time превращается в media-time.
    playback_rate: PlaybackRate,
}

impl MonotonicMediaClockAnchor {
    /// Создаёт anchor без привязки к video FPS или worker tick cadence.
    #[must_use]
    pub(super) const fn new(
        media_position: Duration,
        anchored_at: Instant,
        playback_rate: PlaybackRate,
    ) -> Self {
        Self {
            media_position,
            anchored_at,
            playback_rate,
        }
    }

    /// Возвращает media position на заданный monotonic момент.
    #[must_use]
    pub(super) fn position_at(self, now: Instant) -> Duration {
        let elapsed_wall_time = now.saturating_duration_since(self.anchored_at);
        self.position_after_elapsed_wall_time(elapsed_wall_time)
    }

    /// Проецирует wall delay от `now`, сохраняя округлительную фазу исходного anchor-а.
    #[must_use]
    pub(super) fn position_after_wall_delay(self, now: Instant, wall_delay: Duration) -> Duration {
        let Some(target_instant) = now.checked_add(wall_delay) else {
            return Duration::MAX;
        };
        self.position_at(target_instant)
    }

    /// Возвращает минимальную wall delay до absolute media deadline.
    #[must_use]
    pub(super) fn wall_delay_until_media_deadline(
        self,
        now: Instant,
        media_deadline: Duration,
    ) -> Duration {
        if media_deadline <= self.position_at(now) {
            return Duration::ZERO;
        }

        let media_delta_from_anchor = media_deadline.saturating_sub(self.media_position);
        let wall_delta_from_anchor = self
            .playback_rate
            .scale_media_delta_to_wall_delay(media_delta_from_anchor);
        let Some(target_instant) = self.anchored_at.checked_add(wall_delta_from_anchor) else {
            return Duration::MAX;
        };
        let wall_delay = target_instant.saturating_duration_since(now);

        // Будущий media deadline не должен превращаться в zero-timeout spin.
        wall_delay.max(Duration::from_nanos(1))
    }

    /// Переводит единый elapsed wall interval от anchor-а в media position.
    #[must_use]
    pub(super) fn position_after_elapsed_wall_time(self, elapsed_wall_time: Duration) -> Duration {
        let elapsed_media_time = self
            .playback_rate
            .scale_wall_delta_to_media_delta(elapsed_wall_time);

        self.media_position
            .checked_add(elapsed_media_time)
            .unwrap_or(Duration::MAX)
    }
}
