//! Спиннер ожидания в окне браузерного режима на НАСТОЯЩЕМ плеере (мост, сессия 04b,
//! решение 129: «ожидание данных после перемотки вне полки — спиннер»).
//!
//! Что настоящее: медиа-часть сеанса, плеер (`BrowserPlayback` на production
//! `PlayerWorker`), поток `pushed-media-source` на фикстурах протокола и владелец
//! уведомлений окна (`NotificationCenter`) с его правилом спиннера. Харнесс — тот же, что
//! у живого скраба (`live_scrub_real_player`): страница подделана, команды жеста плееру
//! шлются так же, как их выдаёт цепочка окна «жест → план → диспетчер».
//!
//! Каждый проход тест кормит владельца уведомлений состоянием плеера и тем, держит ли
//! зритель таймлайн, — ровно как `AppState::notifications_frame` в каждом кадре окна.
//!
//! Находка, которую закрепляют тесты: пока плеер ждёт байтов после клика или отпускания
//! вне скачанного, он стоит в `Scrubbing` (посадка перемотки player-core), а не в
//! `Seeking`. Прежнее правило спиннера считало `Scrubbing` «кадрами превью» и молчало —
//! зритель видел застывшую картинку до `SeekTimeout` (живой показ 04a).

use std::time::{Duration, Instant};

use player_core::{PlaybackState, ScrubCommitPolicy};

use super::live_scrub_real_player::{
    OFF_SHELF, ON_SHELF_FIRST, STEP, ScrubChain, chain_at_the_end_of_the_downloaded_media, near,
    page_delivers_media_at, seek_targets,
};
use super::*;
use crate::state::{NotificationCenter, OpenProgress, TimelineGrip};
use crate::ui::animation::UiMotion;
use crate::viewer_transport::ViewerTransportIntent;

/// Сколько ждать после перемотки, чтобы спиннер успел появиться: задержка появления
/// (`BUFFERING_INDICATOR_APPEAR_DELAY`, 500 мс) с запасом на занятую машину.
const WAIT_LONGER_THAN_THE_APPEAR_DELAY: Duration = Duration::from_millis(900);

/// Сколько зритель держит бегунок на полке: дольше задержки появления спиннера.
const HOLD_LONGER_THAN_THE_APPEAR_DELAY: Duration = Duration::from_millis(900);

/// Потолок ожидания посадки после ответа страницы.
const LANDING_DEADLINE: Duration = Duration::from_secs(20);

/// Что окно показало бы за один проход.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PassView {
    /// Состояние плеера.
    state: PlaybackState,
    /// Горит ли спиннер.
    spinner: bool,
}

/// Владелец уведомлений окна, которого тест кормит каждый проход.
struct WindowSpinner {
    center: NotificationCenter,
}

impl WindowSpinner {
    fn new() -> Self {
        Self {
            center: NotificationCenter::default(),
        }
    }

    /// Один кадр окна: состояние плеера и жест таймлайна → горит ли спиннер.
    fn frame(&mut self, state: PlaybackState, grip: TimelineGrip) -> PassView {
        let now = Instant::now();
        self.center.observe_playback_waiting(state, grip, now);
        let spinner = self
            .center
            .frame(OpenProgress::Idle, UiMotion::Standard, now)
            .shows_buffering_indicator();
        PassView { state, spinner }
    }
}

/// Крутит цикл `period`, кормя спиннер; возвращает вид каждого прохода.
fn drive_with_spinner(
    chain: &mut ScrubChain,
    spinner: &mut WindowSpinner,
    grip: TimelineGrip,
    period: Duration,
) -> Vec<PassView> {
    let until = Instant::now() + period;
    let mut views = Vec::new();
    while Instant::now() < until {
        chain.pass();
        views.push(spinner.frame(chain.summary().state, grip));
        std::thread::sleep(STEP);
    }
    views
}

/// Страница прислала байты с цели: крутит цикл, пока кадр у цели не окажется на показе
/// и плеер не заиграет; возвращает вид последнего прохода.
fn deliver_and_land(
    chain: &mut ScrubChain,
    spinner: &mut WindowSpinner,
    writer: &mut PushedStreamWriter,
) -> PassView {
    page_delivers_media_at(writer, OFF_SHELF);
    let deadline = Instant::now() + LANDING_DEADLINE;
    loop {
        chain.pass();
        let summary = chain.summary();
        let view = spinner.frame(summary.state, TimelineGrip::Released);
        let frame_at_target = chain
            .shown_frame_pts()
            .is_some_and(|pts| near(pts, OFF_SHELF));
        if frame_at_target && summary.state == PlaybackState::Playing {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "the player never landed at the target: {:?} at {:?}, frame {:?}",
            summary.state,
            summary.position,
            chain.shown_frame_pts()
        );
        std::thread::sleep(STEP);
    }
}

/// Пока плеер ждёт байтов страницы: он в посадке перемотки (`Scrubbing`), и спиннер
/// горит; странице ушла ровно одна просьба `seek` к цели.
fn assert_waits_with_the_spinner(chain: &ScrubChain, waiting: &[PassView], asked_from: usize) {
    let last = waiting.last().expect("at least one pass");
    assert_eq!(
        last.state,
        PlaybackState::Scrubbing,
        "the player waits for the bytes in the seek landing: {waiting:?}"
    );
    assert!(
        last.spinner,
        "the viewer sees the spinner while the player waits: {waiting:?}"
    );
    assert_eq!(
        seek_targets(&chain.seen[asked_from..]),
        [u64::try_from(OFF_SHELF.as_micros()).expect("small")],
        "the page is asked to seek once"
    );
}

#[test]
fn a_click_outside_the_shelf_shows_the_spinner_until_the_frame_lands() {
    let (mut chain, mut writer) = chain_at_the_end_of_the_downloaded_media();
    let mut spinner = WindowSpinner::new();
    let asked_from = chain.seen.len();

    // Клик по таймлайну вне скачанного: одна перемотка зрителя через сеанс.
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(OFF_SHELF), Instant::now());
    let waiting = drive_with_spinner(
        &mut chain,
        &mut spinner,
        TimelineGrip::Released,
        WAIT_LONGER_THAN_THE_APPEAR_DELAY,
    );
    assert_waits_with_the_spinner(&chain, &waiting, asked_from);

    // Байты пришли: кадр у цели на показе, плеер играет — спиннер погас.
    let landed = deliver_and_land(&mut chain, &mut spinner, &mut writer);
    assert!(
        !landed.spinner,
        "the spinner is gone after landing: {landed:?}"
    );
}

#[test]
fn a_drag_over_the_shelf_never_shows_the_spinner_but_its_release_outside_does() {
    let (mut chain, mut writer) = chain_at_the_end_of_the_downloaded_media();
    // Окно появилось уже во время жеста: ожидание «до жеста» (конец скачанного) не в счёт.
    let mut spinner = WindowSpinner::new();

    // Нажал и тянет по полке: кадры превью бегут, зритель держит бегунок дольше задержки
    // появления спиннера.
    let diagnostics = chain.begin_scrub_at(ON_SHELF_FIRST);
    chain.drive_until_frame_near("the on-shelf preview frame", ON_SHELF_FIRST);
    let held = drive_with_spinner(
        &mut chain,
        &mut spinner,
        TimelineGrip::HeldByViewer,
        HOLD_LONGER_THAN_THE_APPEAR_DELAY,
    );
    assert!(
        held.iter()
            .any(|view| view.state == PlaybackState::Scrubbing),
        "the player scrubs while the viewer holds the timeline: {held:?}"
    );
    assert!(
        held.iter().all(|view| !view.spinner),
        "no spinner over the preview frames: {held:?}"
    );
    assert!(near(
        chain.shown_frame_pts().expect("a preview frame"),
        ON_SHELF_FIRST
    ));

    // Отпустил вне полки: скраб садится на показанное, и одна перемотка зрителя.
    let asked_from = chain.seen.len();
    chain.end_scrub(ScrubCommitPolicy::CommitLatestTarget, diagnostics);
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(OFF_SHELF), Instant::now());
    let waiting = drive_with_spinner(
        &mut chain,
        &mut spinner,
        TimelineGrip::Released,
        WAIT_LONGER_THAN_THE_APPEAR_DELAY,
    );
    assert_waits_with_the_spinner(&chain, &waiting, asked_from);

    let landed = deliver_and_land(&mut chain, &mut spinner, &mut writer);
    assert!(
        !landed.spinner,
        "the spinner is gone after landing: {landed:?}"
    );
}
