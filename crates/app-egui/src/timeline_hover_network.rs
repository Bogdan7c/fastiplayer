use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::timeline_hover_prepare::TimelineHoverPrepareTarget;
use crate::timeline_hover_source::{
    TimelineHoverOpenedSource, TimelineHoverSourceFactory, TimelineHoverSourceIdentity,
    TimelineHoverSourceOpenOutcome,
};

/// App-owned guard для network hover opens: latest-only, max-one-inflight, no queue.
pub(crate) struct TimelineHoverNetworkOpenController {
    /// Generation шагает при source/config invalidation и делает старые results stale.
    source_generation: u64,

    /// Throttle между стартами network opens; `Duration::ZERO` убирает только delay.
    inter_start_throttle: Duration,

    /// Последний реальный старт background open-а.
    last_started_at: Option<Instant>,

    /// Единственный tracked in-flight network open. Для того же source он держит one-in-flight.
    active_job: Option<TimelineHoverNetworkOpenJob>,

    /// Terminal target не auto-retry-ится, пока target/source generation не изменится.
    held_terminal_target: Option<HeldNetworkHoverTarget>,

    /// Bounded diagnostics controller-owned событий без target/source history.
    diagnostics: TimelineHoverNetworkOpenDiagnosticsCounters,
}

/// Read-only diagnostics network open controller-а без source URL/target history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TimelineHoverNetworkOpenDiagnosticsSnapshot {
    /// Текущий throttle между starts network opens.
    pub(crate) inter_start_throttle: Duration,

    /// Generation source/config context-а для stale-result diagnostics.
    pub(crate) source_generation: u64,

    /// Количество in-flight jobs; controller policy допускает только 0 или 1.
    pub(crate) in_flight_count: u8,

    /// Есть ли terminal target, который не будет auto-retry без смены target/source.
    pub(crate) failed_target_held: bool,

    /// Сколько starts прошло без задержки из-за `network_hover_prepare_throttle_ms = 0`.
    pub(crate) zero_throttle_no_delay_count: u64,

    /// Сколько раз новый target заменил in-flight intent без второго parallel open-а.
    pub(crate) latest_only_replaced_in_flight_count: u64,

    /// Сколько late results были проигнорированы как stale.
    pub(crate) stale_late_result_ignored_count: u64,

    /// Сколько раз throttle заблокировал старт.
    pub(crate) throttle_delay_count: u64,

    /// Последняя рассчитанная задержка до разрешённого старта.
    pub(crate) latest_throttle_delay: Option<Duration>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TimelineHoverNetworkOpenDiagnosticsCounters {
    zero_throttle_no_delay_count: u64,
    latest_only_replaced_in_flight_count: u64,
    stale_late_result_ignored_count: u64,
    throttle_delay_count: u64,
    latest_throttle_delay: Option<Duration>,
}

impl TimelineHoverNetworkOpenController {
    /// Создаёт controller без pending jobs.
    #[must_use]
    pub(crate) fn new(inter_start_throttle: Duration) -> Self {
        Self {
            source_generation: 0,
            inter_start_throttle,
            last_started_at: None,
            active_job: None,
            held_terminal_target: None,
            diagnostics: TimelineHoverNetworkOpenDiagnosticsCounters::default(),
        }
    }

    /// Обновляет throttle из committed config; source identity при этом не меняется.
    pub(crate) fn update_inter_start_throttle(&mut self, inter_start_throttle: Duration) {
        self.inter_start_throttle = inter_start_throttle;
    }

    /// Возвращает compact state для telemetry без доступа к job receiver/source identity.
    pub(crate) fn diagnostics_snapshot(&self) -> TimelineHoverNetworkOpenDiagnosticsSnapshot {
        TimelineHoverNetworkOpenDiagnosticsSnapshot {
            inter_start_throttle: self.inter_start_throttle,
            source_generation: self.source_generation,
            in_flight_count: u8::from(self.active_job.is_some()),
            failed_target_held: self.held_terminal_target.is_some(),
            zero_throttle_no_delay_count: self.diagnostics.zero_throttle_no_delay_count,
            latest_only_replaced_in_flight_count: self
                .diagnostics
                .latest_only_replaced_in_flight_count,
            stale_late_result_ignored_count: self.diagnostics.stale_late_result_ignored_count,
            throttle_delay_count: self.diagnostics.throttle_delay_count,
            latest_throttle_delay: self.diagnostics.latest_throttle_delay,
        }
    }

    /// Test-only наблюдение за pending job без раскрытия storage в production API.
    #[cfg(test)]
    pub(crate) fn has_active_job(&self) -> bool {
        self.active_job.is_some()
    }

    /// Отменяет pending network intent при уходе/замене hover target-а.
    pub(crate) fn cancel_pending_target(&mut self) {
        if let Some(job) = self.active_job.as_mut() {
            // Service APIs пока не дают cancellation token-а: receiver остаётся
            // у controller-а, чтобы тот же source не получил второй in-flight open.
            job.mark_stale();
        }
    }

    /// Инвалидирует pending network work при source/config boundary change.
    pub(crate) fn invalidate_source_context(&mut self) {
        self.source_generation = self.source_generation.wrapping_add(1);
        self.cancel_pending_target();
        self.held_terminal_target = None;
    }

    /// Пытается получить network hover source без блокировки UI/render thread-а.
    pub(crate) fn prepare_network_source(
        &mut self,
        source_factory: &TimelineHoverSourceFactory,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverNetworkOpenOutcome {
        if let Some(completed_outcome) = self.drain_completed_job() {
            return completed_outcome;
        }

        let Some(source_identity) = source_factory.active_network_source_identity() else {
            return TimelineHoverNetworkOpenOutcome::NonNetworkSource;
        };

        if self.held_terminal_target_matches(target) {
            return TimelineHoverNetworkOpenOutcome::FailedTargetHeld;
        }

        if let Some(job) = self.active_job.as_mut() {
            if job.source_identity != source_identity {
                // Новый source не обязан ждать старый remote open. Receiver drop разрывает
                // result channel, а late worker не блокирует UI/render thread.
                job.mark_stale();
                self.active_job = None;
            } else {
                if job.target != target {
                    // Для того же source target replacement остаётся latest-only:
                    // stale result будет drained/ignored, а второй open не стартует.
                    job.mark_stale();
                    self.diagnostics.record_latest_only_replaced_in_flight();
                }
                return TimelineHoverNetworkOpenOutcome::Opening;
            }
        }

        let now = Instant::now();
        if let Some(delay) = self.throttle_blocking_delay(now) {
            self.diagnostics.record_throttle_delay(delay);
            return TimelineHoverNetworkOpenOutcome::Throttled;
        }

        let receiver = spawn_network_open(source_factory.clone());
        if self.inter_start_throttle.is_zero() {
            self.diagnostics.record_zero_throttle_no_delay();
        }
        self.active_job = Some(TimelineHoverNetworkOpenJob {
            target,
            source_identity,
            source_generation: self.source_generation,
            stale: false,
            receiver,
        });
        self.last_started_at = Some(now);
        TimelineHoverNetworkOpenOutcome::Opening
    }

    fn drain_completed_job(&mut self) -> Option<TimelineHoverNetworkOpenOutcome> {
        let job = self.active_job.as_mut()?;
        match job.receiver.try_recv() {
            Ok(open_outcome) => {
                let completed_job = self.active_job.take().expect("job was just observed");
                if completed_job.stale || completed_job.source_generation != self.source_generation
                {
                    self.diagnostics.record_stale_late_result_ignored();
                    return Some(TimelineHoverNetworkOpenOutcome::Opening);
                }

                Some(match open_outcome {
                    TimelineHoverSourceOpenOutcome::Opened(source) => {
                        self.held_terminal_target = None;
                        TimelineHoverNetworkOpenOutcome::Opened(source)
                    }
                    TimelineHoverSourceOpenOutcome::MissingActiveSource => {
                        TimelineHoverNetworkOpenOutcome::MissingActiveSource
                    }
                    TimelineHoverSourceOpenOutcome::Unsupported { source_kind } => {
                        self.held_terminal_target = Some(HeldNetworkHoverTarget {
                            target: completed_job.target,
                            source_generation: completed_job.source_generation,
                        });
                        TimelineHoverNetworkOpenOutcome::Unsupported { source_kind }
                    }
                    TimelineHoverSourceOpenOutcome::OpenFailed { source_kind } => {
                        self.held_terminal_target = Some(HeldNetworkHoverTarget {
                            target: completed_job.target,
                            source_generation: completed_job.source_generation,
                        });
                        TimelineHoverNetworkOpenOutcome::OpenFailed { source_kind }
                    }
                })
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let completed_job = self.active_job.take().expect("job was just observed");
                if completed_job.stale || completed_job.source_generation != self.source_generation
                {
                    self.diagnostics.record_stale_late_result_ignored();
                    return Some(TimelineHoverNetworkOpenOutcome::Opening);
                }

                self.held_terminal_target = Some(HeldNetworkHoverTarget {
                    target: completed_job.target,
                    source_generation: completed_job.source_generation,
                });
                Some(TimelineHoverNetworkOpenOutcome::Disconnected)
            }
        }
    }

    fn held_terminal_target_matches(&self, target: TimelineHoverPrepareTarget) -> bool {
        self.held_terminal_target
            .as_ref()
            .is_some_and(|held_target| {
                held_target.target == target
                    && held_target.source_generation == self.source_generation
            })
    }

    fn throttle_blocking_delay(&self, now: Instant) -> Option<Duration> {
        if self.inter_start_throttle.is_zero() {
            return None;
        }

        let elapsed = self
            .last_started_at
            .and_then(|last_started_at| now.checked_duration_since(last_started_at))?;
        (elapsed < self.inter_start_throttle)
            .then_some(self.inter_start_throttle.saturating_sub(elapsed))
    }
}

impl TimelineHoverNetworkOpenDiagnosticsCounters {
    fn record_zero_throttle_no_delay(&mut self) {
        self.zero_throttle_no_delay_count = self.zero_throttle_no_delay_count.saturating_add(1);
    }

    fn record_latest_only_replaced_in_flight(&mut self) {
        self.latest_only_replaced_in_flight_count =
            self.latest_only_replaced_in_flight_count.saturating_add(1);
    }

    fn record_stale_late_result_ignored(&mut self) {
        self.stale_late_result_ignored_count =
            self.stale_late_result_ignored_count.saturating_add(1);
    }

    fn record_throttle_delay(&mut self, delay: Duration) {
        self.throttle_delay_count = self.throttle_delay_count.saturating_add(1);
        self.latest_throttle_delay = Some(delay);
    }
}

/// Outcome network open controller-а; детали source kind уже сохранены в source open layer.
pub(crate) enum TimelineHoverNetworkOpenOutcome {
    NonNetworkSource,
    Opened(TimelineHoverOpenedSource),
    Opening,
    Throttled,
    MissingActiveSource,
    Unsupported {
        source_kind: crate::timeline_hover_source::TimelineHoverUnsupportedSourceKind,
    },
    OpenFailed {
        source_kind: crate::timeline_hover_source::TimelineHoverOpenFailedSourceKind,
    },
    Disconnected,
    FailedTargetHeld,
}

struct TimelineHoverNetworkOpenJob {
    target: TimelineHoverPrepareTarget,
    source_identity: TimelineHoverSourceIdentity,
    source_generation: u64,
    stale: bool,
    receiver: Receiver<TimelineHoverSourceOpenOutcome>,
}

impl TimelineHoverNetworkOpenJob {
    fn mark_stale(&mut self) {
        self.stale = true;
    }
}

struct HeldNetworkHoverTarget {
    target: TimelineHoverPrepareTarget,
    source_generation: u64,
}

fn spawn_network_open(
    source_factory: TimelineHoverSourceFactory,
) -> Receiver<TimelineHoverSourceOpenOutcome> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let open_outcome = source_factory.open_active_source();
        let _ = sender.send(open_outcome);
    });
    receiver
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Sender};

    use frame_server_core::{
        BackendRevision, FrameExactnessPolicy, PlaybackGeneration, ScrubGeneration,
        ScrubGenerationToken, ScrubTrackSelection, SourceRevision, TimelineHoverFrameBucket,
    };
    use media_core::{TimeBase, TrackId, TrackTimestamp};
    use rustiplayer_config::PlayerDemuxConfig;

    use super::*;
    use crate::timeline_hover_prepare::{
        TimelineHoverPreparePlaybackMode, TimelineHoverPrepareTarget,
        TimelineHoverPrepareTargetContext,
    };
    use crate::timeline_hover_source::{
        TimelineHoverOpenFailedSourceKind, TimelineHoverSourceIdentity,
        TimelineHoverUnsupportedSourceKind,
    };

    fn timestamp(millis: i64) -> TrackTimestamp {
        TrackTimestamp::new(
            TrackId::new(1),
            millis,
            TimeBase::new(1, 1_000).expect("valid millisecond timebase"),
        )
    }

    fn hover_target(target_millis: i64) -> TimelineHoverPrepareTarget {
        let context = TimelineHoverPrepareTargetContext::new(
            SourceRevision::new(1),
            BackendRevision::new(2),
            ScrubTrackSelection::video_only(TrackId::new(1)),
            ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
            FrameExactnessPolicy::TargetOrAfter,
        );

        TimelineHoverPrepareTarget::unresolved(
            context,
            timestamp(target_millis),
            TimelineHoverFrameBucket::new(target_millis),
            TimelineHoverPreparePlaybackMode::ActivePlayback,
        )
    }

    fn network_source_factory() -> TimelineHoverSourceFactory {
        let mut source_factory = TimelineHoverSourceFactory::new(PlayerDemuxConfig::default());
        source_factory.set_active_source(TimelineHoverSourceIdentity::DirectMediaUrl(
            "https://example.invalid/video.mp4".to_string(),
        ));
        source_factory
    }

    fn install_manual_job(
        controller: &mut TimelineHoverNetworkOpenController,
        target: TimelineHoverPrepareTarget,
    ) -> Sender<TimelineHoverSourceOpenOutcome> {
        let (sender, receiver) = mpsc::channel();
        controller.active_job = Some(TimelineHoverNetworkOpenJob {
            target,
            source_identity: TimelineHoverSourceIdentity::DirectMediaUrl(
                "https://example.invalid/video.mp4".to_string(),
            ),
            source_generation: controller.source_generation,
            stale: false,
            receiver,
        });
        sender
    }

    #[test]
    fn throttle_prevents_repeated_network_open_starts() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::from_secs(10));
        controller.last_started_at = Some(Instant::now());

        let outcome =
            controller.prepare_network_source(&network_source_factory(), hover_target(1000));

        assert!(matches!(
            outcome,
            TimelineHoverNetworkOpenOutcome::Throttled
        ));
        assert!(controller.active_job.is_none());
        let diagnostics = controller.diagnostics_snapshot();
        assert_eq!(diagnostics.throttle_delay_count, 1);
        assert!(
            diagnostics
                .latest_throttle_delay
                .is_some_and(|delay| delay <= Duration::from_secs(10))
        );
    }

    #[test]
    fn zero_throttle_still_preserves_single_inflight_without_queue() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let first_target = hover_target(1000);
        let second_target = hover_target(1200);
        let _manual_sender = install_manual_job(&mut controller, first_target);

        let outcome = controller.prepare_network_source(&network_source_factory(), second_target);

        assert!(matches!(outcome, TimelineHoverNetworkOpenOutcome::Opening));
        let active_job = controller
            .active_job
            .as_ref()
            .expect("old in-flight job stays until its late result is drained");
        assert_eq!(active_job.target, first_target);
        assert!(
            active_job.stale,
            "new target must stale-mark the old job instead of queueing another open"
        );
        assert_eq!(
            controller
                .diagnostics_snapshot()
                .latest_only_replaced_in_flight_count,
            1
        );
    }

    #[test]
    fn stale_late_result_is_ignored_without_holding_failed_target() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let original_target = hover_target(1000);
        let sender = install_manual_job(&mut controller, original_target);
        controller
            .active_job
            .as_mut()
            .expect("manual job is installed")
            .mark_stale();
        sender
            .send(TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
            })
            .expect("controller still owns the receiver");

        let outcome =
            controller.prepare_network_source(&network_source_factory(), hover_target(1200));

        assert!(matches!(outcome, TimelineHoverNetworkOpenOutcome::Opening));
        assert!(controller.active_job.is_none());
        assert!(
            controller.held_terminal_target.is_none(),
            "stale failures must not block retry of the latest target"
        );
        assert_eq!(
            controller
                .diagnostics_snapshot()
                .stale_late_result_ignored_count,
            1
        );
    }

    #[test]
    fn same_failed_target_is_not_retried_automatically() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let failed_target = hover_target(1000);
        let sender = install_manual_job(&mut controller, failed_target);
        sender
            .send(TimelineHoverSourceOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
            })
            .expect("controller still owns the receiver");

        let first_outcome =
            controller.prepare_network_source(&network_source_factory(), failed_target);
        let second_outcome =
            controller.prepare_network_source(&network_source_factory(), failed_target);

        assert!(matches!(
            first_outcome,
            TimelineHoverNetworkOpenOutcome::OpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl
            }
        ));
        assert!(matches!(
            second_outcome,
            TimelineHoverNetworkOpenOutcome::FailedTargetHeld
        ));
    }

    #[test]
    fn unsupported_target_is_not_retried_automatically() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let unsupported_target = hover_target(1000);
        let sender = install_manual_job(&mut controller, unsupported_target);
        sender
            .send(TimelineHoverSourceOpenOutcome::Unsupported {
                source_kind: TimelineHoverUnsupportedSourceKind::DirectMediaUrl,
            })
            .expect("controller still owns the receiver");

        let first_outcome =
            controller.prepare_network_source(&network_source_factory(), unsupported_target);
        let second_outcome =
            controller.prepare_network_source(&network_source_factory(), unsupported_target);

        assert!(matches!(
            first_outcome,
            TimelineHoverNetworkOpenOutcome::Unsupported {
                source_kind: TimelineHoverUnsupportedSourceKind::DirectMediaUrl
            }
        ));
        assert!(matches!(
            second_outcome,
            TimelineHoverNetworkOpenOutcome::FailedTargetHeld
        ));
    }

    #[test]
    fn source_invalidation_marks_pending_job_stale_and_clears_terminal_target() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let failed_target = hover_target(1000);
        let _manual_sender = install_manual_job(&mut controller, failed_target);
        controller.held_terminal_target = Some(HeldNetworkHoverTarget {
            target: failed_target,
            source_generation: controller.source_generation,
        });

        controller.invalidate_source_context();

        assert_eq!(controller.source_generation, 1);
        let active_job = controller
            .active_job
            .as_ref()
            .expect("old in-flight job stays tracked until stale result is drained");
        assert!(active_job.stale);
        assert!(controller.held_terminal_target.is_none());
    }

    #[test]
    fn target_cancellation_keeps_same_source_inflight_and_failure_hold() {
        let mut controller = TimelineHoverNetworkOpenController::new(Duration::ZERO);
        let failed_target = hover_target(1000);
        let _manual_sender = install_manual_job(&mut controller, failed_target);
        controller.held_terminal_target = Some(HeldNetworkHoverTarget {
            target: failed_target,
            source_generation: controller.source_generation,
        });

        controller.cancel_pending_target();

        assert_eq!(controller.source_generation, 0);
        let active_job = controller
            .active_job
            .as_ref()
            .expect("same-source in-flight job must stay tracked after cancel");
        assert!(active_job.stale);
        assert!(controller.held_terminal_target_matches(failed_target));

        let retry_outcome =
            controller.prepare_network_source(&network_source_factory(), failed_target);
        assert!(matches!(
            retry_outcome,
            TimelineHoverNetworkOpenOutcome::FailedTargetHeld
        ));
    }
}
