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
//!   говорят `waiting` там, где страница ждёт поколение (цель или 0, [`page_clock`]);
//! - часами открывающегося поколения: пока в плеере нет ни одного поколения сеанса,
//!   странице всё равно идёт «жду», иначе умер бы пульс тени (решение 180, [`page_clock`]);
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
//!   полос. С 04d поколение с названной позицией уходит плееру только когда у него
//!   есть данные там (ворота [`generation_placement`], путь C).
//! - транспортом зрителя окна (04a, [`viewer_transport`]): действие человека ведёт
//!   плеер и страницу, воля зрителя становится волей страницы, эхо страницы не дёргает
//!   плеер второй раз.
//! - долгом перед страницей за отданную плееру перемотку ([`seek_handover`]): от команды до
//!   ответа плеера часы говорят её цель.
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
use player_core::{PlaybackRate as PlayerPlaybackRate, PreparedInitialPosition, PreparedMedia};
use tracing::{debug, error, info, warn};

use self::ad_presence::AdPresence;
use self::ending::ActiveDrain;
use self::generation_feed::{FeedReport, LoadedFeed, OpenStep, PendingFeed, in_issue_order};
use self::generation_placement::{GenerationPlacement, PageAvailability};
use self::generation_rejection::ViewerAwareness;
use self::generation_start::{AfterLoad, GenerationStart};
use self::page_clock::{LoadedMediaShown, PageClockSource, PageStartWord};
use self::page_reopen::PageReopen;
use self::seek_handover::UnansweredSeek;
use super::intake_notice::{GenerationHandoff, IntakeNotice, PageIntent};
use super::outbound::{PageOutbound, RangeQueueOutcome};
use super::playback::{
    BrowserPlaybackCommandError, LoadStart, MediaLoadTicket, PlaybackObservation, PlayerSeekAnswers,
};
use super::rejection::FailureResponse;
use super::rejection_notice::GenerationRejectionNotice;
use pushed_media_source::RangeRequest;

mod ad_actions;
mod ad_presence;
mod drain;
mod ending;
mod farewell;
mod generation_feed;
mod generation_placement;
mod generation_rejection;
mod generation_start;
mod install_reconcile;
mod live_hls;
mod page_clock;
mod page_quality;
mod page_reopen;
mod page_subtitles;
mod page_variant_outcome;
mod page_variants;
mod position_carry;
mod resource_requests;
mod scrub_range;
mod seams;
mod seek_handover;
mod viewer_click_claim;
mod viewer_seek_notice;
mod viewer_transport;
mod wake_schedule;

pub(crate) use self::drain::{DrainEnd, DrainPolicy, DrainProgress, PausedDrain};
pub(crate) use self::farewell::ViewerCloseStep;
pub(crate) use self::generation_placement::{GenerationPlacementPolicy, GenerationWait};
pub(crate) use self::page_quality::PageViewNotice;
pub(crate) use self::page_reopen::{PageReopenNotice, PageReopenRefusal};
pub(crate) use self::position_carry::PositionCarryPolicy;
pub(crate) use self::seams::{MediaPlayback, SessionCloser};
pub(crate) use self::viewer_seek_notice::ViewerSeekNotice;
use self::viewer_transport::{PlayWill, ViewerWill};

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
    /// Куда и когда отдать поколение плееру: цель зрителя или страницы, пришедшая,
    /// пока оно не отдано, и ожидание данных у неё (04d, путь C).
    placement: GenerationPlacement,
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
    /// С какого места поколение передано плееру: там страница ждёт его, пока оно
    /// ставится (часы установки, см. `page_clock`).
    start: GenerationStart,
    /// Где плеер поставит медиа при загрузке (`None` — там, где лежат байты: эфир, поток
    /// без длительности, файл). Отличается от `start`: эфир с названным стартом на эту
    /// позицию не ставится.
    positioned_at: Option<Duration>,
    /// Перемотка этого поколения, отданная плееру, на которую он ещё не ответил.
    unanswered_seek: Option<UnansweredSeek>,
}

impl LoadedGeneration {
    /// Наблюдение описывает медиа этого поколения (плеер его уже поставил).
    fn is_shown_by(&self, observation: &PlaybackObservation) -> bool {
        observation.shows(self.load_ticket)
    }

    /// Где страница ждёт поколение, пока плеер его ставит: перемотка, отложенная до
    /// показа, новее места, с которого поколение передано плееру.
    const fn start_awaited_by_page(&self) -> GenerationStart {
        match self.seek_when_shown {
            Some(target) => GenerationStart::PageSeekedTo(target),
            None => self.start,
        }
    }

    /// Перемотка страницы ровно повторяет место, куда плеер и так ставит поколение: ни
    /// отдельной перемотки, ни отложенной раньше другой цели не нужно (последняя
    /// побеждает). Так приходит итоговое T0 передачи, названное ещё в начале повтора
    /// (шаг 3 договора `handoff-controller`): лишняя перемотка выбросила бы только что
    /// показанный кадр и заново декодировала бы то же место.
    ///
    /// Совпадение точное: даже близкая цель — настоящая перемотка страницы. И только пока
    /// поколение ставится: потом позиция уже ушла, и та же цель — перемотка назад.
    fn absorb_page_seek_to_start(&mut self, target: Duration) -> bool {
        let repeats_start = !self.page_state_reconciled && self.positioned_at == Some(target);
        if repeats_start {
            self.seek_when_shown = None;
        }
        repeats_start
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
    drain: Option<ActiveDrain>,
    /// Экземпляр, чей вход поток приёма уже разобрал и запечатал (`InstanceSealed`).
    /// Событие конца сеанса может обогнать данные, поэтому доигрывание решает только
    /// после этого уведомления.
    sealed_instance: Option<SessionRef>,
    /// Последнее наблюдение плеера (ответ на «доиграно?» опирается на него).
    last_observation: Option<PlaybackObservation>,
    /// Воля зрителя, которую страница ещё не подтвердила (правило эха, 04a).
    viewer_will: ViewerWill,
    /// Отказ поколения, о котором окно ещё не сказало зрителю (04b, решение 129).
    rejection_notice: GenerationRejectionNotice,
    /// Сроки ворот нового поколения (04d).
    placement_policy: GenerationPlacementPolicy,
    /// Отказ перемотки зрителя, о котором окно ещё не сказало (09a2, Д-5).
    viewer_seek_notice: ViewerSeekNotice,
    /// Назвала ли страница в этом сеансе, где она стоит (без этого первому поколению
    /// часов не шлём, см. `page_clock`).
    page_start_word: PageStartWord,
    /// Ответы плеера на перемотки, виденные последним наблюдением: от них считается ответ
    /// на следующую отданную перемотку ([`seek_handover`]).
    seek_answers_seen: PlayerSeekAnswers,
    /// Реклама на странице: плашка и защита от повторного клика (09d); только в памяти.
    ad: AdPresence,
    /// Возврат ушедшей страницы в тот же сеанс («сирота», 11b1, [`page_reopen`]).
    page_reopen: PageReopen,
    /// Склад субтитров страницы: живёт весь сеанс, поколения его не трогают (13b1).
    page_subtitles: super::page_subtitles::shelf::PageSubtitleShelf,
    /// Просьба зрителя показать страницу принята, окно ещё не сказало (13c, [`page_quality`]).
    page_view_notice: PageViewNotice,
    /// Варианты страницы: склад меню, возврат на секунду, тост (14a, [`page_variants`]).
    variants: self::page_variants::SessionPageVariants,
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
            drain_policy: DrainPolicy::recommended(PausedDrain::ExitAtOnce),
            drain: None,
            sealed_instance: None,
            last_observation: None,
            viewer_will: ViewerWill::default(),
            rejection_notice: GenerationRejectionNotice::Nothing,
            placement_policy: GenerationPlacementPolicy::recommended(),
            viewer_seek_notice: ViewerSeekNotice::Nothing,
            page_start_word: PageStartWord::NotSaid,
            seek_answers_seen: PlayerSeekAnswers::default(),
            ad: AdPresence::default(),
            page_reopen: PageReopen::default(),
            page_subtitles: Default::default(),
            page_view_notice: PageViewNotice::Nothing,
            variants: self::page_variants::SessionPageVariants::new(),
        }
    }

    /// Открыт сеанс. Повтор того же экземпляра ничего не меняет; новый экземпляр
    /// (другая вкладка или новое открытие) — новая страница: ждущее поколение
    /// выбрасывается, прежнее видео встаёт на паузу, исходящие старой странице
    /// забываются. Прощание старой вкладке (`bid_farewell_to_replaced`, 04b) обязано
    /// прозвучать ДО этого вызова: после него позиции её видео уже нет.
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
        self.page_start_word = PageStartWord::NotSaid;
        self.viewer_will.forget();
        // Реклама прежней страницы к новой не относится, возврат прежней — тоже.
        self.ad.clear();
        self.page_reopen = PageReopen::default();
        self.page_subtitles.start_new_session();
        self.variants.start_new_session();
        self.outbound.reset_for_new_session();
        if self.loaded.take().is_some() {
            self.command("pause for a new session", |playback| playback.pause());
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
        self.viewer_will.expire(now);
        self.expire_page_reopen(now);
        self.poll_pending(now);
        self.settle_position_carry(now);
        let observation = self.playback.pump(now);
        self.report_to_page(&observation, now);
        self.offer_downloaded_range(&observation);
        // Доигрываем: страницы нет, ей ничего не шлём (ни запросов байтов, ни часов).
        if self.drain.is_none() {
            self.send_range_requests();
            self.send_resource_requests();
            self.outbound.flush(now);
        } else if self.page_hears_clock() {
            // Сирота (11b1): страницы нет, но связь держится — уходит просьба `reopen_page`.
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
        self.log_resource_request_totals();
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
            IntakeNotice::InstanceSealed { session } => self.note_instance_sealed(session),
            IntakeNotice::PageIntent { intent, .. } => self.follow_page(intent),
            IntakeNotice::AdState { state, .. } => self.apply_ad_state(state),
            IntakeNotice::PageLink { change, .. } => self.follow_page_link(change),
            IntakeNotice::PageSubtitleReady { document, .. } => {
                self.shelve_page_subtitle(*document)
            }
            IntakeNotice::PageVariants { update, .. } => self.apply_page_variant_update(update),
            IntakeNotice::PlayerSeekAcknowledged {
                seek_id, target, ..
            } => {
                self.note_position_carry_answer(Duration::from_micros(target.as_micros()));
                debug!(
                    seek_id = seek_id.0,
                    target_us = target.as_micros(),
                    "The page started the seek the player asked for"
                );
            }
            IntakeNotice::Reject {
                generation, reason, ..
            } => self.reject(generation, reason, ViewerAwareness::Unaware),
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
        // Файл или HLS заменяет ждущий поток сегментов ТОГО ЖЕ поколения:
        // `generation_started` сразу создаёт поток сегментов (MSE-сообщения вроде
        // `duration_changed` могут прийти до первого SourceBuffer), а `file_source_added`
        // / `hls_source_added` приходит следом и задаёт вид поколения. Загруженное поколение так не заменяется: до плеера
        // поток сегментов доходит только с SourceBuffer, а сторож поколений протокола
        // (`FileSourceAfterSourceBuffer`) закрывает сеанс на `file_source_added` после
        // него — до приёмников (сквозной тест `a_file_announced_after_a_source_buffer_is_closed_by_the_gate`).
        let upgrades_pending_to_file = handoff.source.replaces_announced_segments()
            && self.pending.as_ref().is_some_and(|pending| {
                pending.generation == handoff.generation && pending.feed.is_segments()
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
        // Цель, записанная за ждущим поколением, переходит к файлу того же поколения и
        // (цель зрителя) к поколению, сменившему ждущее (04d).
        let placement =
            self.placement_for_new_generation(upgrades_pending_to_file, &handoff.source);
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
            placement,
            retry_at: None,
        });
        // Решение 280: после смены варианта зрителем — на его секунду (не рекламу).
        self.carry_position_to_new_generation(Instant::now());
    }

    /// Намерение страницы → плеер.
    fn follow_page(&mut self, intent: PageIntent) {
        match intent {
            // Источник уже знает о перемотке (переходник сообщил ему в порядке кадров),
            // поэтому просьбы «перемотай страницу» к этой цели не будет.
            PageIntent::FollowPageSeek(target) => {
                self.page_start_word = PageStartWord::Said;
                self.follow_page_seek(Duration::from_micros(target.as_micros()));
            }
            // play/pause/скорость страницы проходят правило эха воли зрителя.
            PageIntent::Play => self.follow_page_play(PlayWill::Play),
            PageIntent::Pause => self.follow_page_play(PlayWill::Pause),
            PageIntent::SetRate(rate) => self.follow_page_rate(rate),
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

    fn close(&self, session: &SessionRef, reason: SessionCloseReason) {
        // Обрыв по ошибке (4003) не молчаливый: сначала прощание, видео возвращается
        // странице (решение 132). Остальные причины закрывают как раньше.
        if reason == SessionCloseReason::ProtocolViolation {
            self.close_with_stream_error(session);
            return;
        }
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
        // Страница ушла (доигрываем) — байтов у цели больше не будет (ворота 04d).
        let page = if self.drain.is_some() {
            PageAvailability::Gone
        } else {
            PageAvailability::Listening
        };
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        pending.placement.arm(now);
        let due = bytes_arrived || pending.retry_at.is_none_or(|retry_at| retry_at <= now);
        if !due {
            return;
        }
        match pending
            .feed
            .poll_open(&mut pending.placement, now, page, self.playback.as_ref())
        {
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
                            self.reject(pending.generation, reason, ViewerAwareness::Unaware);
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
                    self.reject(
                        pending.generation,
                        StreamRejection::UnsupportedCodec,
                        ViewerAwareness::Unaware,
                    );
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
        // Загрузка забирает `prepared`: где оно стоит, запоминаем до неё.
        let positioned_at = match prepared.prepared_initial_position() {
            PreparedInitialPosition::PositionedAt {
                target_position, ..
            } => Some(target_position.as_duration()),
            PreparedInitialPosition::Beginning => None,
        };
        let load_ticket = match self.playback.load(prepared, start) {
            Ok(load_ticket) => load_ticket,
            Err(load_error) => {
                // Плеер не взял команду (поток плеера встал): честнее вернуть видео
                // странице, чем показывать ей вечное ожидание.
                error!(%load_error, "The player did not accept the browser stream");
                self.reject(
                    pending.generation,
                    StreamRejection::UnsupportedCodec,
                    ViewerAwareness::Unaware,
                );
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
            start: pending.placement.named_start(),
            positioned_at,
            unanswered_seek: None,
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
        self.seek_answers_seen = observation.seek_answers;
        let awaited_target = self.awaited_generation_target();
        let new_source = self.new_source_stance();
        let Some(loaded) = self.loaded.as_mut() else {
            // В плеере пока ничего нет, но страница уже ждёт: ей нужны часы (решение 180).
            self.tell_page_while_nothing_is_loaded(observation, now);
            return;
        };
        loaded.settle_unanswered_seek(observation.seek_answers, now);
        // Позиция прежнего медиа не принадлежит ставящемуся поколению: монитор по ней
        // выбросил бы полку и посчитал запас не от того места.
        let media_installed = loaded.is_shown_by(observation);
        let loaded_shown = if media_installed {
            LoadedMediaShown::Shown
        } else {
            LoadedMediaShown::StillInstalling(loaded.start_awaited_by_page())
        };
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
        let fault = match observation.load_fault_of(loaded.load_ticket) {
            Some(load_fault) if !loaded.fault_reported => {
                loaded.fault_reported = true;
                Some(load_fault.clone())
            }
            _ => None,
        };

        if reconcile_now {
            let seek_after_show = self.seek_after_show_with_viewer_click(seek_when_shown, now);
            self.reconcile_with_page(observation, seek_after_show);
        }
        if let Some(fault) = fault {
            self.report_fault(generation, &fault);
        }
        // Страницы нет (доигрываем) — ей ничего не говорим; монитор позицию уже получил.
        // Кроме часов сироте (11b1): они её пульс и секунда плеера для возврата страницы.
        if !self.page_hears_clock() {
            return;
        }
        let page_present = self.drain.is_none();
        // Буфер и просьбы перемотки есть только у сегментов; у файла страница ничего не
        // качает сама, и позицию никуда не репортим.
        if page_present
            && let FeedReport::Segments {
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
        // Часы — только того источника, где сейчас стоит элемент страницы (Д-4,
        // правила — `page_clock`): чужая позиция или чужой `ended` сбили бы страницу.
        // Перемотка без ответа плеера: наблюдение ещё про старое место, а страница уже на
        // цели. Тик старого места (у свежей установки — «0») отмотал бы её назад.
        let unanswered_seek = self
            .loaded
            .as_ref()
            .and_then(LoadedGeneration::unanswered_seek_target);
        // Окно эфира — по часам без долга перемотки: она того же поколения (`live_hls`).
        let live_window_clock = PageClockSource::choose(awaited_target, new_source, loaded_shown);
        let loaded_shown = unanswered_seek.map_or(loaded_shown, LoadedMediaShown::SeekNotAnswered);
        let clock_source = PageClockSource::choose(awaited_target, new_source, loaded_shown);
        self.outbound
            .tick_clock(&clock_source.page_view_of(observation), now);
        if page_present {
            self.report_live_window(generation, live_window_clock, observation, now);
        }
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
