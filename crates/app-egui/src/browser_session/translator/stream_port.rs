//! Шов «переходник ↔ источник»: какой писатель видит переходник и откуда берёт
//! новый поток на каждое поколение. Настоящая реализация — `pushed-media-source`,
//! тесты подставляют запись вызовов.

use std::time::Duration;

use pushed_media_source::{
    AppendOutcome, EndOfStreamReport, LaneId, PushedContainer, PushedDuration, PushedSourcePolicy,
    PushedStreamError, PushedStreamMonitor, PushedStreamOpener, PushedStreamStartError,
    PushedStreamWriter, TimestampOffset, pushed_stream,
};

/// Сторона приёма потока так, как её видит переходник. Настоящая —
/// [`PushedStreamWriter`]; тесты подставляют запись вызовов.
pub(crate) trait StreamWriterPort: Send {
    /// `addSourceBuffer`.
    fn add_lane(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError>;
    /// `appendBuffer`.
    fn append(&mut self, lane: LaneId, chunk: &[u8]) -> Result<AppendOutcome, PushedStreamError>;
    /// `abort()`.
    fn abort(&mut self, lane: LaneId) -> Result<(), PushedStreamError>;
    /// `changeType`.
    fn change_container(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError>;
    /// `timestampOffset = …`.
    fn set_timestamp_offset(
        &mut self,
        lane: LaneId,
        offset: TimestampOffset,
    ) -> Result<(), PushedStreamError>;
    /// `duration = …`.
    fn set_duration(&mut self, duration: PushedDuration);
    /// `endOfStream()`.
    fn end_of_stream(&mut self) -> Result<EndOfStreamReport, PushedStreamError>;
    /// Страница сама начала перемотку к `target` (`seek_started{page_seek}`).
    /// Звать в порядке кадров на проводе — до байтов новой точки и до команды
    /// плееру перемотаться туда же.
    fn note_page_seek(&mut self, target: Duration);
    /// Первая ошибка потока, если он мёртв (в том числе ошибка демуксера эпизода,
    /// случившаяся на другом потоке).
    fn failure(&self) -> Option<PushedStreamError>;
}

impl StreamWriterPort for PushedStreamWriter {
    fn add_lane(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        Self::add_lane(self, lane, container)
    }

    fn append(&mut self, lane: LaneId, chunk: &[u8]) -> Result<AppendOutcome, PushedStreamError> {
        Self::append(self, lane, chunk)
    }

    fn abort(&mut self, lane: LaneId) -> Result<(), PushedStreamError> {
        Self::abort(self, lane)
    }

    fn change_container(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        Self::change_container(self, lane, container)
    }

    fn set_timestamp_offset(
        &mut self,
        lane: LaneId,
        offset: TimestampOffset,
    ) -> Result<(), PushedStreamError> {
        Self::set_timestamp_offset(self, lane, offset)
    }

    fn set_duration(&mut self, duration: PushedDuration) {
        Self::set_duration(self, duration);
    }

    fn end_of_stream(&mut self) -> Result<EndOfStreamReport, PushedStreamError> {
        Self::end_of_stream(self)
    }

    fn note_page_seek(&mut self, target: Duration) {
        Self::note_page_seek(self, target);
    }

    fn failure(&self) -> Option<PushedStreamError> {
        Self::failure(self)
    }
}

/// Только что созданный поток: писатель остаётся у переходника, остальное уходит
/// главному циклу в [`GenerationHandoff`](crate::browser_session::intake_notice::GenerationHandoff).
pub(crate) struct StartedStream {
    /// Сторона приёма.
    pub(crate) writer: Box<dyn StreamWriterPort>,
    /// Сторона открытия.
    pub(crate) opener: PushedStreamOpener,
    /// Монитор буфера.
    pub(crate) monitor: PushedStreamMonitor,
}

/// Откуда переходник берёт новый поток на каждое поколение.
pub(crate) trait StreamFactory: Send {
    /// Создаёт поток.
    ///
    /// # Errors
    /// Не собрался реестр демуксеров (ошибка сборки, а не данных страницы).
    fn start_stream(&mut self) -> Result<StartedStream, PushedStreamStartError>;
}

/// Настоящая фабрика: `pushed_stream(policy)` с готовой политикой (окно буфера уже
/// подставлено из настроек `[browser_session]` вызывающим — переходник конфиг не читает).
pub(crate) struct PushedStreamFactory {
    /// Политика каждого нового потока.
    policy: PushedSourcePolicy,
}

impl PushedStreamFactory {
    /// Фабрика с политикой `policy`.
    pub(crate) const fn new(policy: PushedSourcePolicy) -> Self {
        Self { policy }
    }
}

impl StreamFactory for PushedStreamFactory {
    fn start_stream(&mut self) -> Result<StartedStream, PushedStreamStartError> {
        let (writer, opener) = pushed_stream(self.policy)?;
        let monitor = writer.monitor();
        Ok(StartedStream {
            writer: Box::new(writer),
            opener,
            monitor,
        })
    }
}
