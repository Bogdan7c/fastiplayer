//! Полоса входа — аналог одного SourceBuffer: приём кусков и выпуск эпизодов.
//!
//! Две стороны полосы живут в разных потоках:
//! - `LaneIntake` — у того, кто приносит байты (поток соединения): сканирует куски,
//!   решает, когда начинать новый эпизод, и пишет в трубу текущего эпизода;
//! - `LaneDemuxer` (модуль `lane_demuxer`) — у плеера: забирает эпизоды по порядку.
//!
//! Между ними — `LaneMailbox`: очередь запущенных эпизодов и флаги «вход окончен»
//! и «плеер закрыл поток».

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use bytes::Bytes;

use crate::container::PushedContainer;
use crate::episode::{EpisodeFactory, EpisodeHandle, EpisodeRecipe, EpisodeStartError};
use crate::error::PushedStreamError;
use crate::framing::{ContainerFramer, FramedUnit, FramingLimits};
use crate::pipe::{EpisodePipeWriter, PipeWrite};
use crate::protection::{ProtectionVerdict, scan_initialization};
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
    /// Байты приняты (целые сегменты ушли демуксеру, хвост ждёт продолжения).
    Accepted,
    /// Плеер уже закрыл поток: байты никому не нужны и выброшены.
    DiscardedConsumerClosed,
}

/// Очередь эпизодов между приёмом и плеером.
#[derive(Debug, Default)]
pub(crate) struct LaneMailbox {
    /// Состояние под замком.
    state: Mutex<MailboxState>,
}

/// Состояние почтового ящика.
#[derive(Debug, Default)]
struct MailboxState {
    /// Запущенные, но ещё не забранные плеером эпизоды.
    episodes: VecDeque<EpisodeHandle>,
    /// Новых эпизодов не будет.
    input_ended: bool,
    /// Плеер закрыл поток.
    consumer_closed: bool,
}

/// Что лежит в ящике для плеера.
#[derive(Debug)]
pub(crate) enum MailboxTake {
    /// Следующий эпизод (в куче: ручка эпизода крупная, а ящик отдаёт её редко).
    Episode(Box<EpisodeHandle>),
    /// Пока пусто, вход продолжается.
    Waiting,
    /// Пусто, и новых эпизодов не будет.
    InputEnded,
}

impl LaneMailbox {
    /// Кладёт эпизод. `false` — плеер уже закрыл поток, эпизод выброшен.
    fn post(&self, episode: EpisodeHandle) -> bool {
        let mut state = self.lock();
        if state.consumer_closed {
            return false;
        }
        state.episodes.push_back(episode);
        true
    }

    /// Сколько эпизодов ждут плеер.
    fn pending_episodes(&self) -> usize {
        self.lock().episodes.len()
    }

    /// Новых эпизодов не будет.
    fn end_input(&self) {
        self.lock().input_ended = true;
    }

    /// Закрыл ли плеер поток.
    fn consumer_closed(&self) -> bool {
        self.lock().consumer_closed
    }

    /// Следующий эпизод для плеера.
    pub(crate) fn take_next(&self) -> MailboxTake {
        let mut state = self.lock();
        match state.episodes.pop_front() {
            Some(episode) => MailboxTake::Episode(Box::new(episode)),
            None if state.input_ended => MailboxTake::InputEnded,
            None => MailboxTake::Waiting,
        }
    }

    /// Плеер закрыл поток: ждущие эпизоды выбрасываются, новые не принимаются.
    pub(crate) fn close_consumer(&self) {
        let abandoned = {
            let mut state = self.lock();
            state.consumer_closed = true;
            std::mem::take(&mut state.episodes)
        };
        // Эпизоды уничтожаются вне замка: их Drop будит потоки демуксеров.
        drop(abandoned);
    }

    /// Захватывает состояние; отравленный mutex не мешает закрытию.
    fn lock(&self) -> MutexGuard<'_, MailboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Приёмная сторона полосы.
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
    /// Текущий init-сегмент — с него начинается каждый новый эпизод.
    initialization: Option<Bytes>,
    /// Смещение времени для следующих медиасегментов.
    offset: TimestampOffset,
    /// Труба текущего эпизода; `None` — следующий медиасегмент начнёт новый эпизод.
    running_episode: Option<EpisodePipeWriter>,
    /// Ящик эпизодов для плеера.
    mailbox: std::sync::Arc<LaneMailbox>,
    /// Потолок ждущих эпизодов.
    max_pending_episodes: usize,
}

impl LaneIntake {
    /// Новая полоса без байтов.
    pub(crate) fn new(
        lane: LaneId,
        container: PushedContainer,
        framing_limits: FramingLimits,
        max_pending_episodes: usize,
        mailbox: std::sync::Arc<LaneMailbox>,
    ) -> Self {
        Self {
            lane,
            container,
            framer: ContainerFramer::new(container, framing_limits),
            framing_limits,
            initialization: None,
            offset: TimestampOffset::ZERO,
            running_episode: None,
            mailbox,
            max_pending_episodes,
        }
    }

    /// Принимает кусок байтов полосы.
    pub(crate) fn append(
        &mut self,
        chunk: &[u8],
        episodes: &EpisodeFactory,
    ) -> Result<AppendOutcome, PushedStreamError> {
        if self.mailbox.consumer_closed() {
            return Ok(AppendOutcome::DiscardedConsumerClosed);
        }
        let mut completed = Vec::new();
        self.framer
            .push(chunk, &mut completed)
            .map_err(|source| PushedStreamError::Framing {
                lane: self.lane,
                source,
            })?;
        for unit in completed {
            self.accept_unit(unit, episodes)?;
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
    pub(crate) fn change_container(&mut self, container: PushedContainer) {
        self.container = container;
        self.framer = ContainerFramer::new(container, self.framing_limits);
        self.initialization = None;
        self.finish_running_episode();
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
        self.finish_running_episode();
        Ok(())
    }

    /// Конец входа полосы: недособранное выбрасывается (возвращается объём).
    pub(crate) fn end_input(
        &mut self,
        episodes: &EpisodeFactory,
    ) -> Result<usize, PushedStreamError> {
        let mut completed = Vec::new();
        let dropped_bytes = self.framer.finish(&mut completed);
        for unit in completed {
            self.accept_unit(unit, episodes)?;
        }
        self.finish_running_episode();
        self.mailbox.end_input();
        Ok(dropped_bytes)
    }

    /// Раскладывает целый сегмент.
    fn accept_unit(
        &mut self,
        unit: FramedUnit,
        episodes: &EpisodeFactory,
    ) -> Result<(), PushedStreamError> {
        match unit {
            FramedUnit::Initialization(initialization) => {
                self.accept_initialization(initialization)
            }
            FramedUnit::Media(media) => self.accept_media(media, episodes),
        }
    }

    /// Новый init: проверка защиты; другие байты — новый эпизод с ближайшего медиасегмента.
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
        if self.initialization.as_ref() == Some(&initialization) {
            return Ok(());
        }
        self.initialization = Some(initialization);
        self.finish_running_episode();
        Ok(())
    }

    /// Медиасегмент: в текущий эпизод или первым в новый.
    fn accept_media(
        &mut self,
        media: Bytes,
        episodes: &EpisodeFactory,
    ) -> Result<(), PushedStreamError> {
        if self.container.has_initialization_segment() && self.initialization.is_none() {
            return Err(PushedStreamError::MediaBeforeInitialization { lane: self.lane });
        }
        let Some(running) = &self.running_episode else {
            return self.start_episode(media, episodes);
        };
        match episodes.push(running, media) {
            Ok(PipeWrite::Queued) => Ok(()),
            // Эпизод выброшен плеером (поток закрыт): байты никому не нужны.
            Ok(PipeWrite::EpisodeAbandoned) => {
                self.running_episode = None;
                Ok(())
            }
            Err(exceeded) => Err(PushedStreamError::IntakeLimitExceeded {
                limit: exceeded.limit,
            }),
        }
    }

    /// Запускает новый эпизод с текущим init и этим медиасегментом.
    fn start_episode(
        &mut self,
        first_media: Bytes,
        episodes: &EpisodeFactory,
    ) -> Result<(), PushedStreamError> {
        // Проверяем до запуска: у эпизода свой поток и свои очереди.
        if self.mailbox.pending_episodes() >= self.max_pending_episodes {
            return Err(PushedStreamError::TooManyPendingEpisodes {
                lane: self.lane,
                limit: self.max_pending_episodes,
            });
        }
        let recipe = EpisodeRecipe {
            container: self.container,
            initialization: self.initialization.as_ref(),
            first_media,
            offset: self.offset,
        };
        let (writer, handle) = episodes.start(recipe).map_err(|error| match error {
            EpisodeStartError::IntakeLimitExceeded(exceeded) => {
                PushedStreamError::IntakeLimitExceeded {
                    limit: exceeded.limit,
                }
            }
            EpisodeStartError::Demuxer(reason) => PushedStreamError::EpisodeStart {
                lane: self.lane,
                reason,
            },
        })?;
        if self.mailbox.post(handle) {
            self.running_episode = Some(writer);
        }
        Ok(())
    }

    /// Текущий эпизод больше не получает байтов: его демуксер дочитает и закончит.
    fn finish_running_episode(&mut self) {
        if let Some(running) = self.running_episode.take() {
            running.finish_input();
        }
    }
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod tests;
