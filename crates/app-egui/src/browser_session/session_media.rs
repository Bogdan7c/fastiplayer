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
//!   не к видео прежнего поколения в плеере. Видео прежнего поколения она не двигает:
//!   она становится стартовой позицией нового (03e, [`generation_start`]) — новый
//!   `MediaSource` сбрасывает время страницы в начало, и страница сразу говорит, где
//!   зритель был. Не назвала — поколение начнётся там, где у него есть данные всех
//!   полос.
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
use browser_bridge_session::SessionRef;
use player_core::{PlaybackRate as PlayerPlaybackRate, PreparedMedia};
use tracing::{debug, error, info, warn};

use self::drain::{DrainPolicy, DrainWatch};
use self::generation_feed::{FeedReport, LoadedFeed, OpenStep, PendingFeed, in_issue_order};
use self::generation_start::{AfterLoad, GenerationStart};
use super::intake_notice::{GenerationHandoff, IntakeNotice, PageIntent};
use super::outbound::{PENDING_RETRY_INTERVAL, PageOutbound, RangeQueueOutcome};
use super::playback::{
    BrowserPlaybackCommandError, BrowserPlaybackFault, LoadStart, MediaLoadTicket,
    PlaybackObservation,
};
use super::rejection::FailureResponse;
use pushed_media_source::RangeRequest;

mod drain;
mod generation_feed;
mod generation_start;
mod install_reconcile;
mod seams;

use self::drain::{DrainContent, DrainSnapshot, IntakeStance, PageStance};
pub(crate) use self::drain::{DrainEnd, DrainProgress};
pub(crate) use self::seams::{MediaPlayback, SessionCloser};

/// Метка источника для плеера. Нейтральная: ни заголовка, ни домена страницы —
/// метка может попасть в журнал плеера (инвариант 7).
pub(crate) const BROWSER_STREAM_LABEL: &str = "browser stream";

// ---- состояние -----------------------------------------------------------------

/// Поколение, которое ещё открывается.
struct PendingGeneration {
    /// Сеанс.
    session: SessionRef,
    /// Номер.
    generation: StreamGeneration,
    /// Откуда плеер возьмёт содержимое: сегменты или файл (сторона открытия и то, что
    /// переходит к загруженному поколению).
    feed: PendingFeed,
    /// Откуда поколение начнёт показ: перемотка страницы, пришедшая, пока оно
    /// открывалось, — его стартовая позиция.
    start: GenerationStart,
    /// Когда спросить снова (`None` — при первой возможности).
    retry_at: Option<Instant>,
}

/// Поколение, загруженное в плеер.
struct LoadedGeneration {
    /// Номер.
    generation: StreamGeneration,
    /// Квитанция загрузки этого поколения в плеер.
    load_ticket: MediaLoadTicket,
    /// Содержимое загруженного поколения: монитор буфера (сегменты) или запросы байтов (файл).
    feed: LoadedFeed,
    /// Об отказе плеера уже сообщено (один раз на поколение).
    fault_reported: bool,
    /// После установки плеер уже приведён к скорости и «играть/пауза» страницы.
    page_state_reconciled: bool,
    /// Перемотка, которую исполнить, когда плеер ПОКАЖЕТ это поколение: команды,
    /// отданные во время установки, достаются прежнему медиа (перемотка страницы
    /// после загрузки, но до показа; старт файла с позиции страницы).
    seek_when_shown: Option<Duration>,
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
    /// Сроки доигрывания.
    drain_policy: DrainPolicy,
    /// Доигрывание идёт: сеанс закончился концом, после которого зритель досматривает
    /// полученное. Страницы больше нет — ей не шлём ни часов, ни запросов.
    drain: Option<DrainWatch>,
    /// Экземпляр, чей вход поток приёма уже разобрал и запечатал (`InstanceSealed`).
    /// Событие конца сеанса может обогнать данные, поэтому доигрывание решает только
    /// после этого уведомления.
    sealed_instance: Option<SessionRef>,
    /// Последнее наблюдение плеера (ответ на «доиграно?» опирается на него).
    last_observation: Option<PlaybackObservation>,
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
            drain_policy: DrainPolicy::recommended(),
            drain: None,
            sealed_instance: None,
            last_observation: None,
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
        // Новый сеанс во время доигрывания заменяет доигрываемый: доигрывание забыто.
        self.drain = None;
        self.page_wants_play = true;
        self.page_rate = PlayerPlaybackRate::NORMAL;
        self.outbound.reset_for_new_session();
        if self.loaded.take().is_some() {
            self.command("pause for a new session", |playback| playback.pause());
        }
    }

    /// Сеанс закончился: отложенные сообщения его странице больше некому слать, а
    /// всё полученное доигрывается (см. [`Self::playback_drain`]).
    ///
    /// Конец чужого (уже заменённого) экземпляра ничего не трогает.
    pub(crate) fn on_session_ended(&mut self, session: &SessionRef) {
        if self.is_current(session) {
            self.outbound.forget_pending_of_ended_session();
            self.drain = Some(DrainWatch::begin(self.drain_policy));
        }
    }

    /// Доиграно ли полученное после конца сеанса (зовёт главный цикл, пока машина
    /// выхода в состоянии «доигрываем»).
    ///
    /// Без `on_session_ended` у текущего экземпляра доигрывать нечего — процесс
    /// выходит сразу.
    pub(crate) fn playback_drain(&mut self, now: Instant) -> DrainProgress {
        let intake =
            if self.current_session.is_some() && self.sealed_instance == self.current_session {
                IntakeStance::Sealed
            } else {
                IntakeStance::Open
            };
        let snapshot = DrainSnapshot {
            intake,
            content: self.drain_content(),
            page: if self.page_wants_play {
                PageStance::WantsPlay
            } else {
                PageStance::WantsPause
            },
        };
        match self.drain.as_mut() {
            Some(watch) => watch.evaluate(snapshot, now),
            None => DrainProgress::Drained(DrainEnd::NothingToPlay),
        }
    }

    /// Что держит медиа-часть для вопроса «доиграно?».
    fn drain_content(&self) -> DrainContent {
        // Ждущее поколение новее загруженного и заменит его: пока оно не открылось,
        // конец, отказ или позиция прежнего видео ничего не решают.
        if self.pending.is_some() {
            return DrainContent::Opening;
        }
        let Some(loaded) = self.loaded.as_ref() else {
            return DrainContent::Nothing;
        };
        let shown = self
            .last_observation
            .as_ref()
            .filter(|observation| loaded.is_shown_by(observation));
        DrainContent::Loaded {
            position: shown.map(|observation| observation.position),
            ended: shown.is_some_and(|observation| observation.ended),
            faulted: self
                .last_observation
                .as_ref()
                .and_then(|observation| observation.fault_of(loaded.load_ticket))
                .is_some(),
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
        // Доигрываем: страницы нет, ей ничего не шлём (ни запросов байтов, ни часов).
        if self.drain.is_none() {
            self.send_range_requests();
            self.outbound.flush(now);
        }
        self.last_observation = Some(observation);
        self.next_wake_at(now)
    }

    /// Конец процесса: итог запросов байтов и плеера в журнал, остановка плеера.
    pub(crate) fn finish(self) {
        // Только числа: сколько запросов ушло странице и сколько байтов просили.
        let ranges = self.outbound.range_counters();
        info!(
            range_requests_sent = ranges.sent,
            range_bytes_requested = ranges.bytes_requested,
            range_requests_refused = ranges.refused_queue_full,
            range_requests_unencodable = ranges.unencodable,
            "Browser file range requests"
        );
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
            IntakeNotice::InstanceSealed { session } => self.sealed_instance = Some(session),
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
        // Файл заменяет ждущий поток сегментов ТОГО ЖЕ поколения: `generation_started`
        // сразу создаёт поток сегментов (MSE-сообщения вроде `duration_changed` могут
        // прийти до первого SourceBuffer), а `file_source_added` приходит следом и
        // делает поколение файлом. Загруженное поколение так не заменяется: до плеера
        // поток сегментов доходит только с SourceBuffer, а сторож поколений протокола
        // (`FileSourceAfterSourceBuffer`) закрывает сеанс на `file_source_added` после
        // него — до приёмников (сквозной тест `a_file_announced_after_a_source_buffer_is_closed_by_the_gate`).
        let upgrades_pending_to_file = handoff.source.is_file()
            && self.pending.as_ref().is_some_and(|pending| {
                pending.generation == handoff.generation && !pending.feed.is_file()
            });
        if !upgrades_pending_to_file
            && newest_known.is_some_and(|newest| handoff.generation <= newest)
        {
            debug!(
                generation = handoff.generation.get(),
                "Stream generation older than the one already known dropped"
            );
            return;
        }
        // Файл, заменивший поток сегментов того же поколения, — то же поколение:
        // перемотка страницы, уже записанная за ним, остаётся его стартом.
        let start = match &self.pending {
            Some(pending) if upgrades_pending_to_file => pending.start,
            _ => GenerationStart::FirstCommonData,
        };
        if let Some(superseded) = &self.pending {
            debug!(
                generation = superseded.generation.get(),
                "Pending stream generation superseded before it opened"
            );
        }
        self.pending = Some(PendingGeneration {
            session: handoff.session,
            generation: handoff.generation,
            feed: PendingFeed::from_source(handoff.source),
            start,
            retry_at: None,
        });
    }

    /// Намерение страницы → плеер.
    fn follow_page(&mut self, intent: PageIntent) {
        match intent {
            PageIntent::FollowPageSeek(target) => {
                // Источник уже знает о перемотке (переходник сообщил ему в порядке
                // кадров), поэтому просьбы «перемотай страницу» к этой цели не будет.
                if let Some(pending) = self.pending.as_mut() {
                    // Переходник сообщил перемотку источнику САМОГО НОВОГО поколения —
                    // того, что ещё открывается; видео прежнего поколения она не
                    // касается. Это стартовая позиция нового поколения (последняя
                    // побеждает): плеер поставит его сразу туда.
                    let start = Duration::from_micros(target.as_micros());
                    pending.start = GenerationStart::PageSeekedTo(start);
                    debug!(
                        generation = pending.generation.get(),
                        target_us = target.as_micros(),
                        "Page seek is the start position of the generation that is still opening; the loaded video is not moved"
                    );
                    return;
                }
                let target = Duration::from_micros(target.as_micros());
                let Some(loaded) = self.loaded.as_mut() else {
                    debug!(
                        "Page seek before the stream opened; the player starts where the bytes start"
                    );
                    return;
                };
                if !loaded.page_state_reconciled {
                    // Плеер ещё ставит поколение: команда сейчас досталась бы прежнему
                    // медиа. Исполнится при показе (последняя побеждает).
                    loaded.seek_when_shown = Some(target);
                    return;
                }
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
        if self.drain.is_some() {
            // Сеанс кончился: сказать отказ некому (отложенное повторялось бы вечно).
            info!(
                generation = generation.get(),
                ?reason,
                "Stream generation rejected after the session ended"
            );
            return;
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
        match pending.feed.poll_open(pending.start) {
            OpenStep::Waiting(retry_after) => {
                pending.retry_at = Some(now + retry_after);
            }
            OpenStep::Ready {
                prepared,
                feed,
                after_load,
            } => {
                if let Some(pending) = self.pending.take() {
                    self.load(&pending, *prepared, feed, after_load);
                }
            }
            OpenStep::Failed { class, response } => {
                if let Some(pending) = self.pending.take() {
                    // Только класс ошибки: текст несёт подробности байтов страницы.
                    warn!(
                        failure_class = class,
                        generation = pending.generation.get(),
                        "Stream generation did not open"
                    );
                    match response {
                        FailureResponse::RejectGeneration(reason) => {
                            self.reject(pending.generation, reason);
                        }
                        FailureResponse::CloseSession(reason) => {
                            self.close(&pending.session, reason);
                        }
                    }
                }
            }
            OpenStep::Unplayable => {
                // Дефект сборки, а не данных страницы; видео — обратно странице.
                if let Some(pending) = self.pending.take() {
                    self.reject(pending.generation, StreamRejection::UnsupportedCodec);
                }
            }
        }
    }

    /// Готовое медиа → плеер; затем то, что нужно для старта с нужного места.
    fn load(
        &mut self,
        pending: &PendingGeneration,
        prepared: PreparedMedia,
        feed: LoadedFeed,
        after_load: AfterLoad,
    ) {
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
            feed,
            fault_reported: false,
            page_state_reconciled: false,
            // Файл перематывается, когда плеер его покажет: до установки команда
            // досталась бы прежнему медиа.
            seek_when_shown: match after_load {
                AfterLoad::Nothing => None,
                AfterLoad::SeekTo(target) => Some(target),
            },
        });
    }

    /// Запросы байтов файла → странице (`fetch_range`), сразу по пробуждению.
    ///
    /// Забираются у ждущего файла (первый запрос открытия выдаёт первый `poll_open`) и
    /// у загруженного (чтение, перемотка, повтор после сбоя сети).
    fn send_range_requests(&mut self) {
        let pending_requests = self
            .pending
            .as_ref()
            .map(|pending| pending.feed.take_range_requests())
            .unwrap_or_default();
        let loaded_requests = self
            .loaded
            .as_ref()
            .map(|loaded| loaded.feed.take_range_requests())
            .unwrap_or_default();
        // Странице запросы уходят в порядке выдачи: номера сквозные на сеанс, а ждущий
        // файл мог выдать запрос раньше, чем загруженный — свой последний.
        let all_requests = in_issue_order(pending_requests.into_iter().chain(loaded_requests));
        for request in all_requests {
            self.queue_range_request(request);
        }
    }

    /// Один запрос байтов — в очередь к странице. Отказ очереди (недостижимый при честной
    /// работе, см. `MAX_PENDING_RANGE_REQUESTS`) — внутренняя ошибка: файл ждал бы байтов,
    /// о которых страница не знает, поэтому сеанс закрывается как нарушение протокола, а
    /// в журнал идёт причина.
    fn queue_range_request(&mut self, request: RangeRequest) {
        let outcome = self.outbound.request_range(request);
        if outcome == RangeQueueOutcome::Queued {
            return;
        }
        error!(
            ?outcome,
            "A range request could not be queued for the page; closing the session"
        );
        if let Some(session) = self.current_session.clone() {
            self.close(&session, SessionCloseReason::ProtocolViolation);
        }
    }

    // ---- ответы странице ---------------------------------------------------------

    fn report_to_page(&mut self, observation: &PlaybackObservation, now: Instant) {
        let Some(loaded) = self.loaded.as_mut() else {
            return;
        };
        // Позиция прежнего медиа не принадлежит ставящемуся поколению: монитор по ней
        // выбросил бы полку и посчитал запас не от того места.
        let media_installed = loaded.is_shown_by(observation);
        let report = loaded
            .feed
            .report(media_installed.then_some(observation.position));
        let reconcile_now = media_installed && !loaded.page_state_reconciled;
        let seek_when_shown = if reconcile_now {
            loaded.page_state_reconciled = true;
            loaded.seek_when_shown.take()
        } else {
            None
        };
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

        if reconcile_now {
            self.reconcile_with_page(observation, seek_when_shown);
        }
        if let Some(fault) = fault {
            self.report_fault(generation, &fault);
        }
        // Страницы нет (доигрываем) — ей ничего не говорим; монитор позицию уже получил.
        if self.drain.is_some() {
            return;
        }
        // Буфер и просьбы перемотки есть только у сегментов; у файла страница ничего не
        // качает сама, и позицию никуда не репортим.
        if let FeedReport::Segments {
            seek_request,
            buffer,
        } = report
        {
            if let Some(request) = seek_request {
                let seek_id = self.outbound.request_page_seek(request);
                debug!(
                    seek_id = seek_id.0,
                    "Asking the page to seek: the target is not on the shelf"
                );
            }
            // Буфер — всегда: без позиции источник считает от первого поданного сегмента.
            self.outbound.note_buffer(generation, buffer, now);
        }
        if media_installed {
            self.outbound.tick_clock(observation, now);
        } else {
            self.outbound
                .tick_clock(&observation.while_media_installs(), now);
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
        // Часы идут странице; пока доигрываем, её нет, и цикл будит `playback_drain`.
        let clock = self
            .loaded
            .as_ref()
            .filter(|_| self.drain.is_none())
            .map(|_| self.outbound.next_clock_at().unwrap_or(now));
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
