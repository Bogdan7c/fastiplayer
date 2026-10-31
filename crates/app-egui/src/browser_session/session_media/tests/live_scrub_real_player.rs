//! Живое перетаскивание окна браузерного режима на НАСТОЯЩЕМ плеере («кадры бегут по
//! полке», решение владельца 31.10.2026).
//!
//! Что настоящее: медиа-часть сеанса, плеер (`BrowserPlayback` — рабочий поток
//! `PlayerWorker`, программный FFmpeg, тихая карта звука реального времени, безоконный
//! вывод), поток `pushed-media-source` на фикстурах протокола и его ответ «что
//! скачано» (`DownloadedRange`, который медиа-часть отдаёт плееру окна). Подделана
//! только страница: видно каждое сообщение, которое ушло бы ей по мосту.
//!
//! Чего здесь НЕТ — `AppState` и жеста мыши: окна в этом харнессе нет. Тест шлёт
//! плееру ровно ту последовательность команд, которую для такого жеста выдаёт цепочка
//! окна «жест → план → диспетчер» (она закреплена своими тестами без плеера:
//! `state::timeline_dispatch::tests`): команды скраба — плееру напрямую (как
//! `send_transport_command` окна), отпускание вне скачанного — перемоткой зрителя
//! через медиа-часть (как `ViewerTransportIntent::Seek`). Какие цели лежат на
//! скачанном, тест спрашивает у того же `DownloadedRange`, что получил бы таймлайн.
//!
//! Проверяется то, что увидят зритель и страница: кадр у цели на скачанном; за жест
//! странице ни одного `seek`/`play`/`pause`/`set_rate`; после отпускания вне скачанного
//! — ровно один `seek`, и после ответа страницы плеер играет у цели; отпускание на
//! скачанном — странице ничего, плеер стоит в точке отпускания (на паузе, как до жеста).

use std::sync::Arc;

use player_core::{PlayerCommand, ScrubCommitPolicy, SeekRequest};
use pushed_media_source::TimestampOffset;

use super::*;
use crate::browser_session::inbox::BrowserSessionWake;
use crate::browser_session::journal::PlaybackSummary;
use crate::browser_session::playback::metered_audio::BrowserAudioOutputChoice;
use crate::browser_session::playback::video_output::HeadlessVideoOutput;
use crate::browser_session::playback::{BrowserPlayback, decoder_thread_config_from_settings};
use crate::browser_session::session_config::BrowserSessionSettings;
use crate::downloaded_range::DownloadedRange;
use crate::ui::timeline::{
    LiveScrubDiagnostics, TimelineLiveScrubDecodeMode, TimelineLiveScrubSettingsSnapshot,
    TimelineUiState,
};
use crate::viewer_transport::ViewerTransportIntent;

/// Объявленная длина потока: данных только 400 мс, остальное «у страницы».
const DECLARED_DURATION: Duration = Duration::from_secs(10);
/// Шаг главного цикла теста.
const STEP: Duration = Duration::from_millis(5);
/// Потолок ожидания одного шага сценария.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(20);
/// Насколько кадр/позиция может отстоять от цели (кадры фикстуры — по 40 мс).
const POSITION_TOLERANCE: Duration = Duration::from_millis(60);
/// Цели на скачанном (0–400 мс) и вне его.
const ON_SHELF_FIRST: Duration = Duration::from_millis(100);
const ON_SHELF_RELEASE: Duration = Duration::from_millis(300);
const OFF_SHELF: Duration = Duration::from_secs(5);

/// Будильник без цикла: тест крутит медиа-часть сам.
struct NoWake;

impl BrowserSessionWake for NoWake {
    fn wake(&self) {}
}

/// Плеер для медиа-части: настоящий `BrowserPlayback`, общий с тестом (тест шлёт ему
/// команды скраба, как плееру окна), и последнее сказанное о скачанном.
struct SharedPlayback {
    player: Rc<RefCell<BrowserPlayback>>,
    offered: Rc<RefCell<DownloadedRange>>,
}

impl MediaPlayback for SharedPlayback {
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError> {
        self.player.borrow_mut().load(prepared, start)
    }

    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        self.player.borrow().play()
    }

    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        self.player.borrow().pause()
    }

    fn set_rate(&mut self, rate: PlayerPlaybackRate) -> Result<(), BrowserPlaybackCommandError> {
        self.player.borrow().set_rate(rate)
    }

    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError> {
        self.player.borrow_mut().seek(target)
    }

    fn offer_downloaded_range(&mut self, range: DownloadedRange) {
        *self.offered.borrow_mut() = range;
    }

    fn pump(&mut self, now: Instant) -> PlaybackObservation {
        self.player.borrow_mut().pump(now)
    }

    fn finish(self: Box<Self>) {}
}

/// Медиа-часть на настоящем плеере + «окно», которое шлёт плееру команды скраба.
struct ScrubChain {
    media: SessionMedia,
    page: FakePage,
    player: Rc<RefCell<BrowserPlayback>>,
    offered: Rc<RefCell<DownloadedRange>>,
    /// Диспетчер жеста окна: даёт диагностику для команд скраба.
    timeline: TimelineUiState,
    /// Всё, что получила страница, по порядку.
    seen: Vec<PlayerSessionMessage>,
}

impl ScrubChain {
    fn start() -> Self {
        let mut config = fastiplayer_config::AppConfig::default();
        // Как у тестов плеера: фиксированные потоки FFmpeg, одинаково на любой машине.
        config.video.sw_decode_threads = 2;
        let settings = BrowserSessionSettings::from_app_config_for_test(config);
        let playback = BrowserPlayback::start(
            &settings,
            Box::new(HeadlessVideoOutput::new(
                decoder_thread_config_from_settings(&settings),
            )),
            BrowserAudioOutputChoice::SimulatedRealTime,
            Arc::new(NoWake),
            Instant::now(),
        )
        .expect("browser playback starts");
        let player = Rc::new(RefCell::new(playback));
        let offered = Rc::new(RefCell::new(DownloadedRange::Unknown));
        let page = FakePage::default();
        let media = SessionMedia::new(
            Box::new(SharedPlayback {
                player: Rc::clone(&player),
                offered: Rc::clone(&offered),
            }),
            Box::new(FakeCloser::default()),
            PageOutbound::new(Box::new(page.clone())),
        );
        Self {
            media,
            page,
            player,
            offered,
            timeline: TimelineUiState::default(),
            seen: Vec::new(),
        }
    }

    fn pass(&mut self) {
        self.media.drive(Instant::now());
        self.seen.extend(self.page.take_sent());
    }

    fn summary(&self) -> PlaybackSummary {
        self.player.borrow().summary()
    }

    /// Крутит цикл, пока плеер не придёт в нужное (или до потолка).
    fn drive_until(&mut self, what: &str, done: impl Fn(&PlaybackSummary) -> bool) {
        let deadline = Instant::now() + SCENARIO_DEADLINE;
        loop {
            self.pass();
            let summary = self.summary();
            if done(&summary) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?} at {:?}",
                summary.state,
                summary.position
            );
            std::thread::sleep(STEP);
        }
    }

    /// Время показа кадра на экране плеера.
    fn shown_frame_pts(&self) -> Option<Duration> {
        self.player.borrow_mut().shown_frame_pts_for_test()
    }

    /// Крутит цикл, пока на экране не окажется кадр у цели.
    fn drive_until_frame_near(&mut self, what: &str, target: Duration) {
        let deadline = Instant::now() + SCENARIO_DEADLINE;
        loop {
            self.pass();
            let shown = self.shown_frame_pts();
            if shown.is_some_and(|pts| near(pts, target)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: the frame on screen is at {shown:?}"
            );
            std::thread::sleep(STEP);
        }
    }

    fn drive_for(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.pass();
            std::thread::sleep(STEP);
        }
    }

    /// Что окно знает о скачанном сейчас.
    fn downloaded(&self) -> DownloadedRange {
        self.offered.borrow().clone()
    }

    fn send_to_player(&self, command: PlayerCommand) {
        let sent: Result<(), BrowserPlaybackCommandError> =
            self.player.borrow().send_command_for_test(command);
        sent.expect("the player accepts the scrub command");
    }

    /// «Начать» и первая цель — как шаг `BeginLiveScrub` окна.
    fn begin_scrub_at(&mut self, target: Duration) -> LiveScrubDiagnostics {
        let diagnostics = self.timeline.begin_live_scrub_dispatch(
            TimelineLiveScrubSettingsSnapshot {
                decode_mode: TimelineLiveScrubDecodeMode::EveryDragEvent,
                max_hz: 60,
            },
            Instant::now(),
            media_core::MediaTime::from_duration(target),
        );
        self.send_to_player(PlayerCommand::begin_live_scrub(diagnostics));
        self.preview_at(target, diagnostics);
        diagnostics
    }

    fn preview_at(&self, target: Duration, diagnostics: LiveScrubDiagnostics) {
        self.send_to_player(PlayerCommand::preview_live_scrub(
            SeekRequest::accurate(target),
            diagnostics,
        ));
    }

    fn end_scrub(&self, policy: ScrubCommitPolicy, diagnostics: LiveScrubDiagnostics) {
        self.send_to_player(PlayerCommand::end_live_scrub(policy, diagnostics));
    }
}

/// Поток поколения: ролик фикстуры (0–400 мс), длина объявлена 10 с, конца нет.
fn partially_delivered_generation(session: &SessionRef) -> (PushedStreamWriter, IntakeNotice) {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, opener) = pushed_stream(policy).expect("stream starts");
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
    writer.set_duration(PushedDuration::Finite(DECLARED_DURATION));
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: session.clone(),
        generation: generation(1),
        source: GenerationSource::Segments {
            monitor: writer.monitor(),
            opener,
        },
    }));
    (writer, notice)
}

/// Страница «отвечает» на перемотку: тот же ролик, уложенный с `at`.
fn page_delivers_media_at(writer: &mut PushedStreamWriter, at: Duration) {
    let offset_us = i64::try_from(at.as_micros()).expect("small offset");
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .set_timestamp_offset(lane, TimestampOffset::from_micros(offset_us))
            .expect("offset");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
}

/// Команды транспорта, которые получила страница (часы и состояние буфера — не они).
fn transport_commands(messages: &[PlayerSessionMessage]) -> Vec<&PlayerSessionMessage> {
    messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                PlayerSessionMessage::Seek { .. }
                    | PlayerSessionMessage::Play {}
                    | PlayerSessionMessage::Pause {}
                    | PlayerSessionMessage::SetRate { .. }
            )
        })
        .collect()
}

fn seek_targets(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Seek { target_us, .. } => Some(target_us.as_micros()),
            _ => None,
        })
        .collect()
}

fn near(position: Duration, target: Duration) -> bool {
    position.abs_diff(target) <= POSITION_TOLERANCE
}

/// Сеанс открыт, поколение доиграло скачанное и ждёт байтов страницы у 400 мс.
fn chain_at_the_end_of_the_downloaded_media() -> (ScrubChain, PushedStreamWriter) {
    let mut chain = ScrubChain::start();
    let current = session(1);
    chain.media.on_session_opened(&current);
    let (writer, notice) = partially_delivered_generation(&current);
    chain.media.on_intake(vec![notice], None);
    // Плеер доиграл скачанное и ждёт данных (у фикстуры — около 0,3 с).
    chain.drive_until("the end of the downloaded media", |summary| {
        summary.state == PlaybackState::Buffering && summary.position >= Duration::from_millis(250)
    });
    (chain, writer)
}

#[test]
fn dragging_over_the_shelf_shows_frames_and_releasing_outside_asks_the_page_once() {
    let (mut chain, mut writer) = chain_at_the_end_of_the_downloaded_media();
    // Таймлайн окна видит полку: что скачано — источник отвечает по своим правилам.
    let downloaded = chain.downloaded();
    assert!(downloaded.covers(ON_SHELF_FIRST), "{downloaded:?}");
    assert!(!downloaded.covers(OFF_SHELF), "{downloaded:?}");
    let gesture_start = chain.seen.len();
    // Плеер стоит у конца скачанного: на экране кадр оттуда, а не у цели.
    assert!(
        chain
            .shown_frame_pts()
            .is_some_and(|pts| !near(pts, ON_SHELF_FIRST)),
        "{:?}",
        chain.shown_frame_pts()
    );

    // Нажали и потянули по полке: кадр у цели показан.
    let diagnostics = chain.begin_scrub_at(ON_SHELF_FIRST);
    chain.drive_until_frame_near("the on-shelf preview frame", ON_SHELF_FIRST);
    // Потянули за полку: окно цель плееру не шлёт (кадр стоит, бегунок едет).
    assert!(!chain.downloaded().covers(OFF_SHELF));
    chain.drive_for(Duration::from_millis(300));
    assert!(
        transport_commands(&chain.seen[gesture_start..]).is_empty(),
        "nothing reaches the page during the gesture: {:?}",
        transport_commands(&chain.seen[gesture_start..])
    );

    // Отпустили вне полки: скраб садится на показанное, и одна перемотка зрителя.
    let release = chain.seen.len();
    chain.end_scrub(ScrubCommitPolicy::CommitLatestTarget, diagnostics);
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(OFF_SHELF), Instant::now());
    chain.drive_for(Duration::from_secs(1));
    assert_eq!(
        seek_targets(&chain.seen[release..]),
        [u64::try_from(OFF_SHELF.as_micros()).expect("small")]
    );

    // Страница прислала байты с цели: плеер у цели и играет, как до жеста.
    page_delivers_media_at(&mut writer, OFF_SHELF);
    chain.drive_until("playback at the release target", |summary| {
        summary.state == PlaybackState::Playing
            && summary.position >= OFF_SHELF
            && summary.position <= OFF_SHELF + Duration::from_millis(400)
    });
    let after_gesture = transport_commands(&chain.seen[gesture_start..]);
    assert_eq!(
        after_gesture.len(),
        1,
        "only the one seek reached the page: {after_gesture:?}"
    );
}

#[test]
fn releasing_on_the_shelf_lands_there_and_tells_the_page_nothing() {
    let (mut chain, _writer) = chain_at_the_end_of_the_downloaded_media();
    // Зритель поставил паузу ДО жеста (одна `pause` странице — до жеста, не в счёт).
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, Instant::now());
    chain.drive_until("the paused player", |summary| {
        summary.state == PlaybackState::Paused
    });
    assert!(chain.downloaded().covers(ON_SHELF_RELEASE));
    let gesture_start = chain.seen.len();

    let diagnostics = chain.begin_scrub_at(ON_SHELF_FIRST);
    chain.drive_until_frame_near("the first preview frame", ON_SHELF_FIRST);
    chain.preview_at(ON_SHELF_RELEASE, diagnostics);
    chain.drive_until_frame_near("the second preview frame", ON_SHELF_RELEASE);
    // Отпустили на полке там же, где последняя цель: окно шлёт только «завершить».
    chain.end_scrub(ScrubCommitPolicy::CommitVisiblePreview, diagnostics);
    chain.drive_until("the landing at the release point", |summary| {
        summary.state == PlaybackState::Paused && near(summary.position, ON_SHELF_RELEASE)
    });
    chain.drive_for(Duration::from_millis(600));

    let summary = chain.summary();
    assert_eq!(summary.state, PlaybackState::Paused, "paused as before");
    assert!(near(summary.position, ON_SHELF_RELEASE), "{summary:?}");
    assert!(
        transport_commands(&chain.seen[gesture_start..]).is_empty(),
        "the page got nothing: {:?}",
        transport_commands(&chain.seen[gesture_start..])
    );
}
