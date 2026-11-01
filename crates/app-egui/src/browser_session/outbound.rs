//! Исходящие сообщения странице: часы, состояние буфера, просьбы перемотаться,
//! отказы от поколения (сессия 03b, пакет C3) и команды транспорта зрителя (04a).
//!
//! Модуль разделён на две части:
//! - чистые правила (без отправки и без часов ОС): [`clock_state_for`],
//!   [`clock_position`], [`clock_rate`], [`ClockCadence`], [`BufferReportPolicy`] —
//!   «что и когда сказать странице»;
//! - [`PageOutbound`] — очередь и отправка через [`PageSender`] (настоящий —
//!   `SessionControl` сервера, в тестах — подделка) с правилами потерь.
//!
//! Правила потерь (решение пакета C3):
//! - `clock` — поток показаний 4 раза в секунду: если очередь к странице полна, тик
//!   выбрасывается, следующий его заменит (повторять устаревшую позицию вредно);
//! - `buffer_state`, `seek`, `stream_rejected`, `play`/`pause`, `set_rate` —
//!   единичные события: при полной очереди они остаются и уходят на следующем проходе
//!   (у `buffer_state` — самое свежее значение, старое просто заменяется);
//! - страница отключилась (обрыв, экстеншен переподключается в пределах
//!   `reconnect_grace`) — единичные события тоже остаются и уходят, когда она
//!   вернётся (повтор раз в [`PENDING_RETRY_INTERVAL`]); иначе отказ или просьба
//!   перемотаться, выданные во время переподключения, терялись бы навсегда. Тик
//!   часов при этом выбрасывается, как при полной очереди;
//! - отложенного не больше четырёх событий, по одному на «место»: `stream_rejected` —
//!   только самый свежий (старое поколение страницу уже не интересует), `seek` — только
//!   последняя просьба (цель новее заменяет старую), `play`/`pause` — ОДНО место на
//!   двоих (последняя воля зрителя: «пауза» после неотправленного «играть» заменяет
//!   его), `set_rate` — последняя скорость. Так ожидание не растёт, сколько бы ни
//!   длилось переподключение;
//! - `fetch_range` (запрос байтов файла `<video src>`) — НАДЁЖНОЕ сообщение: при
//!   полной очереди и пока страница переподключается он остаётся и уходит позже, а с
//!   другими `fetch_range` не сливается (у каждого свой номер, и страница отвечает на
//!   каждый). Граница — [`MAX_PENDING_RANGE_REQUESTS`], и она недостижима при честной
//!   работе (доказательство у константы). Если всё же достигнута, запрос НЕ теряется
//!   молча: [`PageOutbound::request_range`] отказывает явно
//!   ([`RangeQueueOutcome::QueueFull`]), а владелец закрывает сеанс;
//! - сеанс закончился или начался новый — отложенное забывается
//!   ([`PageOutbound::forget_pending_of_ended_session`],
//!   [`PageOutbound::reset_for_new_session`]); сервер выключается — сообщение
//!   выбрасывается с отладочной записью.
//!
//! Приватность (инвариант 7): в журнал идут только виды сообщений и ошибок, без
//! содержимого страницы.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use browser_bridge_protocol::limits::{
    MAX_JSON_SAFE_INTEGER, MAX_PLAYBACK_RATE_PERMILLE, MIN_PLAYBACK_RATE_PERMILLE,
};
use browser_bridge_protocol::session::player::{
    BufferIntake as PageBufferIntake, ClockSequence, ClockState, StreamRejection,
};
use browser_bridge_protocol::{
    ByteOffset, MediaTime, PlaybackRate as PagePlaybackRate, PlayerSessionMessage,
    RangeFetchLength, RangeRequestId as PageRangeRequestId, SeekId, StreamGeneration,
};
use browser_bridge_session::{SendToPageError, SessionControl};
use player_core::{PlaybackRate as PlayerPlaybackRate, PlaybackState};
use pushed_media_source::{BufferIntake, BufferStatus, PageSeekRequest, RangeRequest};
use tracing::{debug, error};

use super::playback::PlaybackObservation;

/// Шаг часов: 4 тика в секунду (протокол, решение 85; `results/00c.md`, п. 4).
/// Чаще — лишняя нагрузка на канал и страницу, реже — муляж элемента заметно
/// «дёргает» `timeupdate` и полосу прогресса.
pub(crate) const CLOCK_INTERVAL: Duration = Duration::from_millis(250);

/// Насколько должен измениться запас вперёд, чтобы сообщить странице сразу.
/// Странице запас нужен для своей полосы «загружено» и решения «качать ли дальше»;
/// дрожание меньше секунды ей ничего не даёт, а сообщений было бы 25+ в секунду
/// (запас меняется с каждым кадром).
pub(crate) const BUFFER_AHEAD_REPORT_STEP: Duration = Duration::from_secs(1);

/// Не реже какого срока повторять состояние буфера, даже если оно не менялось.
/// Экстеншен после переподключения или потерянного события узнаёт правду не
/// позже чем через 2 с; при 4 тиках часов в секунду это один `buffer_state` на 8
/// тиков — на канал не влияет.
pub(crate) const BUFFER_HEARTBEAT: Duration = Duration::from_secs(2);

/// Сколько неотправленных `fetch_range` держим в очереди.
///
/// Почему при честной работе это число недостижимо:
/// - `PushedFileRequests::take_requests` отдаёт только ЖИВЫЕ запросы, а у файла живой
///   запрос один: следующий появляется после итога предыдущего от страницы либо вместо
///   запроса, отменённого перемоткой плеера или повтором после сбоя сети (это тоже
///   требует ответа страницы);
/// - главный цикл берёт запросы не больше чем у двух файлов: ждущего и загруженного
///   (старшее поколение вытесняет младшее, вытесненный файл больше не опрашивается);
/// - итоги страницы приходят, только пока она подключена, и тогда очередь к ней
///   разгребается за миллисекунды; без страницы новых итогов нет, а перемотки плеера
///   по запросу самой страницы (`seek_started{page_seek}`) приходят тоже только от неё.
///
/// Остаётся страница, которая шлёт десятки перемоток подряд и отваливается, пока
/// очередь не разгребли, — это не честная работа. Тогда запрос не теряется молча и не
/// выбрасывается "самый старый": `request_range` отказывает явно, и владелец закрывает
/// сеанс как нарушение протокола.
pub(crate) const MAX_PENDING_RANGE_REQUESTS: usize = 32;

/// Через сколько повторить отложенные сообщения, если очередь к странице была
/// полна. Очередь сервера разгребается потоком соединения за миллисекунды;
/// 50 мс — не крутить цикл вхолостую и не задерживать `seek` заметно для глаза.
pub(crate) const PENDING_RETRY_INTERVAL: Duration = Duration::from_millis(50);

// ---- отправка ------------------------------------------------------------------

/// Куда уходят сообщения странице. Настоящий — `SessionControl` сервера.
pub(crate) trait PageSender {
    /// Ставит сообщение в очередь к подключённой странице.
    ///
    /// # Errors
    /// Нет страницы, очередь полна, сервер выключается или сообщение не по протоколу.
    fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError>;
}

impl PageSender for SessionControl {
    fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
        Self::send_to_page(self, message)
    }
}

/// Что стало с сообщением после попытки отправки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// Поставлено в очередь к странице.
    Sent,
    /// Очередь полна: решает вызывающий (тик — выбросить, событие — повторить).
    QueueFull,
    /// Страница сейчас не подключена (может вернуться): решает вызывающий, как
    /// при полной очереди.
    PageDetached,
    /// Отправлять незачем (сервер выключается, сообщение не по протоколу): выброшено.
    Discarded,
}

// ---- чистые правила ------------------------------------------------------------

/// Что делает плеер — словами протокола.
///
/// `Opening/Buffering/Scrubbing/Seeking` — «хочу играть, жду данных» (муляж отдаёт
/// `waiting`); `Draining` — плеер доигрывает хвост, для страницы это игра;
/// `Failed` — плеер стоит (сам отказ странице сообщает `stream_rejected`).
pub(crate) const fn clock_state_for(state: PlaybackState) -> ClockState {
    match state {
        PlaybackState::Playing | PlaybackState::Draining => ClockState::Playing,
        PlaybackState::Paused
        | PlaybackState::Idle
        | PlaybackState::Stopped
        | PlaybackState::Failed => ClockState::Paused,
        PlaybackState::Opening
        | PlaybackState::Buffering
        | PlaybackState::Scrubbing
        | PlaybackState::Seeking => ClockState::Waiting,
        PlaybackState::Ended => ClockState::Ended,
    }
}

/// Позиция плеера → время протокола (целые мкс, не больше 2⁵³ − 1).
pub(crate) fn clock_position(position: Duration) -> MediaTime {
    let micros = u64::try_from(position.as_micros())
        .unwrap_or(u64::MAX)
        .min(MAX_JSON_SAFE_INTEGER);
    // После `min` значение всегда в пределах; ветка ошибки — только защита.
    MediaTime::from_micros(micros).unwrap_or(MediaTime::ZERO)
}

/// Скорость плеера → тысячные доли протокола в пределах, которые принимают браузеры.
pub(crate) fn clock_rate(rate: PlayerPlaybackRate) -> PagePlaybackRate {
    // `as` у f32 → u32 насыщается (NaN → 0), а потом значение зажимается в диапазон.
    let permille = ((rate.as_f32() * 1000.0).round() as u32)
        .clamp(MIN_PLAYBACK_RATE_PERMILLE, MAX_PLAYBACK_RATE_PERMILLE);
    PagePlaybackRate::from_permille(permille).unwrap_or(PagePlaybackRate::NORMAL)
}

/// Ритм тиков часов: первый — сразу, дальше — не чаще [`CLOCK_INTERVAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ClockCadence {
    /// Когда пора следующему тику (`None` — сразу).
    next_at: Option<Instant>,
}

impl ClockCadence {
    /// Пора ли тикать сейчас; если да — следующий тик назначается через шаг.
    pub(crate) fn take_due(&mut self, now: Instant) -> bool {
        if self.next_at.is_some_and(|next_at| now < next_at) {
            return false;
        }
        self.next_at = Some(now + CLOCK_INTERVAL);
        true
    }

    /// Когда следующий тик (`None` — при первой возможности).
    pub(crate) const fn next_at(&self) -> Option<Instant> {
        self.next_at
    }
}

/// Последнее состояние буфера, которое ушло странице.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReportedBuffer {
    /// Поколение.
    generation: StreamGeneration,
    /// Запас вперёд.
    ahead: Duration,
    /// Принимал ли источник.
    intake: BufferIntake,
    /// Когда ушло.
    at: Instant,
}

/// Правило «пора ли сообщить странице состояние буфера».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BufferReportPolicy {
    /// Последнее отправленное.
    last_reported: Option<ReportedBuffer>,
}

impl BufferReportPolicy {
    /// Сообщать ли: первый раз в поколении, при смене «принимаю/полон», при сдвиге
    /// запаса на [`BUFFER_AHEAD_REPORT_STEP`] и не реже [`BUFFER_HEARTBEAT`].
    pub(crate) fn should_report(
        &self,
        generation: StreamGeneration,
        status: BufferStatus,
        now: Instant,
    ) -> bool {
        let Some(last) = self.last_reported else {
            return true;
        };
        last.generation != generation
            || last.intake != status.intake
            || last.ahead.abs_diff(status.buffered_ahead) >= BUFFER_AHEAD_REPORT_STEP
            || now.saturating_duration_since(last.at) >= BUFFER_HEARTBEAT
    }

    /// Запомнить, что это состояние ушло странице.
    fn note_reported(&mut self, generation: StreamGeneration, status: BufferStatus, at: Instant) {
        self.last_reported = Some(ReportedBuffer {
            generation,
            ahead: status.buffered_ahead,
            intake: status.intake,
            at,
        });
    }
}

/// Источник → слово протокола.
const fn page_intake(intake: BufferIntake) -> PageBufferIntake {
    match intake {
        BufferIntake::Accepting => PageBufferIntake::Accepting,
        BufferIntake::Full => PageBufferIntake::Full,
    }
}

// ---- очередь и отправка --------------------------------------------------------

/// Что вышло с запросами байтов (для итоговой строки журнала: только числа).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RangeRequestCounters {
    /// Запросов `fetch_range`, ушедших странице.
    pub(crate) sent: u64,
    /// Сколько байтов просили этими запросами.
    pub(crate) bytes_requested: u64,
    /// Запросов, в которых очередь отказала из-за переполнения [`MAX_PENDING_RANGE_REQUESTS`].
    pub(crate) refused_queue_full: u64,
    /// Запросов, которые нельзя записать в протоколе (смещение больше 2⁵³): дефект источника.
    pub(crate) unencodable: u64,
}

/// Что вышло с запросом байтов, поставленным в очередь к странице.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a refused request must be handled: the file would wait for bytes nobody asked for"]
pub(crate) enum RangeQueueOutcome {
    /// Поставлен: уйдёт странице, как только она сможет принять.
    Queued,
    /// Не поставлен: очередь неотправленных запросов переполнена (внутренняя ошибка).
    QueueFull,
    /// Не поставлен: смещение или длина не записываются протоколом (дефект источника).
    NotEncodable,
}

/// Состояние буфера, ждущее отправки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingBuffer {
    /// Поколение.
    generation: StreamGeneration,
    /// Состояние.
    status: BufferStatus,
}

/// Исходящие сообщения странице одного сеанса.
pub(crate) struct PageOutbound {
    /// Куда отправлять.
    sender: Box<dyn PageSender>,
    /// Ритм часов.
    clock: ClockCadence,
    /// Номер следующего тика часов (по кругу).
    next_clock_sequence: u32,
    /// Номер следующей просьбы перемотаться (растёт, начинается с 1).
    next_seek_id: u32,
    /// Правило «когда сообщать буфер».
    buffer_policy: BufferReportPolicy,
    /// Свежее состояние буфера, которое ещё не ушло.
    pending_buffer: Option<PendingBuffer>,
    /// `seek` и `stream_rejected` в порядке появления, ещё не ушедшие; не больше
    /// одного каждого вида (см. [`PageOutbound::queue_event_replacing_same_kind`]).
    pending_events: VecDeque<PlayerSessionMessage>,
    /// Последнее поколение, от которого уже отказались (поколения растут: отказ
    /// от того же или более старого — повтор, его не шлём).
    last_rejected: Option<StreamGeneration>,
    /// Счётчики запросов байтов.
    range_counters: RangeRequestCounters,
}

impl PageOutbound {
    /// Пустая очередь с отправкой через `sender`.
    pub(crate) fn new(sender: Box<dyn PageSender>) -> Self {
        Self {
            sender,
            clock: ClockCadence::default(),
            next_clock_sequence: 0,
            next_seek_id: 1,
            buffer_policy: BufferReportPolicy::default(),
            pending_buffer: None,
            pending_events: VecDeque::new(),
            last_rejected: None,
            range_counters: RangeRequestCounters::default(),
        }
    }

    /// Счётчики запросов байтов за жизнь процесса.
    pub(crate) const fn range_counters(&self) -> RangeRequestCounters {
        self.range_counters
    }

    /// Сеанс закончился: отложенное адресовано странице, которой больше не будет.
    ///
    /// Правила часов и буфера не трогаются: их сбросит следующий сеанс.
    pub(crate) fn forget_pending_of_ended_session(&mut self) {
        self.pending_buffer = None;
        self.pending_events.clear();
    }

    /// Новый сеанс: прежние отложенные сообщения адресованы старой странице.
    ///
    /// Номера тиков и перемоток не сбрасываются: так они не повторяются никогда.
    pub(crate) fn reset_for_new_session(&mut self) {
        self.clock = ClockCadence::default();
        self.buffer_policy = BufferReportPolicy::default();
        self.pending_buffer = None;
        self.pending_events.clear();
        self.last_rejected = None;
    }

    /// Тик часов, если пора: уходит сразу, при полной очереди — выбрасывается.
    pub(crate) fn tick_clock(&mut self, observation: &PlaybackObservation, now: Instant) {
        if !self.clock.take_due(now) {
            return;
        }
        let message = PlayerSessionMessage::Clock {
            seq: ClockSequence(self.next_clock_sequence),
            position_us: clock_position(observation.page_clock_position()),
            state: clock_state_for(observation.state),
            rate: clock_rate(observation.rate),
        };
        self.next_clock_sequence = self.next_clock_sequence.wrapping_add(1);
        match self.deliver(&message) {
            Delivery::QueueFull => debug!("Clock tick dropped: the queue to the page is full"),
            Delivery::PageDetached => debug!("Clock tick dropped: the page is not attached"),
            Delivery::Sent | Delivery::Discarded => {}
        }
    }

    /// Когда следующий тик часов (`None` — при первой возможности).
    pub(crate) const fn next_clock_at(&self) -> Option<Instant> {
        self.clock.next_at()
    }

    /// Свежее состояние буфера поколения: ставится в отправку, если правило велит.
    pub(crate) fn note_buffer(
        &mut self,
        generation: StreamGeneration,
        status: BufferStatus,
        now: Instant,
    ) {
        // Отложенное значение заменяется свежим, даже если правило молчит: уйти
        // должно последнее известное состояние, а не устаревшее.
        if self.pending_buffer.is_some()
            || self.buffer_policy.should_report(generation, status, now)
        {
            self.pending_buffer = Some(PendingBuffer { generation, status });
        }
    }

    /// Источнику нужны байты с цели, которой нет на полке: просим страницу
    /// перемотаться. Номер просьбы — свежий, растущий.
    pub(crate) fn request_page_seek(&mut self, request: PageSeekRequest) -> SeekId {
        let seek_id = SeekId(self.next_seek_id);
        self.next_seek_id = self.next_seek_id.wrapping_add(1).max(1);
        // Неотправленная прежняя просьба устарела: источнику нужна новая цель.
        self.queue_event_replacing_same_kind(PlayerSessionMessage::Seek {
            seek_id,
            target_us: clock_position(request.target),
        });
        seek_id
    }

    /// Зритель нажал play: странице `play` (надёжно; заменяет неотправленный `pause`).
    pub(crate) fn request_page_play(&mut self) {
        self.queue_event_replacing_same_kind(PlayerSessionMessage::Play {});
    }

    /// Зритель нажал паузу: странице `pause` (надёжно; заменяет неотправленный `play`).
    pub(crate) fn request_page_pause(&mut self) {
        self.queue_event_replacing_same_kind(PlayerSessionMessage::Pause {});
    }

    /// Зритель сменил скорость: странице `set_rate` (плеер страницы на большей
    /// скорости качает быстрее). Надёжно; неотправленная прежняя скорость заменяется.
    pub(crate) fn request_page_rate(&mut self, rate: PagePlaybackRate) {
        self.queue_event_replacing_same_kind(PlayerSessionMessage::SetRate { rate });
    }

    /// Плеер не будет играть поколение: `stream_rejected` один раз на поколение.
    ///
    /// Возвращает `false`, если отказ для этого поколения уже был.
    pub(crate) fn reject_generation(
        &mut self,
        generation: StreamGeneration,
        reason: StreamRejection,
    ) -> bool {
        if self
            .last_rejected
            .is_some_and(|rejected| generation <= rejected)
        {
            return false;
        }
        self.last_rejected = Some(generation);
        // Неотправленный отказ от более старого поколения странице уже не нужен.
        self.queue_event_replacing_same_kind(PlayerSessionMessage::StreamRejected {
            generation,
            reason,
        });
        true
    }

    /// Источнику файла нужны байты: ставит `fetch_range` в отправку.
    ///
    /// Надёжно: не теряется при полной очереди и переподключении, с другими запросами
    /// не сливается (см. документацию модуля). Отказ — явный
    /// ([`RangeQueueOutcome`]), никогда не молчаливая потеря: владелец обязан его
    /// обработать, иначе файл ждал бы байтов, о которых страница не знает.
    pub(crate) fn request_range(&mut self, request: RangeRequest) -> RangeQueueOutcome {
        let (Ok(offset), Ok(length)) = (
            ByteOffset::new(request.offset),
            RangeFetchLength::new(u64::from(request.length.get())),
        ) else {
            // Источник просит больше, чем умеет записать протокол: просить нечем.
            self.range_counters.unencodable = self.range_counters.unencodable.saturating_add(1);
            return RangeQueueOutcome::NotEncodable;
        };
        let pending_ranges = self
            .pending_events
            .iter()
            .filter(|pending| matches!(pending, PlayerSessionMessage::FetchRange { .. }))
            .count();
        if pending_ranges >= MAX_PENDING_RANGE_REQUESTS {
            self.range_counters.refused_queue_full =
                self.range_counters.refused_queue_full.saturating_add(1);
            return RangeQueueOutcome::QueueFull;
        }
        self.pending_events
            .push_back(PlayerSessionMessage::FetchRange {
                request_id: PageRangeRequestId(request.id.get()),
                offset,
                length,
            });
        RangeQueueOutcome::Queued
    }

    /// Ставит событие в отправку, выбрасывая неотправленное событие того же места
    /// ([`replacement_slot`]): отложенных событий не больше, чем мест.
    fn queue_event_replacing_same_kind(&mut self, message: PlayerSessionMessage) {
        let slot = replacement_slot(&message);
        self.pending_events
            .retain(|pending| replacement_slot(pending) != slot);
        self.pending_events.push_back(message);
    }

    /// Есть ли отложенные события (тогда цикл должен проснуться для повтора).
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending_events.is_empty() || self.pending_buffer.is_some()
    }

    /// Отправляет отложенное: сначала события по порядку, потом буфер. Полная
    /// очередь останавливает отправку до следующего прохода (порядок не ломается).
    pub(crate) fn flush(&mut self, now: Instant) {
        while let Some(message) = self.pending_events.front() {
            match self.deliver(message) {
                Delivery::QueueFull | Delivery::PageDetached => return,
                delivery @ (Delivery::Sent | Delivery::Discarded) => {
                    if let Some(sent) = self.pending_events.pop_front()
                        && delivery == Delivery::Sent
                    {
                        self.count_sent_range_request(&sent);
                    }
                }
            }
        }
        let Some(pending) = self.pending_buffer else {
            return;
        };
        let message = PlayerSessionMessage::BufferState {
            generation: pending.generation,
            ahead_us: clock_position(pending.status.buffered_ahead),
            intake: page_intake(pending.status.intake),
        };
        match self.deliver(&message) {
            Delivery::QueueFull | Delivery::PageDetached => {}
            Delivery::Sent => {
                self.buffer_policy
                    .note_reported(pending.generation, pending.status, now);
                self.pending_buffer = None;
            }
            Delivery::Discarded => self.pending_buffer = None,
        }
    }

    /// Запрос байтов ушёл странице: в счётчики (остальные сообщения не считаются).
    fn count_sent_range_request(&mut self, sent: &PlayerSessionMessage) {
        if let PlayerSessionMessage::FetchRange { length, .. } = sent {
            self.range_counters.sent = self.range_counters.sent.saturating_add(1);
            self.range_counters.bytes_requested = self
                .range_counters
                .bytes_requested
                .saturating_add(length.as_bytes());
        }
    }

    /// Одна попытка отправки с разбором ошибки.
    fn deliver(&self, message: &PlayerSessionMessage) -> Delivery {
        match self.sender.send_to_page(message) {
            Ok(()) => Delivery::Sent,
            Err(SendToPageError::QueueFull) => Delivery::QueueFull,
            Err(SendToPageError::NoAttachedSession) => Delivery::PageDetached,
            Err(reason @ SendToPageError::ShuttingDown) => {
                debug!(
                    %reason,
                    kind = message_kind(message),
                    "Message to the page dropped"
                );
                Delivery::Discarded
            }
            Err(reason @ SendToPageError::Unencodable(_)) => {
                // Наше сообщение не прошло проверку протокола — дефект отправителя.
                error!(
                    %reason,
                    kind = message_kind(message),
                    "Message to the page is not valid"
                );
                Delivery::Discarded
            }
        }
    }
}

/// Место неотправленного события: новое событие того же места заменяет старое.
///
/// У `play` и `pause` место общее: обе — воля «играть/пауза», и уйти должна последняя.
/// У остальных место — их вид.
const fn replacement_slot(message: &PlayerSessionMessage) -> &'static str {
    match message {
        PlayerSessionMessage::Play {} | PlayerSessionMessage::Pause {} => "play_state",
        _ => message_kind(message),
    }
}

/// Вид сообщения для журнала (без значений).
const fn message_kind(message: &PlayerSessionMessage) -> &'static str {
    match message {
        PlayerSessionMessage::Attached {} => "attached",
        PlayerSessionMessage::Clock { .. } => "clock",
        PlayerSessionMessage::BufferState { .. } => "buffer_state",
        PlayerSessionMessage::Play {} => "play",
        PlayerSessionMessage::Pause {} => "pause",
        PlayerSessionMessage::Seek { .. } => "seek",
        PlayerSessionMessage::SetRate { .. } => "set_rate",
        PlayerSessionMessage::ReturnToPage { .. } => "return_to_page",
        PlayerSessionMessage::SelectVariant { .. } => "select_variant",
        PlayerSessionMessage::NextEpisode {} => "next_episode",
        PlayerSessionMessage::FetchRange { .. } => "fetch_range",
        PlayerSessionMessage::StreamRejected { .. } => "stream_rejected",
    }
}

#[cfg(test)]
mod tests;
