//! Ожидание сети посреди HLS-эфира (браузерный мост 16d3, решение 334).
//!
//! После пробуждения машины Wi-Fi поднимается секундами, а обрыв сети посреди эфира
//! длится и дольше. Раньше эфир падал на первом же сбое плейлиста или сегмента: у
//! транспорта всего три быстрые попытки (~0,3 с). Теперь временный сетевой сбой
//! повторяется по общему правилу плеера — паузы 0,5 → 8 с, общий бюджет
//! `network.reconnect_wait_ms` (по умолчанию 60 с), — а потом уже честная ошибка.
//!
//! Модуль владеет только решением «ждать ли ещё и когда повторить» и учётом того, что
//! уже отдано плееру. Сам повтор делают владельцы ресурса: цикл обновления плейлиста
//! (`refresh.rs`) и демуксер компонента (`demux.rs`).

use std::time::Instant;

use source_core::{
    HttpReconnectDecision, HttpReconnectPolicy, HttpReconnectSchedule, HttpRetryAfter,
};

use super::HlsLiveSegmentIdentity;
use super::open::HlsLiveOpenError;

/// Решение после очередного сетевого сбоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HlsLiveOutageDecision {
    /// Повторить не раньше этого момента.
    RetryAt(Instant),
    /// Бюджет ожидания исчерпан: сбой становится роковым.
    GiveUp,
}

/// Серия сетевых сбоев одного места: от первого сбоя до первого успеха.
#[derive(Debug, Clone, Copy)]
pub(super) struct HlsLiveNetworkOutage {
    policy: HttpReconnectPolicy,
    schedule: HttpReconnectSchedule,
}

impl HlsLiveNetworkOutage {
    /// Пустая серия: сбоев ещё не было.
    pub(super) const fn new(policy: HttpReconnectPolicy) -> Self {
        Self {
            policy,
            schedule: HttpReconnectSchedule::new(policy),
        }
    }

    /// Учитывает сбой и решает, когда повторить.
    pub(super) fn record_failure(
        &mut self,
        now: Instant,
        retry_after: HttpRetryAfter,
    ) -> HlsLiveOutageDecision {
        match self.schedule.record_failure(now, retry_after) {
            HttpReconnectDecision::RetryAfter(delay) => now.checked_add(delay).map_or(
                HlsLiveOutageDecision::GiveUp,
                HlsLiveOutageDecision::RetryAt,
            ),
            HttpReconnectDecision::GiveUp => HlsLiveOutageDecision::GiveUp,
        }
    }

    /// Ресурс снова скачался: следующая серия сбоев считает бюджет заново.
    pub(super) fn record_recovery(&mut self) {
        if self.schedule.failed_attempts() > 0 {
            log::info!(
                "HLS live network recovered after {} failed attempts",
                self.schedule.failed_attempts()
            );
        }
        self.schedule = HttpReconnectSchedule::new(self.policy);
    }
}

/// Временный ли это сетевой сбой плейлиста и что сервер просил подождать.
///
/// Протухание ссылки (401/403/404/410) сюда не попадает: у него своя замена ссылки.
pub(super) fn transient_manifest_failure(error: &HlsLiveOpenError) -> Option<HttpRetryAfter> {
    let HlsLiveOpenError::Transport(error) = error else {
        return None;
    };
    error.is_retryable().then(|| error.http_retry_after())
}

/// Отложенный повтор сегмента после сетевого сбоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HlsLivePendingSegmentRetry {
    /// Какой сегмент повторить.
    pub identity: HlsLiveSegmentIdentity,
    /// Сколько пакетов этого сегмента плеер уже получил: их нужно пропустить.
    pub packets_already_delivered: usize,
    /// Раньше этого момента не повторять.
    pub not_before: Instant,
}

/// Состояние отложенного повтора сегмента на текущий момент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HlsLiveSegmentRetryState {
    /// Повтора нет: читаем как обычно.
    NotPending,
    /// Повтор есть, но ещё рано.
    Waiting,
    /// Пора повторить этот сегмент.
    Due(HlsLivePendingSegmentRetry),
}

/// Сетевые сбои сегментов одного компонента и учёт уже отданных пакетов.
///
/// Тот же сегмент даёт те же пакеты в том же порядке, поэтому после повтора достаточно
/// пропустить столько пакетов, сколько плеер уже получил: ни дублей, ни дыр.
#[derive(Debug, Clone, Copy)]
pub(super) struct HlsLiveSegmentNetworkRetry {
    outage: HlsLiveNetworkOutage,
    pending: Option<HlsLivePendingSegmentRetry>,
    /// Пакетов прочитано из текущего открытого сегмента (включая пропущенные).
    packets_read_in_segment: usize,
    /// Сколько ещё пакетов текущего сегмента выбросить как уже отданные.
    packets_to_skip: usize,
}

impl HlsLiveSegmentNetworkRetry {
    pub(super) const fn new(policy: HttpReconnectPolicy) -> Self {
        Self {
            outage: HlsLiveNetworkOutage::new(policy),
            pending: None,
            packets_read_in_segment: 0,
            packets_to_skip: 0,
        }
    }

    /// Открыт сегмент: счёт пакетов с нуля, первые `packets_to_skip` уже отданы плееру.
    pub(super) fn note_segment_opened(&mut self, packets_to_skip: usize) {
        self.packets_read_in_segment = 0;
        self.packets_to_skip = packets_to_skip;
    }

    /// Учитывает прочитанный пакет; `true` — плеер его уже получил, отдавать не нужно.
    ///
    /// Первый новый пакет после сбоя доказывает, что сеть вернулась: бюджет сброшен.
    pub(super) fn is_already_delivered_packet(&mut self) -> bool {
        self.packets_read_in_segment = self.packets_read_in_segment.saturating_add(1);
        if self.packets_to_skip > 0 {
            self.packets_to_skip -= 1;
            return true;
        }
        self.outage.record_recovery();
        false
    }

    /// Сколько пакетов уже прочитано из открытого сегмента (включая пропущенные).
    pub(super) const fn packets_read_in_segment(&self) -> usize {
        self.packets_read_in_segment
    }

    /// Откладывает повтор сегмента; `false` — бюджет исчерпан, сбой роковой.
    ///
    /// `packets_already_delivered` — сколько пакетов этого сегмента плеер уже получил:
    /// при сбое посреди чтения это всё прочитанное, при сбое открытия — сколько было
    /// пропущено к моменту открытия (ноль у нового сегмента).
    pub(super) fn defer(
        &mut self,
        identity: HlsLiveSegmentIdentity,
        packets_already_delivered: usize,
        retry_after: HttpRetryAfter,
        now: Instant,
    ) -> bool {
        let HlsLiveOutageDecision::RetryAt(not_before) =
            self.outage.record_failure(now, retry_after)
        else {
            self.pending = None;
            return false;
        };
        self.pending = Some(HlsLivePendingSegmentRetry {
            identity,
            packets_already_delivered,
            not_before,
        });
        true
    }

    /// Возвращает снятый повтор обратно: продолжать пока не с чего (окно ушло, а следующего
    /// сегмента ещё нет). Это не новый сбой сети, бюджет не тратится; повтор будет при
    /// следующем чтении.
    pub(super) fn keep_waiting_for_segment(&mut self, pending: HlsLivePendingSegmentRetry) {
        self.pending = Some(pending);
    }

    /// Что с отложенным повтором сейчас; `Due` снимает его (повтор начинается).
    pub(super) fn take_due(&mut self, now: Instant) -> HlsLiveSegmentRetryState {
        match self.pending {
            None => HlsLiveSegmentRetryState::NotPending,
            Some(pending) if now < pending.not_before => HlsLiveSegmentRetryState::Waiting,
            Some(pending) => {
                self.pending = None;
                HlsLiveSegmentRetryState::Due(pending)
            }
        }
    }
}

/// Новый сегмент: плеер из него ещё ничего не получил.
pub(super) const NO_PACKETS_DELIVERED: usize = 0;

#[cfg(test)]
#[path = "network_outage_tests.rs"]
mod tests;
