//! Однократное сообщение о постоянной причине, по которой программа не звучит.
//!
//! Битый packet — случайность: его маскирует тишина, и следующий packet
//! обычно декодируется. Другое дело — корректный поток, который adapter не
//! может отдать: неподдерживаемая возможность декодера или раскладка без
//! нейтрального представления. Такая причина повторяется в КАЖДОМ packet-е, а
//! player-core на каждую ошибку `decode` выставляет runtime error. Поэтому
//! adapter сообщает причину ошибкой один раз, а пока она не меняется, отдаёт
//! тишину без новых ошибок. Первый удачный кадр снимает состояние: если
//! проблема вернётся, о ней снова сообщат один раз.

use ac3_decoder::{ChannelPosition, UnsupportedFeature};

/// Постоянная причина, по которой кадр нельзя отдать на neutral boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PersistentFailure {
    /// Декодер распознал корректную возможность потока, но её не реализует.
    UnsupportedFeature(UnsupportedFeature),
    /// Раскладка каналов программы не переводится в нейтральные позиции.
    UnconvertibleLayout(Vec<ChannelPosition>),
}

/// Нужно ли сообщать о причине сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureReport {
    /// Причина новая: вернуть ошибку из `decode`.
    ReportNow,
    /// О той же причине уже сообщили: отдать тишину без ошибки.
    AlreadyReported,
}

/// Владелец «о чём уже сообщили» для одного decoder-а.
#[derive(Debug, Default)]
pub(super) struct PersistentFailureNotice {
    /// Последняя сообщённая причина; `None` — программа звучит нормально.
    reported: Option<PersistentFailure>,
}

impl PersistentFailureNotice {
    /// Регистрирует причину и говорит, сообщать ли о ней.
    ///
    /// Другая причина (например, новая раскладка) сообщается заново.
    pub(super) fn register(&mut self, failure: PersistentFailure) -> FailureReport {
        if self.reported.as_ref() == Some(&failure) {
            return FailureReport::AlreadyReported;
        }
        self.reported = Some(failure);
        FailureReport::ReportNow
    }

    /// Удачный кадр: программа снова звучит, следующая причина сообщается заново.
    pub(super) fn clear_after_decoded_frame(&mut self) {
        self.reported = None;
    }
}
