//! Водитель окна на подделанном плеере: «чья загрузка», «новее заменяет», отказы,
//! команды и журнал — без окна и GPU.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::{
    CorrelatedPlayerEvent, MediaInstanceId, PlaybackRate, PlaybackState, PlayerError,
    PlayerErrorKind, SeekMode, SeekTarget,
};

use super::super::remote::WindowRemote;
use super::super::test_support::stub_prepared_media;
use super::*;
use crate::browser_session::session_media::MediaPlayback;

fn instance(raw: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(raw).expect("non-zero"))
}

/// Плеер окна по сценарию: что ответить на опросы установки, что показать в снимке.
#[derive(Default)]
struct ScriptedPlayer {
    /// Установка идёт (наша, после `begin_load`, или чужая — задаётся тестом).
    pending: bool,
    /// С каким намерением начинались загрузки.
    begun: Vec<PlaybackIntent>,
    /// Сколько раз просили отменить установку.
    supersede_requests: usize,
    /// Ответы на опросы установки по порядку (пусто — «ещё ставится»).
    polls: VecDeque<LoadPoll>,
    /// Отказать сразу на следующий `begin_load`.
    refuse_next_begin: Option<BrowserPlaybackFault>,
    /// Что дошло до плеера.
    sent: Vec<PlayerCommand>,
    /// Текущий снимок.
    snapshot: PlayerSnapshot,
    /// События плеера, ждущие забора.
    events: Vec<CorrelatedPlayerEvent>,
    /// Накопительно отданные на показ кадры.
    presented: u64,
    /// Что водитель передал о скачанном последним.
    downloaded_range: crate::downloaded_range::DownloadedRange,
}

impl WindowPlayer for ScriptedPlayer {
    fn has_pending_load(&self) -> bool {
        self.pending
    }

    fn begin_load(&mut self, _prepared: PreparedMedia, intent: PlaybackIntent) -> BeginLoad {
        assert!(!self.pending, "the driver never starts a second install");
        if let Some(fault) = self.refuse_next_begin.take() {
            return BeginLoad::Refused(fault);
        }
        self.begun.push(intent);
        self.pending = true;
        BeginLoad::Started
    }

    fn supersede_pending_load(&mut self) {
        self.supersede_requests += 1;
    }

    fn poll_load(&mut self) -> LoadPoll {
        match self.polls.pop_front() {
            None | Some(LoadPoll::Pending) => LoadPoll::Pending,
            Some(done) => {
                self.pending = false;
                done
            }
        }
    }

    fn send(&mut self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.sent.push(command);
        Ok(())
    }

    fn snapshot(&mut self) -> Arc<PlayerSnapshot> {
        Arc::new(self.snapshot.clone())
    }

    fn offer_downloaded_range(&mut self, range: crate::downloaded_range::DownloadedRange) {
        self.downloaded_range = range;
    }

    fn take_player_events(&mut self) -> TappedPlayerEvents {
        TappedPlayerEvents {
            events: std::mem::take(&mut self.events),
            evicted: 0,
        }
    }

    fn video_frames_presented(&self) -> u64 {
        self.presented
    }
}

impl ScriptedPlayer {
    /// Плеер показывает экземпляр `raw` в состоянии `state`.
    fn show(&mut self, raw: u64, state: PlaybackState) {
        self.snapshot.media_instance_id = Some(instance(raw));
        self.snapshot.playback_state = state;
    }
}

/// Пульт + водитель над одним столом и подделанный плеер.
struct Harness {
    remote: WindowRemote,
    driver: WindowPlaybackDriver,
    player: ScriptedPlayer,
}

impl Harness {
    fn new() -> Self {
        let desk = PlaybackDesk::new();
        Self {
            remote: WindowRemote::new(desk.clone()),
            driver: WindowPlaybackDriver::new(desk, AudioMeter::default(), Instant::now()),
            player: ScriptedPlayer::default(),
        }
    }

    fn load(&mut self, start: LoadStart) -> MediaLoadTicket {
        self.remote
            .load(stub_prepared_media(), start)
            .expect("the remote queues the load")
    }

    /// Один проход цикла окна: команды, установка, наблюдение — и что увидит сеанс.
    fn pass(&mut self) -> super::super::super::playback::PlaybackObservation {
        self.driver.execute_commands(&mut self.player);
        self.driver.drive_loads(&mut self.player);
        self.driver.observe(&mut self.player, Instant::now());
        self.remote.pump(Instant::now())
    }
}

#[test]
fn an_installed_load_is_what_the_observation_shows() {
    let mut harness = Harness::new();
    let ticket = harness.load(LoadStart::Playing);
    // Пока ставится, плеер ещё пуст: наблюдение — не про эту загрузку.
    let installing = harness.pass();
    assert!(!installing.shows(ticket));
    assert_eq!(harness.player.begun, [PlaybackIntent::StartPlaying]);

    harness
        .player
        .polls
        .push_back(LoadPoll::Installed(instance(7)));
    harness.player.show(7, PlaybackState::Playing);
    harness.player.snapshot.current_position = Duration::from_millis(400);
    let shown = harness.pass();
    assert!(
        shown.shows(ticket),
        "the snapshot is the installed instance of this load"
    );
    assert_eq!(shown.position, Duration::from_millis(400));
    assert_eq!(shown.fault, None);
}

#[test]
fn a_paused_load_starts_paused() {
    let mut harness = Harness::new();
    let _ticket = harness.load(LoadStart::Paused);
    harness.pass();
    assert_eq!(harness.player.begun, [PlaybackIntent::StartPaused]);
}

#[test]
fn an_install_failure_belongs_to_its_own_load_only() {
    let mut harness = Harness::new();
    let failing = harness.load(LoadStart::Playing);
    harness.pass();
    harness
        .player
        .polls
        .push_back(LoadPoll::Failed(BrowserPlaybackFault::UnsupportedVideo));
    let observation = harness.pass();
    assert_eq!(
        observation.fault_of(failing),
        Some(&BrowserPlaybackFault::UnsupportedVideo)
    );

    // Следующая загрузка начинается без чужого отказа.
    let next = harness.load(LoadStart::Playing);
    let observation = harness.pass();
    assert_eq!(observation.fault_of(next), None);
    assert_eq!(
        observation.fault_of(failing),
        None,
        "the old fault is forgotten"
    );
}

#[test]
fn a_superseded_load_failing_late_does_not_blame_the_newer_load() {
    let mut harness = Harness::new();
    let older = harness.load(LoadStart::Playing);
    harness.pass();
    let newer = harness.load(LoadStart::Playing);
    // Прежняя установка кончается отказом уже после того, как пришла новая.
    harness
        .player
        .polls
        .push_back(LoadPoll::Failed(BrowserPlaybackFault::UnsupportedVideo));
    let observation = harness.pass();
    assert_eq!(observation.fault_of(newer), None);
    assert_eq!(observation.fault_of(older), None);
    assert_eq!(harness.player.supersede_requests, 1);
    assert_eq!(
        harness.player.begun.len(),
        2,
        "the newer load starts right after"
    );
}

#[test]
fn the_newest_generation_wins_while_an_older_one_is_installing() {
    let mut harness = Harness::new();
    let _first = harness.load(LoadStart::Playing);
    harness.pass();
    assert_eq!(harness.player.begun.len(), 1);

    // Два новых поколения, пока первое ставится: второе не ставится вовсе.
    let _second = harness.load(LoadStart::Playing);
    harness.pass();
    let third = harness.load(LoadStart::Paused);
    harness.pass();
    assert_eq!(
        harness.player.begun.len(),
        1,
        "the slot is busy, nothing else starts"
    );
    assert_eq!(harness.player.supersede_requests, 2);

    harness.player.polls.push_back(LoadPoll::Cancelled);
    harness.pass();
    assert_eq!(
        harness.player.begun,
        [PlaybackIntent::StartPlaying, PlaybackIntent::StartPaused],
        "after the cancellation only the newest load is installed"
    );

    harness
        .player
        .polls
        .push_back(LoadPoll::Installed(instance(3)));
    harness.player.show(3, PlaybackState::Paused);
    let observation = harness.pass();
    assert!(observation.shows(third));
    assert_eq!(harness.driver.summary().counters.loads, 3);
}

#[test]
fn a_late_install_of_a_superseded_load_is_not_attributed_to_the_newer_one() {
    let mut harness = Harness::new();
    let older = harness.load(LoadStart::Playing);
    harness.pass();
    let newer = harness.load(LoadStart::Playing);
    // Отменять поздно: прежнее медиа всё-таки установилось.
    harness
        .player
        .polls
        .push_back(LoadPoll::Installed(instance(5)));
    harness.player.show(5, PlaybackState::Playing);
    let observation = harness.pass();
    assert!(
        !observation.shows(newer),
        "the old media is still on screen"
    );
    assert!(observation.shows(older));
    // Ошибка прежнего экземпляра — не отказ новой загрузки.
    harness.player.events.push(CorrelatedPlayerEvent {
        media_instance_id: Some(instance(5)),
        event: PlayerEvent::RecoverableError(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            "old media",
        )),
    });
    let observation = harness.pass();
    assert_eq!(observation.fault_of(newer), None);
}

#[test]
fn a_refused_start_is_the_fault_of_that_load() {
    let mut harness = Harness::new();
    harness.player.refuse_next_begin = Some(BrowserPlaybackFault::VideoDecoderStartFailed);
    let ticket = harness.load(LoadStart::Playing);
    let observation = harness.pass();
    assert_eq!(
        observation.fault_of(ticket),
        Some(&BrowserPlaybackFault::VideoDecoderStartFailed)
    );
    assert!(!harness.driver.has_load_in_progress());
}

#[test]
fn a_foreign_pending_open_delays_the_load_until_the_slot_is_free() {
    let mut harness = Harness::new();
    harness.player.pending = true;
    let ticket = harness.load(LoadStart::Playing);
    harness.pass();
    assert!(
        harness.player.begun.is_empty(),
        "the slot belongs to someone else"
    );
    assert!(harness.driver.has_load_in_progress());

    harness.player.pending = false;
    harness.pass();
    assert_eq!(harness.player.begun.len(), 1);
    harness
        .player
        .polls
        .push_back(LoadPoll::Installed(instance(9)));
    harness.player.show(9, PlaybackState::Playing);
    assert!(harness.pass().shows(ticket));
}

#[test]
fn transport_commands_reach_the_player_in_order() {
    let mut harness = Harness::new();
    let half_speed = PlaybackRate::new(0.5).expect("valid rate");
    harness.remote.pause().expect("queued");
    harness
        .remote
        .seek(Duration::from_secs(12))
        .expect("queued");
    harness.remote.set_rate(half_speed).expect("queued");
    harness.remote.play().expect("queued");
    harness.pass();

    assert_eq!(harness.player.sent.len(), 4);
    assert!(matches!(harness.player.sent[0], PlayerCommand::Pause));
    let PlayerCommand::Seek(request) = &harness.player.sent[1] else {
        panic!("seek second: {:?}", harness.player.sent);
    };
    assert_eq!(request.mode, SeekMode::Accurate);
    assert!(matches!(
        request.target,
        SeekTarget::Absolute(time) if time.as_duration() == Duration::from_secs(12)
    ));
    assert!(matches!(
        harness.player.sent[2],
        PlayerCommand::SetPlaybackRate(rate) if rate == half_speed
    ));
    assert!(matches!(harness.player.sent[3], PlayerCommand::Play));
}

#[test]
fn the_journal_counts_frames_the_player_presented_and_the_end() {
    let mut harness = Harness::new();
    let _ticket = harness.load(LoadStart::Playing);
    harness.pass();
    harness
        .player
        .polls
        .push_back(LoadPoll::Installed(instance(1)));
    harness.player.show(1, PlaybackState::Playing);
    harness.player.presented = 4;
    harness.pass();
    harness.player.presented = 10;
    harness.player.show(1, PlaybackState::Ended);
    harness.pass();

    let summary = harness.driver.summary();
    assert_eq!(summary.counters.presented_frames, 10);
    assert_eq!(summary.counters.loads, 1);
    assert_eq!(summary.state, PlaybackState::Ended);
}

#[test]
fn a_current_load_cancelled_without_a_newer_one_is_its_fault() {
    let mut harness = Harness::new();
    let ticket = harness.load(LoadStart::Playing);
    harness.pass();
    // Никто не заменял загрузку, а установка отменена (остановка, структурная отмена
    // до барьера, установка пропала без итога): медиа так и не встанет.
    harness.player.polls.push_back(LoadPoll::Cancelled);
    let observation = harness.pass();
    assert_eq!(
        observation.fault_of(ticket),
        Some(&BrowserPlaybackFault::InstallAborted),
        "the session must learn that this load will never be shown"
    );
    assert!(!observation.shows(ticket));
    assert!(!harness.driver.has_load_in_progress());
}

#[test]
fn a_replaced_load_cancelled_for_the_newer_one_is_nobodys_fault() {
    let mut harness = Harness::new();
    let older = harness.load(LoadStart::Playing);
    harness.pass();
    let newer = harness.load(LoadStart::Playing);
    harness.player.polls.push_back(LoadPoll::Cancelled);
    let observation = harness.pass();
    assert_eq!(observation.fault_of(older), None);
    assert_eq!(observation.fault_of(newer), None);
    assert_eq!(
        harness.player.begun.len(),
        2,
        "the newer load takes the free slot"
    );
}

#[test]
fn an_unsupported_video_error_that_overtakes_the_install_result_fails_the_load() {
    let mut harness = Harness::new();
    let ticket = harness.load(LoadStart::Playing);
    harness.pass();

    // Плеер уже поставил экземпляр 7 (снимок про него) и сообщил «видео не по силам»,
    // а путь открытия окна ещё на позднем этапе — итог установки водитель не знает.
    harness.player.show(7, PlaybackState::Playing);
    harness.player.events.push(CorrelatedPlayerEvent {
        media_instance_id: Some(instance(7)),
        event: PlayerEvent::RecoverableError(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            "new media",
        )),
    });
    let before_result = harness.pass();
    assert!(!before_result.shows(ticket), "the install is not known yet");

    // Итог установки пришёл проходом позже; плеер выжил (звук без видео), новых
    // событий нет — отказ всё равно принадлежит этой загрузке.
    harness
        .player
        .polls
        .push_back(LoadPoll::Installed(instance(7)));
    let observation = harness.pass();
    assert!(observation.shows(ticket));
    assert_eq!(
        observation.fault_of(ticket),
        Some(&BrowserPlaybackFault::UnsupportedVideo)
    );
}

#[test]
fn the_downloaded_range_from_the_session_reaches_the_window_player() {
    let mut harness = Harness::new();
    let (_spans, range) = crate::downloaded_range::test_support::FakeDownloadedSpans::range(vec![
        Duration::ZERO..Duration::from_secs(4),
    ]);

    // Медиа-часть сказала, что скачано: таймлайн окна узнаёт это на этом же проходе.
    harness.remote.offer_downloaded_range(range);
    harness.pass();
    assert!(
        harness
            .player
            .downloaded_range
            .covers(Duration::from_secs(1))
    );
    assert!(
        !harness
            .player
            .downloaded_range
            .covers(Duration::from_secs(5))
    );

    // Следующее слово — «неизвестно» (открывается новое поколение): оно заменяет прежнее.
    harness
        .remote
        .offer_downloaded_range(crate::downloaded_range::DownloadedRange::Unknown);
    harness.pass();
    assert!(!harness.player.downloaded_range.is_known());
}
