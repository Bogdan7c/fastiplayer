//! Эскалация затянувшегося output backpressure в decoder thread-е.
//!
//! Пул выходных VA surfaces ограничен, поэтому «нет свободной surface» — штатный
//! backpressure: decoder thread повторяет packet каждые несколько миллисекунд и
//! пишет только `debug`. Но если плеер или рендерер забыл отпустить кадр, повторы
//! никогда не кончатся, а пользователь увидит замёрзшую картинку без единого
//! `warn`. Этот модуль различает короткое ожидание и застревание: первое остаётся
//! тихим, второе один раз (и затем редко повторно) попадает в лог вместе со
//! счётчиками пула, чтобы причину было видно без отладочной сборки.

use std::time::{Duration, Instant};

use tracing::warn;

/// Через сколько непрерывного backpressure первый раз писать `warn`.
///
/// Норма — единицы кадров (десятки миллисекунд): столько кадр живёт у рендерера
/// до `on_submitted_work_done`. Секунда — заведомо не норма даже для 24 fps.
pub(super) const OUTPUT_BACKPRESSURE_STALL_WARN_AFTER: Duration = Duration::from_secs(1);

/// Как часто повторять `warn`, пока застревание продолжается.
pub(super) const OUTPUT_BACKPRESSURE_STALL_REPEAT_EVERY: Duration = Duration::from_secs(5);

/// Следит за непрерывностью output backpressure одного и того же packet-а.
#[derive(Debug)]
pub(super) struct OutputBackpressureStallMonitor {
    warn_after: Duration,
    repeat_every: Duration,
    /// Когда текущая серия backpressure началась; `None` — серии нет.
    stalled_since: Option<Instant>,
    /// Когда последний раз писали `warn` в текущей серии.
    last_warning_at: Option<Instant>,
}

/// Сигнал «пора писать warn» с длительностью текущего застревания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutputBackpressureStallWarning {
    /// Сколько packet уже не принимается из-за backpressure.
    pub(super) stalled_for: Duration,
    /// Это повторный warn в той же серии (первый уже был).
    pub(super) repeated: bool,
}

impl OutputBackpressureStallMonitor {
    /// Монитор с production-порогами.
    pub(super) fn new() -> Self {
        Self::with_thresholds(
            OUTPUT_BACKPRESSURE_STALL_WARN_AFTER,
            OUTPUT_BACKPRESSURE_STALL_REPEAT_EVERY,
        )
    }

    /// Монитор с явными порогами (тесты задают короткие).
    pub(super) fn with_thresholds(warn_after: Duration, repeat_every: Duration) -> Self {
        Self {
            warn_after,
            repeat_every,
            stalled_since: None,
            last_warning_at: None,
        }
    }

    /// Packet снова не принят из-за backpressure.
    ///
    /// Возвращает `Some`, когда серия длится дольше порога и warn ещё не писали
    /// (или пора повторить). Между повторами молчит.
    pub(super) fn note_backpressured(
        &mut self,
        now: Instant,
    ) -> Option<OutputBackpressureStallWarning> {
        let stalled_since = *self.stalled_since.get_or_insert(now);
        let stalled_for = now.saturating_duration_since(stalled_since);
        if stalled_for < self.warn_after {
            return None;
        }
        match self.last_warning_at {
            None => {
                self.last_warning_at = Some(now);
                Some(OutputBackpressureStallWarning {
                    stalled_for,
                    repeated: false,
                })
            }
            Some(last_warning_at)
                if now.saturating_duration_since(last_warning_at) >= self.repeat_every =>
            {
                self.last_warning_at = Some(now);
                Some(OutputBackpressureStallWarning {
                    stalled_for,
                    repeated: true,
                })
            }
            Some(_) => None,
        }
    }

    /// Backpressure снято: packet принят, либо его убрал flush/reconfigure.
    pub(super) fn note_progress(&mut self) {
        self.stalled_since = None;
        self.last_warning_at = None;
    }

    /// Поток уходит ждать, пока плеер заберёт кадр (канал кадров полон).
    ///
    /// Пока канал полон, декодер стоит из-за потребителя, а не из-за нехватки
    /// surfaces: это время нельзя приписывать застою пула. С очередью «в полёте»
    /// (сессия 09a) декодер успевает взять следующий packet раньше, чем
    /// канал заполнится, и тот «паркуется» на исчерпанном пуле; без сброса
    /// удержание scrub на паузе давало ложный warn «frame leak». Реальную
    /// утечку сброс не прячет: при ней новых кадров нет, канал не наполняется,
    /// и повторы идут непрерывно до порога.
    pub(super) fn note_waiting_for_frame_consumer(&mut self) {
        self.note_progress();
    }
}

/// Пишет warn о застрявшем backpressure вместе с состоянием пула и reclaim queue.
///
/// Именно этих чисел хватает, чтобы отличить утечку кадра у плеера/рендерера
/// (`free_surfaces == 0`, `live == capacity`, release не приходит) от медленного
/// GPU (reclaim queue полна, surfaces не готовы).
pub(super) fn log_output_backpressure_stall(
    decoder: &crate::VaapiVideoDecoder,
    warning: OutputBackpressureStallWarning,
) {
    let pool = decoder.output_surface_pool_stats();
    warn!(
        stalled_ms = warning.stalled_for.as_millis(),
        repeated = warning.repeated,
        pool_capacity = pool.capacity,
        pool_live_surfaces = pool.live_surfaces,
        pool_free_surfaces = pool.free_surfaces,
        pool_exhausted_lease_attempts = pool.exhausted_lease_attempts,
        pool_surfaces_destroyed_not_idle = pool.surfaces_destroyed_not_idle,
        suppressed_reclaim_depth = decoder.suppressed_reclaim_depth(),
        "Video decoder is stuck on output backpressure: no VA output surface has been \
         released for a long time (frame leak in player/renderer or stalled GPU?)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> OutputBackpressureStallMonitor {
        OutputBackpressureStallMonitor::with_thresholds(
            Duration::from_millis(100),
            Duration::from_millis(500),
        )
    }

    #[test]
    fn short_backpressure_stays_silent() {
        let mut monitor = monitor();
        let start = Instant::now();

        assert_eq!(monitor.note_backpressured(start), None);
        assert_eq!(
            monitor.note_backpressured(start + Duration::from_millis(99)),
            None
        );
    }

    #[test]
    fn stall_warns_once_after_threshold_and_repeats_only_after_repeat_interval() {
        let mut monitor = monitor();
        let start = Instant::now();
        assert_eq!(monitor.note_backpressured(start), None);

        let first = monitor.note_backpressured(start + Duration::from_millis(100));
        assert_eq!(
            first,
            Some(OutputBackpressureStallWarning {
                stalled_for: Duration::from_millis(100),
                repeated: false,
            })
        );
        assert_eq!(
            monitor.note_backpressured(start + Duration::from_millis(400)),
            None,
            "no spam between repeats"
        );

        let repeated = monitor.note_backpressured(start + Duration::from_millis(600));
        assert_eq!(
            repeated,
            Some(OutputBackpressureStallWarning {
                stalled_for: Duration::from_millis(600),
                repeated: true,
            })
        );
    }

    #[test]
    fn waiting_for_frame_consumer_is_not_counted_as_surface_stall() {
        let mut monitor = monitor();
        let start = Instant::now();
        assert_eq!(monitor.note_backpressured(start), None);

        // Поток долго ждал плеер (канал кадров полон), потом снова упёрся в пул.
        monitor.note_waiting_for_frame_consumer();
        let after_consumer_wait = start + Duration::from_secs(3);
        assert_eq!(
            monitor.note_backpressured(after_consumer_wait),
            None,
            "time spent waiting for the frame consumer must not become a pool stall"
        );
        assert_eq!(
            monitor.note_backpressured(after_consumer_wait + Duration::from_millis(100)),
            Some(OutputBackpressureStallWarning {
                stalled_for: Duration::from_millis(100),
                repeated: false,
            }),
            "a real continuous pool stall after the wait must still warn"
        );
    }

    #[test]
    fn progress_resets_the_series_so_the_next_stall_warns_again_as_first() {
        let mut monitor = monitor();
        let start = Instant::now();
        assert_eq!(monitor.note_backpressured(start), None);
        assert!(
            monitor
                .note_backpressured(start + Duration::from_millis(150))
                .is_some()
        );

        monitor.note_progress();
        let resumed = start + Duration::from_secs(1);
        assert_eq!(
            monitor.note_backpressured(resumed),
            None,
            "a new series starts from zero"
        );
        assert_eq!(
            monitor.note_backpressured(resumed + Duration::from_millis(100)),
            Some(OutputBackpressureStallWarning {
                stalled_for: Duration::from_millis(100),
                repeated: false,
            })
        );
    }
}
