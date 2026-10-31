//! Доигрывание в окне (04b): пауза ждёт зрителя, «играть» продолжает, сообщение окна
//! следует за доигрыванием, новый сеанс отменяет ожидание, перемотка без страницы —
//! только по скачанному.

use super::*;
use crate::browser_session::session_end::{DrainCause, DrainFinish, SessionEndView};
use crate::viewer_transport::ViewerTransportIntent;

/// Политика окна с коротким застоем.
const WINDOW_POLICY: DrainPolicy = DrainPolicy {
    stall_timeout: Duration::from_millis(400),
    poll_interval: Duration::from_millis(250),
    paused: PausedDrain::WaitForViewer,
};

/// Сеанс с показанным поколением (играет на `position`), политика окна.
fn window_harness(position: Duration) -> (Harness, SessionRef, PushedStreamWriter) {
    let (mut harness, current, writer) = loaded_harness();
    harness.media.drain_policy = WINDOW_POLICY;
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, position, PlaybackState::Playing);
    harness.media.drive(Instant::now());
    harness.player.take_calls();
    harness.page.take_sent();
    (harness, current, writer)
}

fn end_session(harness: &mut Harness, session: &SessionRef, reason: SessionEndReason) {
    harness.media.on_session_ended(session, reason);
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: session.clone(),
        }],
        None,
    );
}

#[test]
fn a_paused_window_waits_for_the_viewer_and_says_so() {
    let (mut harness, current, _writer) = window_harness(Duration::from_millis(200));
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    end_session(&mut harness, &current, TAB_GONE_END);
    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);

    let start = Instant::now();
    for elapsed_ms in [0, 400, 5_000] {
        assert_eq!(
            harness
                .media
                .playback_drain(start + Duration::from_millis(elapsed_ms)),
            DrainProgress::AwaitingViewer,
            "the stall deadline must not end a paused window ({elapsed_ms} ms)"
        );
    }
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::WaitingForViewer(DrainCause::TabClosed)
    );
}

/// «Играть» зрителя после ухода вкладки: плеер играет, странице ничего, доигрывание
/// идёт (сообщение снято) и кончается сообщением «доиграно».
#[test]
fn viewer_play_continues_the_drain_until_the_end() {
    let (mut harness, current, _writer) = window_harness(Duration::from_millis(100));
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, Instant::now());
    end_session(&mut harness, &current, TAB_GONE_END);
    assert_eq!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::AwaitingViewer
    );
    harness.player.take_calls();
    harness.page.take_sent();

    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Play, Instant::now());

    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));
    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);
    {
        let mut observation = harness.player.observation.borrow_mut();
        observation.ended = true;
        observation.state = PlaybackState::Ended;
    }
    harness.media.drive(Instant::now());
    assert_eq!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Drained(DrainEnd::PlayedToEnd)
    );
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end: DrainEnd::PlayedToEnd,
        })
    );
    assert!(
        harness.page.take_sent().is_empty(),
        "the page is gone: no play, no clock"
    );
}

#[test]
fn a_lost_browser_gets_its_own_waiting_view() {
    let (mut harness, current, _writer) = window_harness(Duration::from_millis(100));
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    end_session(
        &mut harness,
        &current,
        SessionEndReason::ReconnectGraceExpired,
    );
    harness.media.playback_drain(Instant::now());

    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::WaitingForViewer(DrainCause::BrowserConnectionLost)
    );
}

/// Новый сеанс во время ожидания зрителя заменяет его: доигрывание и сообщение забыты.
#[test]
fn a_new_session_while_the_window_waits_replaces_the_wait() {
    let (mut harness, current, _writer) = window_harness(Duration::from_millis(100));
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    end_session(&mut harness, &current, TAB_GONE_END);
    harness.media.playback_drain(Instant::now());

    harness.media.on_session_opened(&session(2));

    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);
    assert_eq!(
        harness.media.close_for_viewer(),
        ViewerCloseStep::SessionClosed
    );
}

/// Конец, который не доигрывается (например, нарушение протокола), окно не объясняет
/// словами «вкладка закрыта».
#[test]
fn an_end_that_is_not_drained_has_no_window_message() {
    let (mut harness, current, _writer) = window_harness(Duration::from_millis(100));
    end_session(
        &mut harness,
        &current,
        SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
    );
    harness.media.playback_drain(Instant::now());

    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);
}

/// Сеанс с поколением, у которого объявлено 10 с, а скачано только 0,4 с ролика фикстуры
/// (страница ушла, не докачав): цель после 0,4 с — вне скачанного.
fn partly_downloaded_window_harness() -> (Harness, SessionRef, PushedStreamWriter) {
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
    harness.player.take_calls();
    harness.page.take_sent();
    (harness, current, writer)
}

/// Перемотка без страницы: по скачанному — плееру; вне скачанного — никому (байтов не
/// будет, плеер завис бы в «перемотке»); странице — ничего в обоих случаях.
#[test]
fn without_the_page_the_viewer_seeks_only_within_what_was_downloaded() {
    let (mut harness, current, _writer) = partly_downloaded_window_harness();
    end_session(&mut harness, &current, TAB_GONE_END);
    harness.page.take_sent();

    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_millis(300)),
        Instant::now(),
    );
    let on_shelf = harness.player.take_calls();
    assert!(
        on_shelf.contains(&PlayerCall::Seek(Duration::from_millis(300))),
        "{on_shelf:?}"
    );

    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(5)),
        Instant::now(),
    );
    assert!(harness.player.take_calls().is_empty());
    harness.media.drive(Instant::now());
    assert!(harness.page.take_sent().is_empty());
}

/// Пока страница слушает, та же перемотка вне скачанного идёт плееру (источник сам
/// попросит страницу о байтах) — отказ только без страницы.
#[test]
fn with_the_page_an_off_shelf_viewer_seek_still_reaches_the_player() {
    let (mut harness, _current, _writer) = partly_downloaded_window_harness();

    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(5)),
        Instant::now(),
    );

    assert!(
        harness
            .player
            .take_calls()
            .contains(&PlayerCall::Seek(Duration::from_secs(5)))
    );
}
