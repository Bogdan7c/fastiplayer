use std::time::{Duration, Instant};

use crate::{PlayerTickConfig, PlayerWorkerWakeupPlan};

/// Чистый helper выбора ближайшего самостоятельного wakeup-а worker loop-а.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WorkerScheduler;

/// Запланированный wakeup с timeout-ом для `select!` и причиной последующего side effect-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedWorkerWakeup {
    /// Относительное ожидание, которое worker передаст в `select! default(timeout)`.
    timeout: Duration,

    /// Действие, которое worker выполнит, если timeout истечёт без событий каналов.
    deadline: WorkerWakeupDeadline,
}

impl PlannedWorkerWakeup {
    /// Собирает playback wakeup без доступа scheduler-а к `PlayerSession`.
    #[must_use]
    fn playback(timeout: Duration, plan: PlayerWorkerWakeupPlan, deadline: Instant) -> Self {
        Self {
            timeout,
            deadline: WorkerWakeupDeadline::Playback { plan, deadline },
        }
    }

    /// Возвращает timeout, сохраняя zero-time timeout как явный immediate wakeup.
    #[must_use]
    pub(crate) const fn timeout(self) -> Duration {
        self.timeout
    }

    /// Возвращает причину timeout-а для worker-owned side effects.
    #[must_use]
    pub(crate) const fn deadline(self) -> WorkerWakeupDeadline {
        self.deadline
    }
}

/// Источник ближайшего timeout-а worker loop-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerWakeupDeadline {
    /// Playback planner попросил вызвать `PlayerSession::tick()`.
    Playback {
        /// Read-only план, по которому будет запущен tick.
        plan: PlayerWorkerWakeupPlan,

        /// Монотонный deadline, относительно которого считаем lateness.
        deadline: Instant,
    },
}

impl WorkerScheduler {
    /// Вычисляет ближайший wakeup из playback-плана.
    ///
    /// Scheduler принимает только boundary function: session state остаётся у владельца,
    /// а здесь живёт только policy преобразования media deadline-а в worker timeout.
    #[must_use]
    pub(crate) fn next_worker_wakeup_deadline<PlaybackPlan>(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
        decoder_readiness_poll_interval: Duration,
        coarse_wakeup_interval: Duration,
        playback_wakeup_plan: PlaybackPlan,
    ) -> Option<PlannedWorkerWakeup>
    where
        PlaybackPlan:
            FnOnce(Instant, &PlayerTickConfig, Duration, Duration) -> PlayerWorkerWakeupPlan,
    {
        self.next_playback_wakeup_deadline(
            now,
            tick_config,
            decoder_readiness_poll_interval,
            coarse_wakeup_interval,
            playback_wakeup_plan,
        )
    }

    /// Возвращает media-clock-driven playback deadline.
    #[must_use]
    pub(crate) fn next_playback_wakeup_deadline<PlaybackPlan>(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
        decoder_readiness_poll_interval: Duration,
        coarse_wakeup_interval: Duration,
        playback_wakeup_plan: PlaybackPlan,
    ) -> Option<PlannedWorkerWakeup>
    where
        PlaybackPlan:
            FnOnce(Instant, &PlayerTickConfig, Duration, Duration) -> PlayerWorkerWakeupPlan,
    {
        let plan = playback_wakeup_plan(
            now,
            tick_config,
            decoder_readiness_poll_interval,
            coarse_wakeup_interval,
        );
        let timeout = plan.delay?;
        let deadline = now.checked_add(timeout).unwrap_or(now);

        Some(PlannedWorkerWakeup::playback(timeout, plan, deadline))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkerWakeupReason;

    fn playback_plan(delay: Option<Duration>) -> PlayerWorkerWakeupPlan {
        PlayerWorkerWakeupPlan {
            delay,
            reason: WorkerWakeupReason::CoarseProgress,
            frame_timing: None,
        }
    }

    fn scheduler() -> WorkerScheduler {
        WorkerScheduler
    }

    fn tick_config() -> PlayerTickConfig {
        PlayerTickConfig::default()
    }

    #[test]
    fn scheduler_returns_none_when_playback_is_idle() {
        let now = Instant::now();

        let wakeup = scheduler().next_worker_wakeup_deadline(
            now,
            &tick_config(),
            Duration::from_millis(2),
            Duration::from_millis(250),
            |_, _, _, _| playback_plan(None),
        );

        assert_eq!(wakeup, None);
    }

    #[test]
    fn scheduler_uses_playback_deadline_when_only_playback_is_due() {
        let now = Instant::now();
        let playback_timeout = Duration::from_millis(16);

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(Some(playback_timeout)),
            )
            .expect("playback wakeup должен быть выбран");

        assert_eq!(wakeup.timeout(), playback_timeout);
        assert_eq!(
            wakeup.deadline(),
            WorkerWakeupDeadline::Playback {
                plan: playback_plan(Some(playback_timeout)),
                deadline: now + playback_timeout,
            }
        );
    }

    #[test]
    fn scheduler_preserves_zero_playback_timeout_as_immediate_wakeup() {
        let now = Instant::now();

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(Some(Duration::ZERO)),
            )
            .expect("zero playback wakeup должен быть сохранён");

        assert_eq!(wakeup.timeout(), Duration::ZERO);
        assert!(matches!(
            wakeup.deadline(),
            WorkerWakeupDeadline::Playback { .. }
        ));
    }
}
