//! Полоса входа — аналог одного SourceBuffer: приём кусков и выпуск эпизодов.
//!
//! Две стороны полосы живут в разных потоках:
//! - `LaneIntake` — у того, кто приносит байты (поток соединения): сканирует куски,
//!   размечает целые сегменты по времени и отдаёт их буферу потока
//!   (`stream_buffer`: полка, подача в эпизоды, учёт);
//! - `LaneDemuxer` (модуль `lane_demuxer`) — у плеера: забирает эпизоды по порядку.
//!
//! Между ними — `LaneMailbox` (модуль `mailbox`): очередь запущенных эпизодов и флаги «вход окончен»
//! и «плеер закрыл поток».

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;

use crate::container::PushedContainer;
use crate::error::PushedStreamError;
use crate::feed::EpisodeKey;
use crate::framing::{
    ContainerFramer, FramedUnit, FramingError, FramingLimits, SegmentTiming, SegmentTimingContext,
    SegmentTimingError,
};
use crate::intake::ChargedSegment;
pub(crate) use crate::mailbox::{LaneMailbox, MailboxTake};
use crate::protection::{ProtectionVerdict, scan_initialization};
use crate::stream_buffer::{StreamBuffer, TimedSegment};
use crate::time::TimestampOffset;

/// Номер полосы внутри потока (у браузера — индекс SourceBuffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LaneId(u16);

impl LaneId {
    /// Полоса с номером.
    #[must_use]
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    /// Номер полосы.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }
}

impl fmt::Display for LaneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "#{}", self.0)
    }
}

/// Что произошло с принятым куском.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// Байты приняты (целые сегменты легли на полку или ушли демуксеру, хвост
    /// ждёт продолжения).
    Accepted,
    /// Плеер уже закрыл поток: байты никому не нужны и выброшены.
    DiscardedConsumerClosed,
}

/// Текущий init полосы и разметка времени, прочитанная из него.
#[derive(Debug)]
struct CurrentInitialization {
    /// Init (зачтён в учёт, пока его держит полоса или сегменты полки).
    segment: ChargedSegment,
    /// Как читать время медиасегментов этого init.
    timing: SegmentTimingContext,
}

/// Приёмная сторона полосы: куски → целые сегменты → буфер потока.
#[derive(Debug)]
pub(crate) struct LaneIntake {
    /// Номер полосы.
    lane: LaneId,
    /// Контейнер (меняется `changeType`).
    container: PushedContainer,
    /// Сканер сегментов.
    framer: ContainerFramer,
    /// Пределы сканера (нужны при смене контейнера).
    framing_limits: FramingLimits,
    /// Текущий init-сегмент — к нему относятся следующие медиасегменты.
    initialization: Option<CurrentInitialization>,
    /// Смещение времени для следующих медиасегментов.
    offset: TimestampOffset,
    /// Ящик эпизодов: только чтобы узнать, не закрыл ли плеер поток.
    mailbox: Arc<LaneMailbox>,
    /// Буфер потока: полки, подача, учёт.
    buffer: Arc<StreamBuffer>,
}

impl LaneIntake {
    /// Новая полоса без байтов (полоса уже заведена в буфере).
    pub(crate) fn new(
        lane: LaneId,
        container: PushedContainer,
        framing_limits: FramingLimits,
        mailbox: Arc<LaneMailbox>,
        buffer: Arc<StreamBuffer>,
    ) -> Self {
        Self {
            lane,
            container,
            framer: ContainerFramer::new(container, framing_limits),
            framing_limits,
            initialization: None,
            offset: TimestampOffset::ZERO,
            mailbox,
            buffer,
        }
    }

    /// Принимает кусок байтов полосы.
    pub(crate) fn append(&mut self, chunk: &[u8]) -> Result<AppendOutcome, PushedStreamError> {
        if self.mailbox.consumer_closed() {
            // Плееру ничего не нужно: полка и текущий init отпускаются сразу.
            self.initialization = None;
            self.buffer.discard_lane(self.lane)?;
            return Ok(AppendOutcome::DiscardedConsumerClosed);
        }
        let mut completed = Vec::new();
        self.framer
            .push(chunk, &mut completed)
            .map_err(|source| self.framing_error(source))?;
        for unit in completed {
            self.accept_unit(unit)?;
        }
        Ok(AppendOutcome::Accepted)
    }

    /// Байты недособранного сегмента, которые держит сканер.
    pub(crate) fn partial_segment_bytes(&self) -> usize {
        self.framer.held_bytes()
    }

    /// `abort()`: недособранный сегмент выбрасывается; эпизод продолжается.
    pub(crate) fn abort(&mut self) {
        self.framer.reset();
    }

    /// `changeType()`: другой контейнер или кодек. Следующим обязан прийти новый init.
    pub(crate) fn change_container(
        &mut self,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        self.container = container;
        self.framer = ContainerFramer::new(container, self.framing_limits);
        self.initialization = None;
        self.buffer.change_lane_container(self.lane, container)
    }

    /// Новое смещение времени действует с ближайшего медиасегмента — в новом эпизоде.
    pub(crate) fn set_timestamp_offset(
        &mut self,
        offset: TimestampOffset,
    ) -> Result<(), PushedStreamError> {
        if offset == self.offset {
            return Ok(());
        }
        if !self.container.has_initialization_segment() {
            // У TS нет init-сегмента, с которого начать новый эпизод: PAT/PMT
            // остались в прошлом. Источник TS-сегментов — сам экстеншен (решение 83),
            // и время он берёт из потока.
            return Err(PushedStreamError::TimestampOffsetUnsupported { lane: self.lane });
        }
        self.offset = offset;
        self.buffer.finish_running_episode(self.lane)
    }

    /// Конец входа полосы: недособранное выбрасывается (возвращается объём).
    pub(crate) fn end_input(&mut self) -> Result<usize, PushedStreamError> {
        let mut completed = Vec::new();
        let dropped_bytes = self.framer.finish(&mut completed);
        for unit in completed {
            self.accept_unit(unit)?;
        }
        self.buffer.end_lane_input(self.lane)?;
        Ok(dropped_bytes)
    }

    /// Вход снова открыт (байты после `endOfStream`).
    pub(crate) fn reopen_input(&mut self) -> Result<(), PushedStreamError> {
        self.buffer.reopen_lane_input(self.lane)
    }

    /// Раскладывает целый сегмент.
    fn accept_unit(&mut self, unit: FramedUnit) -> Result<(), PushedStreamError> {
        match unit {
            FramedUnit::Initialization(initialization) => {
                self.accept_initialization(initialization)
            }
            FramedUnit::Media(media) => self.accept_media(media),
        }
    }

    /// Новый init: проверка защиты и разметки времени; другие байты — новый эпизод.
    fn accept_initialization(&mut self, initialization: Bytes) -> Result<(), PushedStreamError> {
        match scan_initialization(self.container, &initialization) {
            Ok(ProtectionVerdict::Clear) => {}
            Ok(ProtectionVerdict::Protected) => {
                return Err(PushedStreamError::ProtectedContent { lane: self.lane });
            }
            Err(_) => {
                return Err(PushedStreamError::InconsistentInitialization { lane: self.lane });
            }
        }
        // Плееры страниц часто повторяют тот же init (перед перемоткой, на всякий
        // случай). Тот же init не повод пересоздавать декодер.
        if self
            .initialization
            .as_ref()
            .is_some_and(|current| current.segment.bytes() == &initialization)
        {
            return Ok(());
        }
        let timing = SegmentTimingContext::from_initialization(self.container, &initialization)
            .map_err(|error| self.framing_error(FramingError::Timing(error)))?;
        let segment = self.buffer.charge(initialization)?;
        self.initialization = Some(CurrentInitialization { segment, timing });
        self.buffer.finish_running_episode(self.lane)
    }

    /// Медиасегмент: размечается по времени и уходит в буфер потока.
    fn accept_media(&mut self, media: Bytes) -> Result<(), PushedStreamError> {
        let current = match &self.initialization {
            Some(current) => Some(current),
            None if self.container.has_initialization_segment() => {
                return Err(PushedStreamError::MediaBeforeInitialization { lane: self.lane });
            }
            None => None,
        };
        let timing = match current {
            Some(current) => current
                .timing
                .media_segment_timing(&media)
                .map_err(|error| self.framing_error(FramingError::Timing(error)))?,
            None => SegmentTiming::Untimed,
        };
        let key = EpisodeKey::new(
            self.container,
            current.map(|current| current.segment.clone()),
            self.offset,
        );
        match timing {
            SegmentTiming::Timed(raw_timing) => {
                let timing = raw_timing.shifted_by(self.offset).ok_or_else(|| {
                    self.framing_error(FramingError::Timing(SegmentTimingError::TimeOutOfRange))
                })?;
                self.buffer
                    .accept_timed(self.lane, TimedSegment { timing, media, key })
            }
            SegmentTiming::Untimed => self.buffer.accept_untimed(self.lane, media, &key),
        }
    }

    /// Ошибка разбора байтов этой полосы.
    const fn framing_error(&self, source: FramingError) -> PushedStreamError {
        PushedStreamError::Framing {
            lane: self.lane,
            source,
        }
    }
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod tests;
