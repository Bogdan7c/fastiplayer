//! Клик зрителя за скачанным во время досмотра (сессия 09a2, дыра Д-5).
//!
//! Страница ушла, окно досматривает скачанное. Поток приёма при этом обрывает скачивание
//! (`end_download_cut_short`): вход полос кончен. Раньше правило «перемотка только по
//! скачанному» спрашивало «сядет ли перемотка с полки», а после конца входа перемотка за
//! конец полки «садится» на последний сегмент — клик за скачанное уходил плееру, плеер
//! прыгал в конец, и досмотр кончался сразу. Теперь клик отклоняется: плееру ничего
//! (бегунок остаётся у позиции, спиннера нет), окну — одно сообщение зрителю.

use super::*;
use crate::browser_session::session_media::ViewerSeekNotice;
use crate::viewer_transport::ViewerTransportIntent;

/// Политика окна: пауза ждёт зрителя.
const WINDOW_POLICY: DrainPolicy = DrainPolicy {
    stall_timeout: Duration::from_millis(400),
    poll_interval: Duration::from_millis(250),
    paused: PausedDrain::WaitForViewer,
};

/// Сеанс, у поколения которого объявлено 10 с, а скачан ролик фикстуры (0,4 с); плеер
/// показывает его на 0,1 с. Затем вкладка ушла: сеанс кончился, вход запечатан, а
/// скачивание оборвано, как это делает поток приёма.
fn draining_after_a_cut_download() -> (Harness, PushedStreamWriter) {
    let mut harness = Harness::new();
    harness.media.drain_policy = WINDOW_POLICY;
    let current = session(1);
    harness.media.on_session_opened(&current);
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, opener) = pushed_stream(policy).expect("stream starts");
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(10)));
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: current.clone(),
        generation: generation(1),
        source: GenerationSource::Segments {
            monitor: writer.monitor(),
            opener,
        },
    }));
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::from_millis(100), PlaybackState::Playing);
    harness.media.drive(Instant::now());

    harness.media.on_session_ended(&current, TAB_GONE_END);
    writer
        .end_download_cut_short()
        .expect("the intake cuts the download short");
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: current.clone(),
        }],
        None,
    );
    harness.media.drive(Instant::now());
    harness.player.take_calls();
    harness.page.take_sent();
    (harness, writer)
}

fn viewer_seek(harness: &mut Harness, target: Duration) {
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
}

/// Клик за скачанное (5 с при скачанных 0,4 с): плееру ничего — ни перемотки, ни
/// «играть/пауза»; окну — ровно одно сообщение; странице — ничего.
#[test]
fn a_click_past_a_cut_download_is_refused_and_told_to_the_viewer_once() {
    let (mut harness, _writer) = draining_after_a_cut_download();

    viewer_seek(&mut harness, Duration::from_secs(5));

    assert_eq!(
        harness.player.take_calls(),
        Vec::new(),
        "the player got a seek past what was downloaded"
    );
    assert_eq!(
        harness.media.take_viewer_seek_notice(),
        ViewerSeekNotice::OutsideDownloaded
    );
    assert_eq!(
        harness.media.take_viewer_seek_notice(),
        ViewerSeekNotice::Nothing,
        "the notice is told once"
    );
    harness.media.drive(Instant::now());
    assert!(harness.page.take_sent().is_empty(), "the page is gone");
}

/// Отказ не трогает доигрывание: плеер продолжает с того же места, вердикт «доиграно?»
/// по-прежнему ждёт конца скачанного (не застой и не конец из-за клика).
#[test]
fn a_refused_click_does_not_end_or_stall_the_drain() {
    let (mut harness, _writer) = draining_after_a_cut_download();
    let start = Instant::now();
    harness.player.observation.borrow_mut().position = Duration::from_millis(150);

    viewer_seek(&mut harness, Duration::from_secs(5));
    let _notice = harness.media.take_viewer_seek_notice();

    assert!(matches!(
        harness.media.playback_drain(start),
        DrainProgress::Draining { .. }
    ));
    harness.player.observation.borrow_mut().position = Duration::from_millis(300);
    harness.media.drive(start + Duration::from_millis(250));
    assert!(matches!(
        harness
            .media
            .playback_drain(start + Duration::from_millis(250)),
        DrainProgress::Draining { .. }
    ));
}

/// Клик внутри скачанного во время досмотра по-прежнему перематывает плеер — без
/// сообщения зрителю.
#[test]
fn a_click_inside_the_download_still_seeks_without_a_notice() {
    let (mut harness, _writer) = draining_after_a_cut_download();

    viewer_seek(&mut harness, Duration::from_millis(300));

    assert!(
        harness
            .player
            .take_calls()
            .contains(&PlayerCall::Seek(Duration::from_millis(300)))
    );
    assert_eq!(
        harness.media.take_viewer_seek_notice(),
        ViewerSeekNotice::Nothing
    );
}

/// Пока страница слушает, клик вне полки — не отказ: источник попросит страницу о
/// байтах, сообщения зрителю нет.
#[test]
fn with_the_page_listening_an_off_shelf_click_is_no_refusal() {
    let (mut harness, _current, _writer) = loaded_harness();

    viewer_seek(&mut harness, Duration::from_secs(5));

    assert_eq!(
        harness.media.take_viewer_seek_notice(),
        ViewerSeekNotice::Nothing
    );
}

/// Окну отдаётся скачанное, которое кончается там, где оборвано скачивание: живое
/// перетаскивание за конец не показывает плееру ни одной цели (кадр стоит, бегунок едет),
/// а отпускание там становится перемоткой зрителя — и отказом с сообщением.
#[test]
fn the_window_range_ends_where_the_download_was_cut() {
    let (harness, _writer) = draining_after_a_cut_download();

    let range = harness.player.offered_range();

    assert!(range.covers(Duration::from_millis(300)), "downloaded");
    assert!(
        !range.covers(Duration::from_secs(5)),
        "a target past the cut download is not downloaded"
    );
}
