//! Счётчики пробуждений player worker-а за окно diagnostics summary.
//!
//! Владелец — worker runtime: только он знает, *почему* выполнился playback tick
//! (наступил запланированный deadline, пришла decoder activity или tick догнал
//! просроченный план после пачки команд). `PlaybackDiagnostics` хранит
//! накопительные счётчики media pipeline-а с момента загрузки media, а эти
//! счётчики — оконные: каждое окно заканчивается на строке
//! `Playback diagnostics summary` и начинается заново. Так в логе сразу видно
//! «тиков в секунду», а не разность двух накопленных значений.

use std::time::{Duration, Instant};

use crate::WorkerWakeupReason;

/// Что именно запустило playback tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerTickTrigger {
    /// Наступил deadline плана (timeout `select!` или уже просроченный план).
    PlannedDeadline,

    /// Decoder сообщил о новой activity раньше deadline-а.
    DecoderActivity,

    /// Fairness-точка после пачки команд нашла уже просроченный план.
    AfterCommands,
}

/// Все причины плана в стабильном порядке вывода.
///
/// Порядок совпадает с `reason_slot`: это проверяет unit-тест, поэтому новая
/// причина не может молча выпасть из сводки.
const ALL_WAKEUP_REASONS: [WorkerWakeupReason; 9] = [
    WorkerWakeupReason::Idle,
    WorkerWakeupReason::PipelineWorkReady,
    WorkerWakeupReason::DecodeReadiness,
    WorkerWakeupReason::DemuxRetryDeadline,
    WorkerWakeupReason::StagedPreflightDeadline,
    WorkerWakeupReason::FramePtsDeadline,
    WorkerWakeupReason::FrameReady,
    WorkerWakeupReason::SeekOrPreroll,
    WorkerWakeupReason::CoarseProgress,
];

/// Индекс причины в массиве счётчиков; match без wildcard заставит учесть новую причину.
const fn reason_slot(reason: WorkerWakeupReason) -> usize {
    match reason {
        WorkerWakeupReason::Idle => 0,
        WorkerWakeupReason::PipelineWorkReady => 1,
        WorkerWakeupReason::DecodeReadiness => 2,
        WorkerWakeupReason::DemuxRetryDeadline => 3,
        WorkerWakeupReason::StagedPreflightDeadline => 4,
        WorkerWakeupReason::FramePtsDeadline => 5,
        WorkerWakeupReason::FrameReady => 6,
        WorkerWakeupReason::SeekOrPreroll => 7,
        WorkerWakeupReason::CoarseProgress => 8,
    }
}

/// Оконные счётчики пробуждений, которыми владеет worker runtime.
#[derive(Debug)]
pub(super) struct WorkerWakeupCounters {
    /// Начало текущего окна.
    window_started_at: Instant,

    /// Тики, запущенные наступившим deadline-ом.
    ticks_planned_deadline: u64,

    /// Тики, запущенные decoder activity раньше deadline-а.
    ticks_decoder_activity: u64,

    /// Тики, догнавшие просроченный план после пачки команд.
    ticks_after_commands: u64,

    /// Тики по причине плана, индекс — `reason_slot`.
    ticks_by_reason: [u64; ALL_WAKEUP_REASONS.len()],

    /// Сколько decoder pulse-ов worker принял в `select!` (включая устаревшие,
    /// которые epoch-проверка отбросила без tick-а).
    decoder_pulses: u64,

    /// Пробуждения от render feedback (release, acquire/timing/lock samples):
    /// они не запускают tick, но будят поток и стоят CPU.
    render_feedback_wakeups: u64,
}

impl WorkerWakeupCounters {
    /// Начинает первое окно в момент старта worker-а.
    #[must_use]
    pub(super) const fn new(window_started_at: Instant) -> Self {
        Self {
            window_started_at,
            ticks_planned_deadline: 0,
            ticks_decoder_activity: 0,
            ticks_after_commands: 0,
            ticks_by_reason: [0; ALL_WAKEUP_REASONS.len()],
            decoder_pulses: 0,
            render_feedback_wakeups: 0,
        }
    }

    /// Учитывает один выполненный playback tick.
    pub(super) fn record_tick(&mut self, trigger: WorkerTickTrigger, reason: WorkerWakeupReason) {
        let trigger_counter = match trigger {
            WorkerTickTrigger::PlannedDeadline => &mut self.ticks_planned_deadline,
            WorkerTickTrigger::DecoderActivity => &mut self.ticks_decoder_activity,
            WorkerTickTrigger::AfterCommands => &mut self.ticks_after_commands,
        };
        *trigger_counter = trigger_counter.saturating_add(1);
        let reason_counter = &mut self.ticks_by_reason[reason_slot(reason)];
        *reason_counter = reason_counter.saturating_add(1);
    }

    /// Учитывает decoder pulse, принятый в `select!`.
    pub(super) fn record_decoder_pulse(&mut self) {
        self.decoder_pulses = self.decoder_pulses.saturating_add(1);
    }

    /// Учитывает пробуждение от render feedback event-а.
    pub(super) fn record_render_feedback_wakeup(&mut self) {
        self.render_feedback_wakeups = self.render_feedback_wakeups.saturating_add(1);
    }

    /// Закрывает текущее окно, возвращает его итог и начинает новое с `now`.
    pub(super) fn take_window(&mut self, now: Instant) -> WorkerWakeupWindow {
        let finished = WorkerWakeupWindow {
            duration: now.saturating_duration_since(self.window_started_at),
            ticks_planned_deadline: self.ticks_planned_deadline,
            ticks_decoder_activity: self.ticks_decoder_activity,
            ticks_after_commands: self.ticks_after_commands,
            ticks_by_reason: self.ticks_by_reason,
            decoder_pulses: self.decoder_pulses,
            render_feedback_wakeups: self.render_feedback_wakeups,
        };
        *self = Self::new(now);
        finished
    }
}

/// Итог одного окна счётчиков для строки diagnostics summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkerWakeupWindow {
    /// Длина окна.
    pub(super) duration: Duration,

    /// Тики по наступившему deadline-у.
    pub(super) ticks_planned_deadline: u64,

    /// Тики по decoder activity.
    pub(super) ticks_decoder_activity: u64,

    /// Тики после пачки команд.
    pub(super) ticks_after_commands: u64,

    /// Тики по причине плана, индекс — `reason_slot`.
    ticks_by_reason: [u64; ALL_WAKEUP_REASONS.len()],

    /// Принятые decoder pulse-ы.
    pub(super) decoder_pulses: u64,

    /// Пробуждения от render feedback event-ов.
    pub(super) render_feedback_wakeups: u64,
}

impl WorkerWakeupWindow {
    /// Все тики окна независимо от повода.
    #[must_use]
    pub(super) const fn ticks_total(&self) -> u64 {
        self.ticks_planned_deadline
            .saturating_add(self.ticks_decoder_activity)
            .saturating_add(self.ticks_after_commands)
    }

    /// Сколько тиков выполнено по данной причине плана.
    #[must_use]
    pub(super) const fn ticks_for_reason(&self, reason: WorkerWakeupReason) -> u64 {
        self.ticks_by_reason[reason_slot(reason)]
    }

    /// Компактная строка «причина=число» только для ненулевых причин.
    ///
    /// Формат для лога: `frame_pts_deadline=60 frame_ready=60`; пустое окно — `none`.
    #[must_use]
    pub(super) fn reasons_for_log(&self) -> String {
        let mut formatted = String::new();
        for reason in ALL_WAKEUP_REASONS {
            let ticks = self.ticks_for_reason(reason);
            if ticks == 0 {
                continue;
            }
            if !formatted.is_empty() {
                formatted.push(' ');
            }
            formatted.push_str(&format!("{}={ticks}", reason.metric_name()));
        }
        if formatted.is_empty() {
            formatted.push_str("none");
        }
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_slots_match_output_order() {
        for (expected_slot, reason) in ALL_WAKEUP_REASONS.into_iter().enumerate() {
            assert_eq!(reason_slot(reason), expected_slot, "{reason:?}");
        }
    }

    #[test]
    fn window_counts_ticks_by_trigger_reason_and_pulses() {
        let started_at = Instant::now();
        let mut counters = WorkerWakeupCounters::new(started_at);

        counters.record_tick(
            WorkerTickTrigger::PlannedDeadline,
            WorkerWakeupReason::FramePtsDeadline,
        );
        counters.record_tick(
            WorkerTickTrigger::PlannedDeadline,
            WorkerWakeupReason::FrameReady,
        );
        counters.record_tick(
            WorkerTickTrigger::DecoderActivity,
            WorkerWakeupReason::FramePtsDeadline,
        );
        counters.record_tick(
            WorkerTickTrigger::AfterCommands,
            WorkerWakeupReason::PipelineWorkReady,
        );
        counters.record_decoder_pulse();
        counters.record_decoder_pulse();
        counters.record_render_feedback_wakeup();

        let window = counters.take_window(started_at + Duration::from_secs(2));

        assert_eq!(window.duration, Duration::from_secs(2));
        assert_eq!(window.ticks_total(), 4);
        assert_eq!(window.ticks_planned_deadline, 2);
        assert_eq!(window.ticks_decoder_activity, 1);
        assert_eq!(window.ticks_after_commands, 1);
        assert_eq!(window.decoder_pulses, 2);
        assert_eq!(window.render_feedback_wakeups, 1);
        assert_eq!(
            window.ticks_for_reason(WorkerWakeupReason::FramePtsDeadline),
            2
        );
        assert_eq!(
            window.ticks_for_reason(WorkerWakeupReason::DecodeReadiness),
            0
        );
        assert_eq!(
            window.reasons_for_log(),
            "pipeline_work_ready=1 frame_pts_deadline=2 frame_ready=1"
        );
    }

    #[test]
    fn taking_window_starts_a_fresh_empty_window() {
        let started_at = Instant::now();
        let mut counters = WorkerWakeupCounters::new(started_at);
        counters.record_tick(
            WorkerTickTrigger::PlannedDeadline,
            WorkerWakeupReason::DecodeReadiness,
        );
        counters.record_decoder_pulse();
        let window_end = started_at + Duration::from_secs(2);
        let _first_window = counters.take_window(window_end);

        let second_window = counters.take_window(window_end + Duration::from_secs(1));

        assert_eq!(second_window.duration, Duration::from_secs(1));
        assert_eq!(second_window.ticks_total(), 0);
        assert_eq!(second_window.decoder_pulses, 0);
        assert_eq!(second_window.reasons_for_log(), "none");
    }
}
