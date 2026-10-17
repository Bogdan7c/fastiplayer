//! Учёт `continuity_counter` по PID-ам (ISO/IEC 13818-1 §2.4.3.3).
//!
//! Модуль только классифицирует очередной пакет с payload-ом: «по порядку»,
//! «дубликат» или «разрыв». Что делать с разрывом (какой PES отбросить, что
//! дособрать), решает `MpegTsDemuxer` (`demuxer_continuity.rs`): разрыв
//! непрерывности транспорта — это повреждение или склейка записей, а не смена
//! дорожек.

use std::collections::HashMap;

/// Что означает `continuity_counter` очередного пакета с payload-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuityVerdict {
    /// Первый пакет PID-а после старта/сброса или ровно следующий счётчик.
    InOrder,
    /// Повтор предыдущего пакета: тот же счётчик и те же байты payload-а.
    /// Стандарт разрешает передать пакет дважды; второй экземпляр пропускается.
    Duplicate,
    /// Пропуск: потеряны пакеты либо началась склеенная запись со своим счётчиком.
    Gap,
}

/// Последний принятый пакет PID-а — нужен, чтобы отличить дубликат от
/// совпадения счётчика на стыке склеенных записей (1 шанс из 16).
#[derive(Debug, Clone)]
struct LastPayload {
    counter: u8,
    payload: Vec<u8>,
}

/// Per-PID состояние continuity; владелец — `MpegTsDemuxer`.
#[derive(Debug, Default, Clone)]
pub(crate) struct ContinuityTracker {
    last_by_pid: HashMap<u16, LastPayload>,
}

impl ContinuityTracker {
    /// Классифицирует пакет с непустым payload-ом и запоминает его (кроме дубликата).
    pub(crate) fn observe(&mut self, pid: u16, counter: u8, payload: &[u8]) -> ContinuityVerdict {
        let Some(last) = self.last_by_pid.get_mut(&pid) else {
            self.last_by_pid.insert(
                pid,
                LastPayload {
                    counter,
                    payload: payload.to_vec(),
                },
            );
            return ContinuityVerdict::InOrder;
        };
        // Тот же счётчик без тех же байт — не повтор: раньше такой пакет молча
        // выбрасывался как «дубликат», и на стыке склейки терялся первый пакет
        // (PES header с IDR) следующей записи.
        if counter == last.counter && payload == last.payload.as_slice() {
            return ContinuityVerdict::Duplicate;
        }
        let verdict = if counter == (last.counter + 1) & 0x0f {
            ContinuityVerdict::InOrder
        } else {
            ContinuityVerdict::Gap
        };
        last.counter = counter;
        last.payload.clear();
        last.payload.extend_from_slice(payload);
        verdict
    }

    /// Забывает PID: следующий пакет будет первым (сброс PID-а, discontinuity_indicator).
    pub(crate) fn forget(&mut self, pid: u16) {
        self.last_by_pid.remove(&pid);
    }

    /// Забывает все PID-ы (новый ordered segment, seek, смена topology).
    pub(crate) fn clear(&mut self) {
        self.last_by_pid.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PID: u16 = 0x100;

    #[test]
    fn first_packet_and_next_counter_are_in_order_including_wrap() {
        let mut tracker = ContinuityTracker::default();
        assert_eq!(tracker.observe(PID, 14, &[1]), ContinuityVerdict::InOrder);
        assert_eq!(tracker.observe(PID, 15, &[2]), ContinuityVerdict::InOrder);
        assert_eq!(tracker.observe(PID, 0, &[3]), ContinuityVerdict::InOrder);
    }

    #[test]
    fn identical_repeat_is_duplicate_but_same_counter_with_other_bytes_is_gap() {
        let mut tracker = ContinuityTracker::default();
        tracker.observe(PID, 3, &[7, 7]);
        assert_eq!(
            tracker.observe(PID, 3, &[7, 7]),
            ContinuityVerdict::Duplicate
        );
        // Дубликат не меняет состояние: следующий счётчик по-прежнему 4.
        assert_eq!(tracker.observe(PID, 4, &[8]), ContinuityVerdict::InOrder);
        assert_eq!(tracker.observe(PID, 4, &[9]), ContinuityVerdict::Gap);
        // Разрыв принят как новая точка отсчёта.
        assert_eq!(tracker.observe(PID, 5, &[1]), ContinuityVerdict::InOrder);
    }

    #[test]
    fn skipped_counter_is_gap_and_pids_are_independent() {
        let mut tracker = ContinuityTracker::default();
        tracker.observe(PID, 1, &[1]);
        tracker.observe(PID + 1, 9, &[1]);
        assert_eq!(tracker.observe(PID, 3, &[2]), ContinuityVerdict::Gap);
        assert_eq!(
            tracker.observe(PID + 1, 10, &[2]),
            ContinuityVerdict::InOrder
        );
    }

    #[test]
    fn forgotten_pid_starts_again_without_touching_others() {
        let mut tracker = ContinuityTracker::default();
        tracker.observe(PID, 1, &[1]);
        tracker.observe(PID + 1, 1, &[1]);
        tracker.forget(PID);
        assert_eq!(tracker.observe(PID, 9, &[2]), ContinuityVerdict::InOrder);
        assert_eq!(tracker.observe(PID + 1, 9, &[2]), ContinuityVerdict::Gap);
        tracker.clear();
        assert_eq!(
            tracker.observe(PID + 1, 4, &[3]),
            ContinuityVerdict::InOrder
        );
    }
}
