//! Медиа-часть цикла с подделками плеера, страницы и закрытия сеанса; потоки
//! источника — настоящие (`pushed-media-source`) на файлах фикстур протокола.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use browser_bridge_protocol::session::player::ClockState;
use browser_bridge_protocol::{MediaTime, PlayerSessionMessage, SessionKey};
use browser_bridge_session::{SendToPageError, SessionInstance};
use player_core::PlaybackState;
use pushed_media_source::{
    LaneId, PushedContainer, PushedSourcePolicy, PushedStreamWriter, pushed_stream,
};

use super::*;
use crate::browser_session::outbound::PageSender;
use crate::browser_session::playback::{LoadFault, MediaLoadTicketIssuer};

/// Потолок ожидания открытия настоящего потока.
const WAIT: Duration = Duration::from_secs(20);

// ---- подделки ----------------------------------------------------------------

/// Что плеер-подделка получил.
#[derive(Debug, Clone, PartialEq)]
enum PlayerCall {
    /// Загрузка: метка источника и как она начинается.
    Load(String, LoadStart),
    Play,
    Pause,
    SetRate(f32),
    Seek(Duration),
}

/// Плеер-подделка: записывает команды, на шаг отвечает заданным наблюдением.
/// Загрузка выдаёт настоящую квитанцию; «установку» тест изображает сам —
/// [`FakePlayback::show_load`].
#[derive(Clone)]
struct FakePlayback {
    calls: Rc<RefCell<Vec<PlayerCall>>>,
    observation: Rc<RefCell<PlaybackObservation>>,
    tickets: Rc<RefCell<MediaLoadTicketIssuer>>,
    loaded_tickets: Rc<RefCell<Vec<MediaLoadTicket>>>,
}

impl FakePlayback {
    fn new() -> Self {
        Self {
            calls: Rc::default(),
            observation: Rc::new(RefCell::new(PlaybackObservation {
                position: Duration::ZERO,
                duration: None,
                state: PlaybackState::Idle,
                rate: PlayerPlaybackRate::NORMAL,
                buffering: false,
                ended: false,
                fault: None,
                shown_load: None,
            })),
            tickets: Rc::default(),
            loaded_tickets: Rc::default(),
        }
    }

    /// Квитанция `index`-й загрузки (с нуля).
    fn load_ticket(&self, index: usize) -> MediaLoadTicket {
        self.loaded_tickets.borrow()[index]
    }

    /// Плеер «поставил» медиа загрузки: наблюдения теперь про неё.
    fn show_load(&self, ticket: MediaLoadTicket, position: Duration, state: PlaybackState) {
        let mut observation = self.observation.borrow_mut();
        observation.shown_load = Some(ticket);
        observation.position = position;
        observation.state = state;
    }

    fn take_calls(&self) -> Vec<PlayerCall> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }

    fn record(&self, call: PlayerCall) -> Result<(), BrowserPlaybackCommandError> {
        self.calls.borrow_mut().push(call);
        Ok(())
    }
}

impl MediaPlayback for FakePlayback {
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError> {
        self.record(PlayerCall::Load(prepared.source_label(), start))?;
        let ticket = self.tickets.borrow_mut().issue();
        self.loaded_tickets.borrow_mut().push(ticket);
        Ok(ticket)
    }

    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        self.record(PlayerCall::Play)
    }

    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        self.record(PlayerCall::Pause)
    }

    fn set_rate(&mut self, rate: PlayerPlaybackRate) -> Result<(), BrowserPlaybackCommandError> {
        self.record(PlayerCall::SetRate(rate.as_f32()))
    }

    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError> {
        self.record(PlayerCall::Seek(target))
    }

    fn pump(&mut self, _now: Instant) -> PlaybackObservation {
        self.observation.borrow().clone()
    }

    fn finish(self: Box<Self>) {}
}

/// Страница-подделка: запоминает поставленные сообщения.
#[derive(Clone, Default)]
struct FakePage {
    sent: Rc<RefCell<Vec<PlayerSessionMessage>>>,
    refusals: Rc<RefCell<VecDeque<SendToPageError>>>,
}

impl FakePage {
    fn take_sent(&self) -> Vec<PlayerSessionMessage> {
        std::mem::take(&mut *self.sent.borrow_mut())
    }
}

impl PageSender for FakePage {
    fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
        if let Some(reason) = self.refusals.borrow_mut().pop_front() {
            return Err(reason);
        }
        self.sent.borrow_mut().push(message.clone());
        Ok(())
    }
}

/// Закрытие-подделка: запоминает, какой экземпляр просили закрыть.
#[derive(Clone, Default)]
struct FakeCloser {
    closed: Rc<RefCell<Vec<(SessionRef, SessionCloseReason)>>>,
}

impl SessionCloser for FakeCloser {
    fn close_session(&self, session: &SessionRef, reason: SessionCloseReason) -> CloseOutcome {
        self.closed.borrow_mut().push((session.clone(), reason));
        CloseOutcome::Closed
    }
}

struct Harness {
    media: SessionMedia,
    player: FakePlayback,
    page: FakePage,
    closer: FakeCloser,
}

impl Harness {
    fn new() -> Self {
        let player = FakePlayback::new();
        let page = FakePage::default();
        let closer = FakeCloser::default();
        let media = SessionMedia::new(
            Box::new(player.clone()),
            Box::new(closer.clone()),
            PageOutbound::new(Box::new(page.clone())),
        );
        Self {
            media,
            player,
            page,
            closer,
        }
    }

    /// Крутит медиа-часть, пока плеер не получит загрузку (или до потолка).
    fn drive_until_loaded(&mut self) -> Vec<PlayerCall> {
        let deadline = Instant::now() + WAIT;
        loop {
            let now = Instant::now();
            let wake_at = self.media.drive(now);
            let calls = self.player.take_calls();
            if calls
                .iter()
                .any(|call| matches!(call, PlayerCall::Load(..)))
            {
                return calls;
            }
            assert!(now < deadline, "the stream was never loaded");
            let pause = wake_at.map_or(Duration::from_millis(5), |wake_at| {
                wake_at.saturating_duration_since(now)
            });
            std::thread::sleep(pause.max(Duration::from_millis(1)));
        }
    }
}

// ---- помощники ---------------------------------------------------------------

fn session(instance: u64) -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab-1-video-1").expect("valid key"),
        instance: SessionInstance(instance),
    }
}

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("generation")
}

fn fixture_media(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../browser-bridge-protocol/fixtures/media")
        .join(name);
    std::fs::read(path).expect("fixture media")
}

/// Настоящий поток поколения: писатель (держит тест) и уведомление главному циклу.
/// `with_media` — сразу подать целый ролик фикстуры (видео + звук) и конец потока.
fn stream_generation(
    session: &SessionRef,
    number: u32,
    with_media: bool,
) -> (PushedStreamWriter, IntakeNotice) {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, opener) = pushed_stream(policy).expect("stream starts");
    if with_media {
        for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
            let lane = LaneId::new(lane);
            writer
                .add_lane(lane, PushedContainer::FragmentedMp4)
                .expect("lane");
            writer.append(lane, &fixture_media(file)).expect("append");
        }
        writer.end_of_stream().expect("end of stream");
    }
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: session.clone(),
        generation: generation(number),
        monitor: writer.monitor(),
        opener,
    }));
    (writer, notice)
}

fn intent(session: &SessionRef, intent: PageIntent) -> IntakeNotice {
    IntakeNotice::PageIntent {
        session: session.clone(),
        intent,
    }
}

fn buffer_generations(messages: &[PlayerSessionMessage]) -> Vec<u32> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::BufferState { generation, .. } => Some(generation.get()),
            _ => None,
        })
        .collect()
}

/// Сеанс 1 с загруженным и уже поставленным плеером поколением 1; писатель
/// держится тестом.
fn loaded_harness() -> (Harness, SessionRef, PushedStreamWriter) {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (writer, notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, Duration::ZERO, PlaybackState::Opening);
    harness.page.take_sent();
    (harness, current, writer)
}

fn clocks(messages: &[PlayerSessionMessage]) -> Vec<(u64, ClockState)> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us, state, ..
            } => Some((position_us.as_micros(), *state)),
            _ => None,
        })
        .collect()
}

// ---- открытие и поколения ------------------------------------------------------

#[test]
fn a_newer_generation_supersedes_the_pending_one_and_an_older_one_never_replaces_it() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    // Оба поколения целиком готовы к открытию: открыто и загружено должно быть
    // только новое.
    let (_old_writer, old) = stream_generation(&current, 1, true);
    let (_new_writer, new) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![old, new], None);

    let calls = harness.drive_until_loaded();
    assert_eq!(
        calls,
        vec![PlayerCall::Load(
            BROWSER_STREAM_LABEL.to_owned(),
            LoadStart::Playing
        )]
    );
    assert_eq!(buffer_generations(&harness.page.take_sent()), vec![2]);

    // Запоздавшее старое поколение после загрузки нового — выброшено.
    let (_late_writer, late) = stream_generation(&current, 1, true);
    harness.media.on_intake(vec![late], None);
    for _ in 0..20 {
        harness.media.drive(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(harness.player.take_calls().is_empty());
}

#[test]
fn generation_of_a_replaced_session_is_not_loaded_and_the_old_video_is_paused() {
    let (mut harness, old_session, _writer) = loaded_harness();
    let new_session = session(2);
    harness.media.on_session_opened(&new_session);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);
    // Повтор того же экземпляра ничего не трогает.
    harness.media.on_session_opened(&new_session);
    assert!(harness.player.take_calls().is_empty());

    let (_late_writer, late) = stream_generation(&old_session, 2, true);
    harness.media.on_intake(
        vec![late, intent(&old_session, PageIntent::Pause)],
        Some(old_session.clone()),
    );
    for _ in 0..20 {
        harness.media.drive(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(harness.player.take_calls().is_empty());
    // Без загруженного поколения странице не уходят ни часы, ни буфер.
    assert!(harness.page.take_sent().is_empty());
}

// ---- намерения страницы ----------------------------------------------------------

#[test]
fn page_seek_moves_the_player_and_then_restores_the_page_play_state() {
    let (mut harness, current, _writer) = loaded_harness();
    let target = MediaTime::from_micros(250_000).expect("time");

    harness.media.on_intake(
        vec![intent(&current, PageIntent::FollowPageSeek(target))],
        None,
    );
    // Страница играет (автозапуск): после перемотки — play (перемотка после конца
    // в плеере кончается паузой).
    assert_eq!(
        harness.player.take_calls(),
        vec![
            PlayerCall::Seek(Duration::from_millis(250)),
            PlayerCall::Play
        ]
    );

    harness.media.on_intake(
        vec![
            intent(&current, PageIntent::Pause),
            intent(&current, PageIntent::FollowPageSeek(MediaTime::ZERO)),
            intent(
                &current,
                PageIntent::SetRate(PagePlaybackRate::from_permille(16_000).expect("rate")),
            ),
        ],
        None,
    );
    assert_eq!(
        harness.player.take_calls(),
        vec![
            PlayerCall::Pause,
            PlayerCall::Seek(Duration::ZERO),
            PlayerCall::Pause,
            // 16× страницы — ближняя граница плеера.
            PlayerCall::SetRate(4.0),
        ]
    );
}

/// Страница на паузе к моменту загрузки: медиа загружается сразу на паузе (а не
/// «играть, потом пауза» — звук мелькнул бы), отдельной паузы следом нет.
#[test]
fn page_pause_before_the_stream_opens_loads_the_stream_paused() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(
        vec![
            intent(&current, PageIntent::FollowPageSeek(MediaTime::ZERO)),
            intent(&current, PageIntent::Pause),
            notice,
        ],
        None,
    );
    // До загрузки плееру командовать нечем.
    assert!(harness.player.take_calls().is_empty());
    assert_eq!(
        harness.drive_until_loaded(),
        vec![PlayerCall::Load(
            BROWSER_STREAM_LABEL.to_owned(),
            LoadStart::Paused
        )]
    );
}

// ---- отказы и закрытие ---------------------------------------------------------

#[test]
fn close_requests_close_exactly_the_instance_they_name() {
    let mut harness = Harness::new();
    let old_session = session(1);
    let current = session(2);
    harness.media.on_session_opened(&current);
    harness.media.on_intake(
        vec![
            IntakeNotice::CloseSession {
                session: old_session.clone(),
                reason: SessionCloseReason::ProtocolViolation,
            },
            IntakeNotice::CloseSession {
                session: current.clone(),
                reason: SessionCloseReason::ProtocolViolation,
            },
        ],
        None,
    );
    assert_eq!(
        *harness.closer.closed.borrow(),
        vec![
            (old_session, SessionCloseReason::ProtocolViolation),
            (current, SessionCloseReason::ProtocolViolation),
        ]
    );
}

#[test]
fn rejection_reaches_the_page_once_and_cancels_the_pending_open() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, notice) = stream_generation(&current, 1, true);
    let reject = || IntakeNotice::Reject {
        session: current.clone(),
        generation: generation(1),
        reason: StreamRejection::ProtectedContent,
    };
    harness
        .media
        .on_intake(vec![notice, reject(), reject()], None);
    for _ in 0..20 {
        harness.media.drive(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        harness.player.take_calls().is_empty(),
        "rejected stream loaded"
    );
    assert_eq!(
        harness.page.take_sent(),
        vec![PlayerSessionMessage::StreamRejected {
            generation: generation(1),
            reason: StreamRejection::ProtectedContent,
        }]
    );
}

#[test]
fn unsupported_video_in_the_player_rejects_the_loaded_generation_once() {
    let (mut harness, _current, _writer) = loaded_harness();
    let first_load = harness.player.load_ticket(0);
    harness.player.observation.borrow_mut().fault = Some(LoadFault {
        load: first_load,
        fault: BrowserPlaybackFault::UnsupportedVideo,
    });
    let start = Instant::now();
    harness.media.drive(start);
    harness.media.drive(start + Duration::from_secs(1));
    let rejections: Vec<PlayerSessionMessage> = harness
        .page
        .take_sent()
        .into_iter()
        .filter(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. }))
        .collect();
    assert_eq!(
        rejections,
        vec![PlayerSessionMessage::StreamRejected {
            generation: generation(1),
            reason: StreamRejection::UnsupportedCodec,
        }]
    );
}

// ---- часы и пробуждение --------------------------------------------------------

#[test]
fn loaded_generation_ticks_the_page_clock_and_asks_to_wake_for_the_next_tick() {
    let (mut harness, _current, _writer) = loaded_harness();
    {
        let mut observation = harness.player.observation.borrow_mut();
        observation.state = PlaybackState::Playing;
        observation.position = Duration::from_millis(120);
    }
    let start = Instant::now() + Duration::from_secs(1);
    let wake_at = harness.media.drive(start);
    assert_eq!(
        clocks(&harness.page.take_sent()),
        vec![(120_000, ClockState::Playing)]
    );
    assert_eq!(
        wake_at,
        Some(start + crate::browser_session::outbound::CLOCK_INTERVAL)
    );
}

/// Плеер ставит новое медиа асинхронно: пока наблюдение ещё про прежнее медиа
/// (поколение 1 на 30 с), позиция не уходит монитору поколения 2 и часы страницы
/// не приписывают ему старую позицию; как только плеер показывает медиа загрузки
/// поколения 2 — позиция и часы идут.
#[test]
fn position_of_the_previous_media_is_never_attributed_to_an_installing_generation() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, Duration::from_secs(30), PlaybackState::Playing);

    let (second_writer, second) = stream_generation(&current, 2, true);
    let second_monitor = second_writer.monitor();
    harness.media.on_intake(vec![second], None);
    harness.drive_until_loaded();
    harness.page.take_sent();
    // Без позиции источник считает от начала первого поданного сегмента: весь
    // ролик фикстуры (0,4 с) — запас вперёд.
    let ahead_while_installing = second_monitor.buffer_status().buffered_ahead;
    assert!(
        ahead_while_installing > Duration::from_millis(300),
        "the installing generation's monitor got a foreign position: {ahead_while_installing:?}"
    );

    // Плеер всё ещё показывает поколение 1 на 30,x с — несколько тиков часов.
    let start = Instant::now() + Duration::from_secs(1);
    for step in 0..3_u32 {
        harness.player.observation.borrow_mut().position =
            Duration::from_millis(30_100 + u64::from(step) * 250);
        harness
            .media
            .drive(start + Duration::from_secs(u64::from(step)));
    }
    let sent = harness.page.take_sent();
    assert_eq!(
        clocks(&sent),
        vec![(0, ClockState::Waiting); 3],
        "while installing the page sees `waiting` at 0, not the old media's position"
    );
    assert!(buffer_generations(&sent).iter().all(|number| *number == 2));
    assert_eq!(
        second_monitor.buffer_status().buffered_ahead,
        ahead_while_installing,
        "no position report reached the installing generation's monitor"
    );

    // Плеер поставил медиа поколения 2: позиция и часы — его.
    let second_load = harness.player.load_ticket(1);
    harness.player.show_load(
        second_load,
        Duration::from_millis(40),
        PlaybackState::Playing,
    );
    harness.media.drive(start + Duration::from_secs(10));
    assert_eq!(
        clocks(&harness.page.take_sent()),
        vec![(40_000, ClockState::Playing)]
    );
    let ahead_after_install = second_monitor.buffer_status().buffered_ahead;
    assert!(
        ahead_after_install < ahead_while_installing && ahead_after_install > Duration::ZERO,
        "the monitor counts from the reported position: {ahead_after_install:?} vs {ahead_while_installing:?}"
    );
}

#[test]
fn without_a_session_the_media_part_does_not_ask_to_wake() {
    let mut harness = Harness::new();
    assert_eq!(harness.media.drive(Instant::now()), None);
    assert!(harness.page.take_sent().is_empty());
}

mod generation_switch;
