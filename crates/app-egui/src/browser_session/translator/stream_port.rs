//! Шов «переходник ↔ источник»: какой писатель видит переходник и откуда берёт
//! новый поток на каждое поколение. Настоящая реализация — `pushed-media-source`,
//! тесты подставляют запись вызовов.

use std::time::Duration;

use tracing::warn;

use pushed_media_source::{
    AppendOutcome, EndOfStreamReport, LaneId, PageRemovalOutcome, PageRemovedRange,
    PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamError, PushedStreamMonitor,
    PushedStreamOpener, PushedStreamStartError, PushedStreamWriter, TimestampOffset, pushed_stream,
};

use crate::browser_session::buffer_policy::LiveBufferWindow;

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
    /// `remove(start, end)`: страница стёрла у себя кусок времени полосы. Звать в
    /// порядке кадров на проводе (стирание и повтор того же времени идут подряд).
    fn note_page_removed(
        &mut self,
        lane: LaneId,
        range: PageRemovedRange,
    ) -> Result<PageRemovalOutcome, PushedStreamError>;
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

    fn note_page_removed(
        &mut self,
        lane: LaneId,
        range: PageRemovedRange,
    ) -> Result<PageRemovalOutcome, PushedStreamError> {
        Self::note_page_removed(self, lane, range)
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
///
/// Окно буфера можно сделать «живым» ([`Self::following`]): тогда каждый НОВЫЙ поток
/// стартует с окном, которое пользователь выставил в окне настроек к этому моменту.
/// Идущий поток свою копию окна не меняет (решения 96/124).
pub(crate) struct PushedStreamFactory {
    /// Политика каждого нового потока (окно буфера — по умолчанию).
    policy: PushedSourcePolicy,
    /// Живое окно буфера: если задано, перекрывает окно `policy` при старте потока.
    live_window: Option<LiveBufferWindow>,
    /// С каким окном стартовал последний поток (проверка тестов: «текущему не меняли»).
    #[cfg(test)]
    last_started_window: Option<pushed_media_source::BufferWindow>,
}

impl PushedStreamFactory {
    /// Фабрика с политикой `policy` и неизменным окном буфера (безоконные тесты).
    #[cfg(test)]
    pub(crate) const fn new(policy: PushedSourcePolicy) -> Self {
        Self {
            policy,
            live_window: None,
            #[cfg(test)]
            last_started_window: None,
        }
    }

    /// Фабрика, берущая окно буфера нового потока у `live_window` в момент старта.
    pub(crate) const fn following(
        policy: PushedSourcePolicy,
        live_window: LiveBufferWindow,
    ) -> Self {
        Self {
            policy,
            live_window: Some(live_window),
            #[cfg(test)]
            last_started_window: None,
        }
    }

    /// Политика для потока, который стартует сейчас.
    fn policy_for_new_stream(&self) -> PushedSourcePolicy {
        let Some(live_window) = &self.live_window else {
            return self.policy;
        };
        match self
            .policy
            .with_buffer_window(live_window.window_for_next_stream())
        {
            Ok(policy) => policy,
            // Окно из config-а уже проверено (запас вперёд > 0); сюда попасть нельзя, но
            // тихо проглотить нельзя тоже: играем с окном, с которым процесс стартовал.
            Err(error) => {
                warn!(%error, "The live buffer window was refused; using the startup window");
                self.policy
            }
        }
    }
}

impl StreamFactory for PushedStreamFactory {
    fn start_stream(&mut self) -> Result<StartedStream, PushedStreamStartError> {
        let policy = self.policy_for_new_stream();
        #[cfg(test)]
        {
            self.last_started_window = Some(policy.buffer_window());
        }
        let (writer, opener) = pushed_stream(policy)?;
        let monitor = writer.monitor();
        Ok(StartedStream {
            writer: Box::new(writer),
            opener,
            monitor,
        })
    }
}

#[cfg(test)]
mod live_window_tests {
    use std::time::Duration;

    use fastiplayer_config::AppConfig;

    use super::*;
    use crate::settings_runtime::BrowserBufferWindowOwner;

    fn config_window(
        ahead_s: u16,
        behind_s: u16,
    ) -> fastiplayer_config::BrowserSessionBufferWindow {
        let mut config = AppConfig::default();
        config.browser_session.buffer_ahead_s = ahead_s;
        config.browser_session.buffer_behind_s = behind_s;
        config.browser_session.buffer_window()
    }

    /// Первое видео стартует со старым окном, после усыновления нового окна следующее
    /// видео — с новым; окно уже идущего потока (его монитор жив) остаётся прежним.
    #[test]
    fn the_next_stream_starts_with_the_adopted_window_and_the_running_one_keeps_its_own() {
        let live = LiveBufferWindow::new(config_window(120, 30));
        let mut factory = PushedStreamFactory::following(
            PushedSourcePolicy::recommended().expect("policy"),
            live.clone(),
        );

        let running = factory.start_stream().expect("first stream");
        let first_window = factory.last_started_window.expect("first window");
        live.adopt_buffer_window_for_next_stream(config_window(45, 10));
        let second = factory.start_stream().expect("second stream");
        let second_window = factory.last_started_window.expect("second window");

        assert_eq!(first_window.ahead, Duration::from_secs(120));
        assert_eq!(second_window.ahead, Duration::from_secs(45));
        assert_eq!(second_window.behind, Duration::from_secs(10));
        // Идущий поток жив; окно, с которым он стартовал, зафиксировано в `first_window`
        // и усыновление нового окна его не меняет.
        assert_eq!(first_window.ahead, Duration::from_secs(120));
        drop((running, second));
    }

    /// Фабрика без живого окна (безоконный процесс, тесты) всегда берёт окно политики.
    #[test]
    fn a_factory_without_a_live_window_keeps_the_policy_window() {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let mut factory = PushedStreamFactory::new(policy);

        factory.start_stream().expect("stream");

        assert_eq!(factory.last_started_window, Some(policy.buffer_window()));
    }
}
