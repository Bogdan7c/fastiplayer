//! Поток страницы → плеер → ответы странице: медиа-часть главного цикла (03b, C3).
//!
//! ```text
//!  поток приёма ── IntakeNotice ──► SessionMedia ── load/play/pause/seek ──► MediaPlayback
//!                                       │  ▲                                    │
//!                                       │  └──────── pump(now): позиция, состояние ┘
//!                                       ├─ монитор потока: позиция → запас, «полон», просьба seek
//!                                       └─ PageOutbound: clock / buffer_state / seek / stream_rejected
//! ```
//!
//! Чем владеет модуль (и больше ничем):
//! - текущим экземпляром сеанса — уведомления чужих экземпляров не исполняются
//!   (кроме просьбы закрыть: она адресна сама, см. `SessionControl::close_session`);
//! - не больше чем ОДНОЙ ждущей стороной открытия: новее загруженного поколения и
//!   новее прежней ждущей (старшее вытесняет младшее, младшее после старшего
//!   выбрасывается) — так смена `MediaSource` страницы никогда не возвращает плеер
//!   к старому видео;
//! - загруженным поколением: его монитор получает позицию плеера и отдаёт запас,
//!   «полон» и просьбы перемотать страницу;
//! - правилом «чья позиция»: плеер ставит новое медиа асинхронно, и до установки
//!   наблюдение описывает ПРЕЖНЕЕ медиа. Позиция идёт в монитор поколения и в часы
//!   страницы только когда наблюдение показывает медиа загрузки этого поколения
//!   ([`PlaybackObservation::shows`]); пока поколение «ставится», монитор позиции не
//!   получает (источник считает от начала первого поданного сегмента), а часы
//!   говорят `waiting` в позиции 0 ([`PlaybackObservation::while_media_installs`]);
//! - намерением страницы «играть/пауза» (автозапуск — играть) и её скоростью:
//!   загрузка начинается так, как хочет страница ([`LoadStart`]); после перемотки
//!   плеер приводится к «играть/пауза» (перемотка после `Ended` в плеере кончается
//!   паузой — поэтому `play()` идёт следом, если страница играет; ядро принимает
//!   `play` и во время перемотки — проверено тестом настоящего плеера);
//! - правилом «согласовать после установки»: плеер ставит медиа асинхронно, а
//!   установка сбрасывает скорость на 1× и берёт «играть/пауза» из загрузки. Команды,
//!   отданные страницей, пока медиа ставилось, достались прежнему медиа. Поэтому,
//!   как только наблюдение впервые показывает медиа поколения, плеер один раз
//!   приводится к скорости и «играть/пауза» страницы;
//! - правилом «перемотка страницы — самому новому поколению»: пока новое поколение
//!   открывается, перемотка страницы относится к нему (источник уже знает о ней), а
//!   не к видео прежнего поколения в плеере — плееру её не передаём, новое поколение
//!   начнётся там, где начинаются его байты.
//!
//! Ограничение (записано в отчёте 03b): длительность и «можно ли перематывать»
//! плеер запоминает при сборке `PreparedMedia`, то есть в момент готовности потока.
//! YouTube присылает `duration_changed` раньше `source_buffer_added` (проверено
//! вживую), поэтому медиа собирается сразу после `Ready`; страница, которая
//! объявит длительность позже первых байтов, получит плеер без длительности.

use std::time::{Duration, Instant};

use browser_bridge_protocol::session::player::StreamRejection;
use browser_bridge_protocol::{
    PlaybackRate as PagePlaybackRate, SessionCloseReason, StreamGeneration,
};
use browser_bridge_session::{CloseOutcome, SessionControl, SessionRef};
use player_core::{
    PlaybackRate as PlayerPlaybackRate, PlaybackState, PreparedMedia,
    PreparedMediaTimelineModeError,
};
use pushed_media_source::{
    OpenProgress, OpenedPushedStream, PushedStreamMonitor, PushedStreamOpener,
};
use tracing::{debug, error, info, warn};

use super::intake_notice::{GenerationHandoff, IntakeNotice, PageIntent};
use super::outbound::{PENDING_RETRY_INTERVAL, PageOutbound};
use super::playback::{
    BrowserPlayback, BrowserPlaybackCommandError, BrowserPlaybackFault, LoadStart, MediaLoadTicket,
    PlaybackObservation,
};
use super::rejection::{FailureResponse, response_to_stream_failure};

/// Метка источника для плеера. Нейтральная: ни заголовка, ни домена страницы —
/// метка может попасть в журнал плеера (инвариант 7).
pub(crate) const BROWSER_STREAM_LABEL: &str = "browser stream";

// ---- швы -----------------------------------------------------------------------

/// Плеер так, как его видит медиа-часть. Настоящий — [`BrowserPlayback`].
pub(crate) trait MediaPlayback {
    /// Загрузить новое медиа: играть сразу или встать на паузу — по `start`.
    /// Квитанция связывает загрузку с наблюдениями, когда плеер её установит.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError>;
    /// Играть.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError>;
    /// Пауза.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError>;
    /// Скорость.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn set_rate(&mut self, rate: PlayerPlaybackRate) -> Result<(), BrowserPlaybackCommandError>;
    /// Точная перемотка.
    ///
    /// # Errors
    /// Команда не дошла до плеера.
    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError>;
    /// Шаг плеера: события, кадры, звук, журнал.
    fn pump(&mut self, now: Instant) -> PlaybackObservation;
    /// Итоговая строка журнала и остановка плеера.
    fn finish(self: Box<Self>);
}

impl MediaPlayback for BrowserPlayback {
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError> {
        Self::load(self, prepared, start)
    }

    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        Self::play(self)
    }

    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        Self::pause(self)
    }

    fn set_rate(&mut self, rate: PlayerPlaybackRate) -> Result<(), BrowserPlaybackCommandError> {
        Self::set_rate(self, rate)
    }

    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError> {
        Self::seek(self, target)
    }

    fn pump(&mut self, now: Instant) -> PlaybackObservation {
        Self::pump(self, now)
    }

    fn finish(self: Box<Self>) {
        let _summary = Self::finish(*self);
    }
}

/// Кто закрывает сеанс по просьбе потока приёма. Настоящий — `SessionControl`.
pub(crate) trait SessionCloser {
    /// Закрывает ровно этот экземпляр (не тот, что успел его заменить).
    fn close_session(&self, session: &SessionRef, reason: SessionCloseReason) -> CloseOutcome;
}

impl SessionCloser for SessionControl {
    fn close_session(&self, session: &SessionRef, reason: SessionCloseReason) -> CloseOutcome {
        Self::close_session(self, session, reason)
    }
}

// ---- состояние -----------------------------------------------------------------

/// Поколение, которое ещё открывается.
struct PendingGeneration {
    /// Сеанс.
    session: SessionRef,
    /// Номер.
    generation: StreamGeneration,
    /// Сторона открытия.
    opener: PushedStreamOpener,
    /// Монитор (переходит к загруженному поколению).
    monitor: PushedStreamMonitor,
    /// Когда спросить снова (`None` — при первой возможности).
    retry_at: Option<Instant>,
}

/// Поколение, загруженное в плеер.
struct LoadedGeneration {
    /// Номер.
    generation: StreamGeneration,
    /// Квитанция загрузки этого поколения в плеер.
    load_ticket: MediaLoadTicket,
    /// Монитор буфера потока.
    monitor: PushedStreamMonitor,
    /// Об отказе плеера уже сообщено (один раз на поколение).
    fault_reported: bool,
    /// После установки плеер уже приведён к скорости и «играть/пауза» страницы.
    page_state_reconciled: bool,
}

impl LoadedGeneration {
    /// Наблюдение описывает медиа этого поколения (плеер его уже поставил).
    fn is_shown_by(&self, observation: &PlaybackObservation) -> bool {
        observation.shows(self.load_ticket)
    }
}

/// Медиа-часть главного цикла.
pub(crate) struct SessionMedia {
    /// Плеер.
    playback: Box<dyn MediaPlayback>,
    /// Закрытие сеанса по просьбе потока приёма.
    closer: Box<dyn SessionCloser>,
    /// Исходящие странице.
    outbound: PageOutbound,
    /// Текущий экземпляр сеанса.
    current_session: Option<SessionRef>,
    /// Поколение, которое открывается.
    pending: Option<PendingGeneration>,
    /// Поколение в плеере.
    loaded: Option<LoadedGeneration>,
    /// Страница хочет играть (автозапуск — да).
    page_wants_play: bool,
    /// Скорость, которую выбрала страница (плеер сбрасывает её при каждой загрузке).
    page_rate: PlayerPlaybackRate,
    /// Поток приёма сообщил о новых байтах: открыть стоит попробовать сразу.
    bytes_arrived: bool,
}

impl SessionMedia {
    /// Медиа-часть без сеанса.
    pub(crate) fn new(
        playback: Box<dyn MediaPlayback>,
        closer: Box<dyn SessionCloser>,
        outbound: PageOutbound,
    ) -> Self {
        Self {
            playback,
            closer,
            outbound,
            current_session: None,
            pending: None,
            loaded: None,
            page_wants_play: true,
            page_rate: PlayerPlaybackRate::NORMAL,
            bytes_arrived: false,
        }
    }

    /// Открыт сеанс. Повтор того же экземпляра ничего не меняет; новый экземпляр
    /// (другая вкладка или новое открытие) — новая страница: ждущее поколение
    /// выбрасывается, прежнее видео встаёт на паузу, исходящие старой странице
    /// забываются.
    pub(crate) fn on_session_opened(&mut self, session: &SessionRef) {
        if self.current_session.as_ref() == Some(session) {
            return;
        }
        self.current_session = Some(session.clone());
        self.pending = None;
        self.bytes_arrived = false;
        self.page_wants_play = true;
        self.page_rate = PlayerPlaybackRate::NORMAL;
        self.outbound.reset_for_new_session();
        if self.loaded.take().is_some() {
            self.command("pause for a new session", |playback| playback.pause());
        }
    }

    /// Сеанс закончился: отложенные сообщения его странице больше некому слать.
    ///
    /// Конец чужого (уже заменённого) экземпляра ничего не трогает.
    pub(crate) fn on_session_ended(&mut self, session: &SessionRef) {
        if self.is_current(session) {
            self.outbound.forget_pending_of_ended_session();
        }
    }

    /// Уведомления потока приёма в порядке выдачи.
    pub(crate) fn on_intake(
        &mut self,
        notices: Vec<IntakeNotice>,
        bytes_appended: Option<SessionRef>,
    ) {
        if bytes_appended.is_some_and(|session| self.is_current(&session)) {
            self.bytes_arrived = true;
        }
        for notice in notices {
            self.apply_notice(notice);
        }
    }

    /// Один проход: открыть ждущее, шаг плеера, ответы странице.
    ///
    /// Возвращает, когда проснуться без внешнего повода (повтор открытия, тик часов,
    /// повтор отложенных сообщений); `None` — ждать только пробуждений.
    pub(crate) fn drive(&mut self, now: Instant) -> Option<Instant> {
        self.poll_pending(now);
        let observation = self.playback.pump(now);
        self.report_to_page(&observation, now);
        self.outbound.flush(now);
        self.next_wake_at(now)
    }

    /// Конец процесса: итог плеера в журнал и остановка плеера.
    pub(crate) fn finish(self) {
        self.playback.finish();
    }

    // ---- уведомления ---------------------------------------------------------

    fn is_current(&self, session: &SessionRef) -> bool {
        self.current_session.as_ref() == Some(session)
    }

    fn apply_notice(&mut self, notice: IntakeNotice) {
        // Просьба закрыть адресна сама: `close_session` не тронет чужой экземпляр.
        if let IntakeNotice::CloseSession { session, reason } = notice {
            self.close(&session, reason);
            return;
        }
        if !self.is_current(notice.session()) {
            debug!("Stream notice of a session instance that is no longer current dropped");
            return;
        }
        match notice {
            IntakeNotice::NewGeneration(handoff) => self.adopt_generation(*handoff),
            IntakeNotice::BytesAppended { .. } => self.bytes_arrived = true,
            IntakeNotice::PageIntent { intent, .. } => self.follow_page(intent),
            IntakeNotice::PlayerSeekAcknowledged {
                seek_id, target, ..
            } => {
                debug!(
                    seek_id = seek_id.0,
                    target_us = target.as_micros(),
                    "The page started the seek the player asked for"
                );
            }
            IntakeNotice::Reject {
                generation, reason, ..
            } => self.reject(generation, reason),
            // Исполнена выше, до проверки «текущий ли экземпляр».
            IntakeNotice::CloseSession { .. } => {}
        }
    }

    /// Новое поколение становится ждущим, если оно новее загруженного и ждущего.
    fn adopt_generation(&mut self, handoff: GenerationHandoff) {
        let newest_known = self
            .pending
            .as_ref()
            .map(|pending| pending.generation)
            .into_iter()
            .chain(self.loaded.as_ref().map(|loaded| loaded.generation))
            .max();
        if newest_known.is_some_and(|newest| handoff.generation <= newest) {
            debug!(
                generation = handoff.generation.get(),
                "Stream generation older than the one already known dropped"
            );
            return;
        }
        if let Some(superseded) = &self.pending {
            debug!(
                generation = superseded.generation.get(),
                "Pending stream generation superseded before it opened"
            );
        }
        self.pending = Some(PendingGeneration {
            session: handoff.session,
            generation: handoff.generation,
            opener: handoff.opener,
            monitor: handoff.monitor,
            retry_at: None,
        });
    }

    /// Намерение страницы → плеер.
    fn follow_page(&mut self, intent: PageIntent) {
        match intent {
            PageIntent::FollowPageSeek(target) => {
                // Источник уже знает о перемотке (переходник сообщил ему в порядке
                // кадров), поэтому просьбы «перемотай страницу» к этой цели не будет.
                if self.pending.is_some() {
                    // Переходник сообщил перемотку источнику САМОГО НОВОГО поколения —
                    // того, что ещё открывается; видео прежнего поколения она не
                    // касается. Новое поколение начнётся там, где начинаются его байты.
                    debug!(
                        "Page seek belongs to the generation that is still opening; the loaded video is not moved"
                    );
                    return;
                }
                if self.loaded.is_none() {
                    debug!(
                        "Page seek before the stream opened; the player starts where the bytes start"
                    );
                    return;
                }
                let target = Duration::from_micros(target.as_micros());
                self.command("seek", |playback| playback.seek(target));
                self.apply_page_play_state();
            }
            PageIntent::Play => {
                self.page_wants_play = true;
                self.apply_page_play_state();
            }
            PageIntent::Pause => {
                self.page_wants_play = false;
                self.apply_page_play_state();
            }
            PageIntent::SetRate(rate) => {
                let rate = player_rate(rate);
                self.page_rate = rate;
                self.command("set rate", |playback| playback.set_rate(rate));
            }
        }
    }

    /// Плеер следует за «играть/пауза» страницы (только если есть что играть).
    fn apply_page_play_state(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        if self.page_wants_play {
            self.command("play", |playback| playback.play());
        } else {
            self.command("pause", |playback| playback.pause());
        }
    }

    /// Отказ от поколения: странице `stream_rejected`; ждущее открытие этого
    /// поколения больше не нужно.
    fn reject(&mut self, generation: StreamGeneration, reason: StreamRejection) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            self.pending = None;
        }
        if self.outbound.reject_generation(generation, reason) {
            info!(
                generation = generation.get(),
                ?reason,
                "Stream generation rejected"
            );
        }
    }

    fn close(&self, session: &SessionRef, reason: SessionCloseReason) {
        let outcome = self.closer.close_session(session, reason);
        info!(
            ?reason,
            ?outcome,
            "Browser session close requested by the stream intake"
        );
    }

    // ---- открытие --------------------------------------------------------------

    /// Спрашивает ждущее поколение, если пора (срок подсказки или пришли байты).
    fn poll_pending(&mut self, now: Instant) {
        let bytes_arrived = std::mem::take(&mut self.bytes_arrived);
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let due = bytes_arrived || pending.retry_at.is_none_or(|retry_at| retry_at <= now);
        if !due {
            return;
        }
        match pending.opener.poll_open() {
            Ok(OpenProgress::Waiting(hint)) => {
                pending.retry_at = Some(now + hint.retry_after());
            }
            Ok(OpenProgress::Ready(opened)) => {
                if let Some(pending) = self.pending.take() {
                    self.load(pending, opened);
                }
            }
            Err(open_error) => {
                if let Some(pending) = self.pending.take() {
                    // Только класс ошибки: текст несёт подробности байтов страницы.
                    warn!(
                        failure_class = ?open_error.failure_class(),
                        generation = pending.generation.get(),
                        "Stream generation did not open"
                    );
                    match response_to_stream_failure(&open_error) {
                        FailureResponse::RejectGeneration(reason) => {
                            self.reject(pending.generation, reason);
                        }
                        FailureResponse::CloseSession(reason) => {
                            self.close(&pending.session, reason);
                        }
                    }
                }
            }
        }
    }

    /// Готовый поток → `PreparedMedia` → плеер.
    fn load(&mut self, pending: PendingGeneration, opened: OpenedPushedStream) {
        let prepared = match prepared_media(opened) {
            Ok(prepared) => prepared,
            Err(timeline_error) => {
                // Источник отдаёт живую шкалу только потоку без длительности, так что
                // это дефект договора, а не данных страницы; видео — обратно странице.
                error!(
                    ?timeline_error,
                    "Live timeline does not fit the browser stream"
                );
                self.reject(pending.generation, StreamRejection::UnsupportedCodec);
                return;
            }
        };
        // Страница на паузе — загрузка сразу на паузе: «играть, потом пауза» дало бы
        // мелькнуть звуку. Страница играет — старт сразу (решение 99).
        let start = if self.page_wants_play {
            LoadStart::Playing
        } else {
            LoadStart::Paused
        };
        let load_ticket = match self.playback.load(prepared, start) {
            Ok(load_ticket) => load_ticket,
            Err(load_error) => {
                // Плеер не взял команду (поток плеера встал): честнее вернуть видео
                // странице, чем показывать ей вечное ожидание.
                error!(%load_error, "The player did not accept the browser stream");
                self.reject(pending.generation, StreamRejection::UnsupportedCodec);
                return;
            }
        };
        info!(
            generation = pending.generation.get(),
            "Browser stream generation loaded"
        );
        self.loaded = Some(LoadedGeneration {
            generation: pending.generation,
            load_ticket,
            monitor: pending.monitor,
            fault_reported: false,
            page_state_reconciled: false,
        });
    }

    // ---- ответы странице ---------------------------------------------------------

    fn report_to_page(&mut self, observation: &PlaybackObservation, now: Instant) {
        let Some(loaded) = self.loaded.as_mut() else {
            return;
        };
        // Позиция прежнего медиа не принадлежит ставящемуся поколению: монитор по ней
        // выбросил бы полку и посчитал запас не от того места.
        let media_installed = loaded.is_shown_by(observation);
        if media_installed {
            loaded.monitor.note_playback_position(observation.position);
        }
        let reconcile_now = media_installed && !loaded.page_state_reconciled;
        if reconcile_now {
            loaded.page_state_reconciled = true;
        }
        let generation = loaded.generation;
        // Только отказ загрузки ЭТОГО поколения: отказ прежнего медиа (его `Failed`
        // ещё виден, пока новое ставится) новое поколение не отвергает.
        let fault = match observation.fault_of(loaded.load_ticket) {
            Some(fault) if !loaded.fault_reported => {
                loaded.fault_reported = true;
                Some(fault.clone())
            }
            _ => None,
        };
        let seek_request = loaded.monitor.take_page_seek_request();
        let buffer = loaded.monitor.buffer_status();

        if reconcile_now {
            self.reconcile_with_page(observation);
        }
        if let Some(fault) = fault {
            self.report_fault(generation, &fault);
        }
        if let Some(request) = seek_request {
            let seek_id = self.outbound.request_page_seek(request);
            debug!(
                seek_id = seek_id.0,
                "Asking the page to seek: the target is not on the shelf"
            );
        }
        // Буфер — всегда: без позиции источник считает от первого поданного сегмента.
        self.outbound.note_buffer(generation, buffer, now);
        if media_installed {
            self.outbound.tick_clock(observation, now);
        } else {
            self.outbound
                .tick_clock(&observation.while_media_installs(), now);
        }
    }

    /// Плеер только что поставил медиа поколения: привести его к странице.
    ///
    /// Установка сбрасывает скорость на 1× и берёт «играть/пауза» из загрузки, а
    /// команды страницы, пришедшие во время установки, достались прежнему медиа.
    /// Команда уходит, только если наблюдение расходится с намерением страницы.
    fn reconcile_with_page(&mut self, observation: &PlaybackObservation) {
        if observation.rate != self.page_rate {
            let rate = self.page_rate;
            self.command("restore the page rate", |playback| playback.set_rate(rate));
        }
        let player_plays = matches!(
            observation.state,
            PlaybackState::Playing
                | PlaybackState::Draining
                | PlaybackState::Opening
                | PlaybackState::Buffering
        );
        if self.page_wants_play && observation.state == PlaybackState::Paused {
            self.command("play after install", |playback| playback.play());
        } else if !self.page_wants_play && player_plays {
            self.command("pause after install", |playback| playback.pause());
        }
    }

    fn report_fault(&mut self, generation: StreamGeneration, fault: &BrowserPlaybackFault) {
        match fault {
            BrowserPlaybackFault::UnsupportedVideo => {
                self.reject(generation, StreamRejection::UnsupportedCodec);
            }
            BrowserPlaybackFault::VideoDecoderStartFailed
            | BrowserPlaybackFault::PlaybackFailed(_) => {
                warn!(
                    ?fault,
                    generation = generation.get(),
                    "Browser stream playback failed"
                );
            }
        }
    }

    /// Когда проснуться без внешнего повода.
    ///
    /// Сводка журнала плеера (раз в `BROWSER_PLAYBACK_SUMMARY_INTERVAL`) пишется во
    /// время `pump`, отдельного срока у неё нет: пока поколение загружено, цикл
    /// просыпается не реже тика часов (`CLOCK_INTERVAL` — 250 мс, много чаще
    /// сводки), а без поколения плееру нечего сводить — счётчики стоят, и будить
    /// цикл ради одинаковых строк незачем (без поколения и без повторов — `None`,
    /// цикл спит до пробуждения, холостого кручения нет).
    fn next_wake_at(&self, now: Instant) -> Option<Instant> {
        let open_retry = self
            .pending
            .as_ref()
            .map(|pending| pending.retry_at.unwrap_or(now));
        let clock = self
            .loaded
            .as_ref()
            .and(Some(self.outbound.next_clock_at().unwrap_or(now)));
        let outbound_retry = self
            .outbound
            .has_pending()
            .then(|| now + PENDING_RETRY_INTERVAL);
        [open_retry, clock, outbound_retry]
            .into_iter()
            .flatten()
            .min()
    }

    /// Команда плееру; недошедшая — в журнал (её не повторить: плеер встал).
    fn command(
        &mut self,
        what: &'static str,
        send: impl FnOnce(&mut dyn MediaPlayback) -> Result<(), BrowserPlaybackCommandError>,
    ) {
        if let Err(command_error) = send(self.playback.as_mut()) {
            warn!(%command_error, command = what, "Browser session player command failed");
        }
    }
}

/// Готовый поток → медиа для плеера (с живой шкалой, если это эфир).
fn prepared_media(
    opened: OpenedPushedStream,
) -> Result<PreparedMedia, PreparedMediaTimelineModeError> {
    let prepared =
        PreparedMedia::from_external_label(BROWSER_STREAM_LABEL, Box::new(opened.demuxer));
    match opened.live_timeline {
        Some(timeline) => prepared.with_dynamic_timeline(timeline),
        None => Ok(prepared),
    }
}

/// Скорость страницы → скорость плеера. Плеер умеет 0,25×–4×; за пределами — ближняя
/// граница (страница увидит настоящую скорость в `clock`).
fn player_rate(rate: PagePlaybackRate) -> PlayerPlaybackRate {
    // Тысячные доли протокола — не больше 16 000, в f32 представимы точно.
    let multiplier = rate.as_permille() as f32 / 1000.0;
    let clamped = multiplier.clamp(
        PlayerPlaybackRate::MIN.as_f32(),
        PlayerPlaybackRate::MAX.as_f32(),
    );
    PlayerPlaybackRate::new(clamped).unwrap_or(PlayerPlaybackRate::NORMAL)
}

#[cfg(test)]
mod tests;
