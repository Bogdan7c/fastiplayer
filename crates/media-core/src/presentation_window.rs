use std::cmp::Ordering;

use thiserror::Error;

use crate::{TimeBase, TrackId, TrackTimestamp};

/// Точная полуоткрытая граница показа packet-а в исходной временной базе трека.
///
/// Диапазон всегда имеет форму `[start, end_exclusive)` и не допускает
/// отрицательного начала, пустого интервала или смешения разных track clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactPresentationWindow {
    /// Первая допустимая presentation-позиция.
    start: TrackTimestamp,

    /// Первая presentation-позиция, уже не входящая в окно.
    end_exclusive: TrackTimestamp,
}

impl ExactPresentationWindow {
    /// Создаёт точное окно только из согласованных границ одного track clock.
    pub fn new(
        start: TrackTimestamp,
        end_exclusive: TrackTimestamp,
    ) -> Result<Self, ExactPresentationWindowError> {
        if start.track_id.get() != end_exclusive.track_id.get() {
            return Err(ExactPresentationWindowError::TrackMismatch {
                start_track_id: start.track_id,
                end_track_id: end_exclusive.track_id,
            });
        }

        if start.time_base.numer != end_exclusive.time_base.numer
            || start.time_base.denom != end_exclusive.time_base.denom
        {
            return Err(ExactPresentationWindowError::TimeBaseMismatch {
                start_time_base: start.time_base,
                end_time_base: end_exclusive.time_base,
            });
        }

        if start.units.is_negative() {
            return Err(ExactPresentationWindowError::NegativeStart { start });
        }

        match start.cmp_timeline_position(end_exclusive) {
            Ordering::Less => {}
            Ordering::Equal => {
                return Err(ExactPresentationWindowError::Empty { boundary: start });
            }
            Ordering::Greater => {
                return Err(ExactPresentationWindowError::Reversed {
                    start,
                    end_exclusive,
                });
            }
        }

        Ok(Self {
            start,
            end_exclusive,
        })
    }

    /// Возвращает включённую левую границу окна.
    #[must_use]
    pub const fn start(self) -> TrackTimestamp {
        self.start
    }

    /// Возвращает исключённую правую границу окна.
    #[must_use]
    pub const fn end_exclusive(self) -> TrackTimestamp {
        self.end_exclusive
    }

    /// Атомарно remap-ит обе границы на новый внешний track id.
    #[must_use]
    pub(crate) const fn with_track_id(self, track_id: TrackId) -> Self {
        Self {
            start: self.start.with_track_id(track_id),
            end_exclusive: self.end_exclusive.with_track_id(track_id),
        }
    }
}

/// Причина, по которой две track timestamp-границы не образуют точное окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ExactPresentationWindowError {
    /// Границы принадлежат разным трекам.
    #[error(
        "границы presentation window принадлежат разным трекам: start={start_track_id:?}, end={end_track_id:?}"
    )]
    TrackMismatch {
        /// Track включённой левой границы.
        start_track_id: TrackId,

        /// Track исключённой правой границы.
        end_track_id: TrackId,
    },

    /// Границы используют разные временные базы.
    #[error(
        "границы presentation window используют разные time base: start={start_time_base:?}, end={end_time_base:?}"
    )]
    TimeBaseMismatch {
        /// Временная база включённой левой границы.
        start_time_base: TimeBase,

        /// Временная база исключённой правой границы.
        end_time_base: TimeBase,
    },

    /// Левая граница находится раньше нулевой позиции трека.
    #[error("начало presentation window отрицательное: {start:?}")]
    NegativeStart {
        /// Недопустимая левая граница.
        start: TrackTimestamp,
    },

    /// Обе границы совпадают и образуют пустой полуоткрытый диапазон.
    #[error("presentation window пусто на границе {boundary:?}")]
    Empty {
        /// Совпавшая граница.
        boundary: TrackTimestamp,
    },

    /// Левая граница расположена после правой.
    #[error(
        "presentation window развёрнуто в обратном порядке: start={start:?}, end={end_exclusive:?}"
    )]
    Reversed {
        /// Недопустимая левая граница.
        start: TrackTimestamp,

        /// Недопустимая правая граница.
        end_exclusive: TrackTimestamp,
    },
}

/// Точное ограничение показа packet-а на neutral media boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketPresentationWindow {
    /// Packet не несёт точного ограничения presentation interval.
    Unbounded,

    /// Packet разрешено показывать только внутри проверенного полуоткрытого окна.
    Bounded(ExactPresentationWindow),
}

impl PacketPresentationWindow {
    /// Сохраняет вариант окна и remap-ит обе bounded-границы при смене track id.
    #[must_use]
    pub(crate) const fn with_track_id(self, track_id: TrackId) -> Self {
        match self {
            Self::Unbounded => Self::Unbounded,
            Self::Bounded(window) => Self::Bounded(window.with_track_id(track_id)),
        }
    }
}

/// Причина, по которой точное окно нельзя присоединить к конкретному packet-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PacketPresentationWindowAssignmentError {
    /// Packet не несёт raw PTS, поэтому его presentation clock нельзя доказать.
    #[error("packet не содержит обязательный raw presentation timestamp")]
    MissingPacketPresentationTimestamp,

    /// Packet и окно принадлежат разным трекам.
    #[error(
        "packet и presentation window принадлежат разным трекам: packet={packet_track_id:?}, window={window_track_id:?}"
    )]
    TrackMismatch {
        /// Track packet-а.
        packet_track_id: TrackId,

        /// Track окна.
        window_track_id: TrackId,
    },

    /// Raw timing packet-а не принадлежит заявленному track id.
    #[error(
        "raw timing packet-а принадлежит другому треку: packet={packet_track_id:?}, timing={timing_track_id:?}"
    )]
    PacketTimingTrackMismatch {
        /// Track packet-а.
        packet_track_id: TrackId,

        /// Track одного из raw timing-значений.
        timing_track_id: TrackId,
    },

    /// Raw timing packet-а и окно используют разные временные базы.
    #[error(
        "packet timing и presentation window используют разные time base: packet={packet_time_base:?}, window={window_time_base:?}"
    )]
    TimeBaseMismatch {
        /// Временная база одного из raw timing-значений packet-а.
        packet_time_base: TimeBase,

        /// Временная база точного окна.
        window_time_base: TimeBase,
    },
}

/// Проверяет один raw track clock packet-а относительно exact presentation window.
pub(crate) fn validate_packet_track_clock(
    packet_track_id: TrackId,
    timing_track_id: TrackId,
    packet_time_base: TimeBase,
    window: ExactPresentationWindow,
) -> Result<(), PacketPresentationWindowAssignmentError> {
    if timing_track_id != packet_track_id {
        return Err(
            PacketPresentationWindowAssignmentError::PacketTimingTrackMismatch {
                packet_track_id,
                timing_track_id,
            },
        );
    }

    let window_time_base = window.start().time_base;
    if packet_time_base != window_time_base {
        return Err(PacketPresentationWindowAssignmentError::TimeBaseMismatch {
            packet_time_base,
            window_time_base,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        ExactPresentationWindow, ExactPresentationWindowError, TimeBase, TrackId, TrackTimestamp,
    };

    /// Создаёт test timestamp без скрытой нормализации или пересчёта.
    fn timestamp(track_id: u32, units: i64, time_base: TimeBase) -> TrackTimestamp {
        TrackTimestamp::new(TrackId::new(track_id), units, time_base)
    }

    #[test]
    fn exact_window_rejects_mismatched_track_ids() {
        let time_base = TimeBase::new(1, 1_000).expect("test time base должна быть валидной");

        let error =
            ExactPresentationWindow::new(timestamp(1, 10, time_base), timestamp(2, 20, time_base))
                .expect_err("разные track id должны быть отклонены");

        assert!(matches!(
            error,
            ExactPresentationWindowError::TrackMismatch { .. }
        ));
    }

    #[test]
    fn exact_window_rejects_mismatched_time_bases() {
        let millisecond_time_base =
            TimeBase::new(1, 1_000).expect("test time base должна быть валидной");
        let video_time_base =
            TimeBase::new(1, 90_000).expect("test time base должна быть валидной");

        let error = ExactPresentationWindow::new(
            timestamp(1, 10, millisecond_time_base),
            timestamp(1, 20, video_time_base),
        )
        .expect_err("разные time base должны быть отклонены");

        assert!(matches!(
            error,
            ExactPresentationWindowError::TimeBaseMismatch { .. }
        ));
    }

    #[test]
    fn exact_window_rejects_negative_start() {
        let time_base = TimeBase::new(1, 1_000).expect("test time base должна быть валидной");

        let error =
            ExactPresentationWindow::new(timestamp(1, -1, time_base), timestamp(1, 20, time_base))
                .expect_err("отрицательное начало должно быть отклонено");

        assert!(matches!(
            error,
            ExactPresentationWindowError::NegativeStart { .. }
        ));
    }

    #[test]
    fn exact_window_rejects_empty_interval() {
        let time_base = TimeBase::new(1, 1_000).expect("test time base должна быть валидной");

        let error =
            ExactPresentationWindow::new(timestamp(1, 10, time_base), timestamp(1, 10, time_base))
                .expect_err("пустое окно должно быть отклонено");

        assert!(matches!(error, ExactPresentationWindowError::Empty { .. }));
    }

    #[test]
    fn exact_window_rejects_reversed_interval() {
        let time_base = TimeBase::new(1, 1_000).expect("test time base должна быть валидной");

        let error =
            ExactPresentationWindow::new(timestamp(1, 20, time_base), timestamp(1, 10, time_base))
                .expect_err("развёрнутое окно должно быть отклонено");

        assert!(matches!(
            error,
            ExactPresentationWindowError::Reversed { .. }
        ));
    }
}
