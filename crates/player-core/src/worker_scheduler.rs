use std::time::{Duration, Instant};

use crate::tick::{PlayerTickConfig, PlayerWorkerWakeupPlan};

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

    /// Собирает preview wakeup без доступа scheduler-а к `ScrubDriver`.
    #[must_use]
    fn preview_scrub(timeout: Duration) -> Self {
        Self {
            timeout,
            deadline: WorkerWakeupDeadline::PreviewScrub,
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

    /// Live scrub preview throttle window достигнет срока раньше playback.
    PreviewScrub,
}

impl WorkerScheduler {
    /// Вычисляет ближайший wakeup из playback и preview планов.
    ///
    /// Scheduler принимает только boundary functions: session/scrub state остаётся
    /// у владельцев, а здесь живёт только policy выбора ближайшего timeout-а.
    #[must_use]
    pub(crate) fn next_worker_wakeup_deadline<PlaybackPlan, PreviewTimeout>(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
        decoder_readiness_poll_interval: Duration,
        coarse_wakeup_interval: Duration,
        playback_wakeup_plan: PlaybackPlan,
        preview_scrub_timeout: PreviewTimeout,
    ) -> Option<PlannedWorkerWakeup>
    where
        PlaybackPlan:
            FnOnce(Instant, &PlayerTickConfig, Duration, Duration) -> PlayerWorkerWakeupPlan,
        PreviewTimeout: FnOnce(Instant) -> Option<Duration>,
    {
        let playback_wakeup = self.next_playback_wakeup_deadline(
            now,
            tick_config,
            decoder_readiness_poll_interval,
            coarse_wakeup_interval,
            playback_wakeup_plan,
        );
        let Some(preview_timeout) = self.next_preview_scrub_timeout(now, preview_scrub_timeout)
        else {
            return playback_wakeup;
        };

        let preview_wakeup = PlannedWorkerWakeup::preview_scrub(preview_timeout);
        Some(Self::select_earlier_deadline(
            playback_wakeup,
            preview_wakeup,
        ))
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

    /// Возвращает время до throttled preview seek-а во время interactive scrub.
    #[must_use]
    pub(crate) fn next_preview_scrub_timeout<PreviewTimeout>(
        &self,
        now: Instant,
        preview_scrub_timeout: PreviewTimeout,
    ) -> Option<Duration>
    where
        PreviewTimeout: FnOnce(Instant) -> Option<Duration>,
    {
        preview_scrub_timeout(now)
    }

    /// Выбирает preview timeout только когда он строго раньше playback timeout-а.
    fn select_earlier_deadline(
        playback_wakeup: Option<PlannedWorkerWakeup>,
        preview_wakeup: PlannedWorkerWakeup,
    ) -> PlannedWorkerWakeup {
        match playback_wakeup {
            Some(playback_wakeup) if playback_wakeup.timeout() <= preview_wakeup.timeout() => {
                playback_wakeup
            }
            _ => preview_wakeup,
        }
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
    fn scheduler_returns_none_when_playback_and_preview_are_idle() {
        let now = Instant::now();

        let wakeup = scheduler().next_worker_wakeup_deadline(
            now,
            &tick_config(),
            Duration::from_millis(2),
            Duration::from_millis(250),
            |_, _, _, _| playback_plan(None),
            |_| None,
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
                |_| None,
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
    fn scheduler_uses_preview_deadline_when_only_preview_is_due() {
        let now = Instant::now();
        let preview_timeout = Duration::from_millis(10);

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(None),
                |_| Some(preview_timeout),
            )
            .expect("preview wakeup должен быть выбран");

        assert_eq!(wakeup.timeout(), preview_timeout);
        assert_eq!(wakeup.deadline(), WorkerWakeupDeadline::PreviewScrub);
    }

    #[test]
    fn scheduler_prefers_earlier_playback_deadline() {
        let now = Instant::now();
        let playback_timeout = Duration::from_millis(8);
        let preview_timeout = Duration::from_millis(20);

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(Some(playback_timeout)),
                |_| Some(preview_timeout),
            )
            .expect("playback wakeup должен быть выбран");

        assert!(matches!(
            wakeup.deadline(),
            WorkerWakeupDeadline::Playback { .. }
        ));
        assert_eq!(wakeup.timeout(), playback_timeout);
    }

    #[test]
    fn scheduler_prefers_earlier_preview_deadline() {
        let now = Instant::now();
        let playback_timeout = Duration::from_millis(30);
        let preview_timeout = Duration::from_millis(12);

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(Some(playback_timeout)),
                |_| Some(preview_timeout),
            )
            .expect("preview wakeup должен быть выбран");

        assert_eq!(wakeup.timeout(), preview_timeout);
        assert_eq!(wakeup.deadline(), WorkerWakeupDeadline::PreviewScrub);
    }

    #[test]
    fn scheduler_keeps_playback_deadline_when_timeouts_are_equal() {
        let now = Instant::now();
        let equal_timeout = Duration::from_millis(15);

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(Some(equal_timeout)),
                |_| Some(equal_timeout),
            )
            .expect("playback wakeup должен выигрывать при равенстве");

        assert!(matches!(
            wakeup.deadline(),
            WorkerWakeupDeadline::Playback { .. }
        ));
        assert_eq!(wakeup.timeout(), equal_timeout);
    }

    #[test]
    fn scheduler_preserves_zero_preview_timeout_as_immediate_wakeup() {
        let now = Instant::now();

        let wakeup = scheduler()
            .next_worker_wakeup_deadline(
                now,
                &tick_config(),
                Duration::from_millis(2),
                Duration::from_millis(250),
                |_, _, _, _| playback_plan(None),
                |_| Some(Duration::ZERO),
            )
            .expect("zero preview wakeup должен быть сохранён");

        assert_eq!(wakeup.timeout(), Duration::ZERO);
        assert_eq!(wakeup.deadline(), WorkerWakeupDeadline::PreviewScrub);
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
                |_| Some(Duration::from_millis(1)),
            )
            .expect("zero playback wakeup должен быть сохранён");

        assert_eq!(wakeup.timeout(), Duration::ZERO);
        assert!(matches!(
            wakeup.deadline(),
            WorkerWakeupDeadline::Playback { .. }
        ));
    }
}
