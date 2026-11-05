//! Реклама на странице в медиа-части (сессия 09d): плашка, команды `skip_ad` /
//! `reveal_page`, защита от двойного клика и запрет перемотки со скоростью (решение 206).
//!
//! Плеер и страница подделаны: видна каждая команда плееру и каждое сообщение странице.

use browser_bridge_protocol::session::{
    AdCountdownSeconds, AdPod, AdPodNumber, AdSkip, PageAdState,
};

use super::*;
use crate::ad_plate::{AdActionProgress, AdPlate, AdPodLabel, AdSkipOffer};
use crate::browser_session::session_media::ad_presence::ACTION_IN_FLIGHT_TIMEOUT;
use crate::viewer_transport::ViewerTransportIntent;

fn ad_notice(session: &SessionRef, state: PageAdState) -> IntakeNotice {
    IntakeNotice::AdState {
        session: session.clone(),
        state,
    }
}

fn pod(index: u16, count: u16) -> AdPod {
    AdPod::Known {
        index: AdPodNumber::new(index).expect("index"),
        count: AdPodNumber::new(count).expect("count"),
    }
}

fn showing(skip: AdSkip) -> PageAdState {
    PageAdState::Showing {
        pod: pod(1, 2),
        skip,
    }
}

fn countdown(seconds: u16) -> AdSkip {
    AdSkip::Countdown {
        seconds: AdCountdownSeconds::new(seconds).expect("seconds"),
    }
}

/// Сообщения странице без часов и буфера.
fn commands_to_page(harness: &mut Harness) -> Vec<PlayerSessionMessage> {
    harness
        .page
        .take_sent()
        .into_iter()
        .filter(|message| {
            !matches!(
                message,
                PlayerSessionMessage::Clock { .. } | PlayerSessionMessage::BufferState { .. }
            )
        })
        .collect()
}

fn viewer(harness: &mut Harness, intent: ViewerTransportIntent, now: Instant) {
    harness.media.on_viewer_transport(intent, now);
    harness.media.drive(now);
}

fn state(harness: &mut Harness, current: &SessionRef, state: PageAdState) {
    harness
        .media
        .on_intake(vec![ad_notice(current, state)], None);
}

#[test]
fn an_ad_state_makes_the_plate_and_none_removes_it() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    assert_eq!(harness.media.ad_plate(now), None);

    state(&mut harness, &current, showing(countdown(5)));
    assert_eq!(
        harness.media.ad_plate(now),
        Some(AdPlate {
            pod: AdPodLabel::Known { index: 1, count: 2 },
            offer: AdSkipOffer::Countdown { seconds: 5 },
            progress: AdActionProgress::Idle,
            locks_transport: true,
        })
    );
    state(&mut harness, &current, PageAdState::None {});
    assert_eq!(harness.media.ad_plate(now), None);
}

#[test]
fn the_ad_state_of_a_replaced_session_is_not_ours() {
    let (mut harness, _current, _writer) = loaded_harness();
    state(&mut harness, &session(99), showing(AdSkip::Available {}));
    assert_eq!(harness.media.ad_plate(Instant::now()), None);
}

#[test]
fn skip_sends_exactly_one_skip_ad_until_the_page_answers() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    state(&mut harness, &current, showing(AdSkip::Available {}));
    commands_to_page(&mut harness);

    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    assert_eq!(
        commands_to_page(&mut harness),
        vec![PlayerSessionMessage::SkipAd {}],
        "three clicks, one command"
    );
    assert_eq!(
        harness.media.ad_plate(now).map(|plate| plate.progress),
        Some(AdActionProgress::InFlight)
    );

    // Страница ответила: пошла вторая реклама — кнопка свободна и снова работает.
    state(
        &mut harness,
        &current,
        PageAdState::Showing {
            pod: pod(2, 2),
            skip: AdSkip::Available {},
        },
    );
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    assert_eq!(
        commands_to_page(&mut harness),
        vec![PlayerSessionMessage::SkipAd {}]
    );
}

#[test]
fn a_page_that_never_reacts_gets_the_command_again_after_the_timeout() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    state(&mut harness, &current, showing(AdSkip::Available {}));
    commands_to_page(&mut harness);
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    assert_eq!(commands_to_page(&mut harness).len(), 1);

    let almost = now + ACTION_IN_FLIGHT_TIMEOUT - Duration::from_millis(1);
    viewer(&mut harness, ViewerTransportIntent::SkipAd, almost);
    assert!(commands_to_page(&mut harness).is_empty(), "still in flight");

    let later = now + ACTION_IN_FLIGHT_TIMEOUT;
    viewer(&mut harness, ViewerTransportIntent::SkipAd, later);
    assert_eq!(
        commands_to_page(&mut harness),
        vec![PlayerSessionMessage::SkipAd {}]
    );
}

#[test]
fn go_to_page_sends_reveal_page_and_only_when_the_page_wants_a_human() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    // Кнопка «Skip» у сайта, который верит только мыши, не наша: reveal не предложен.
    state(&mut harness, &current, showing(AdSkip::Available {}));
    commands_to_page(&mut harness);
    viewer(&mut harness, ViewerTransportIntent::RevealPage, now);
    assert!(commands_to_page(&mut harness).is_empty());

    state(&mut harness, &current, showing(AdSkip::OnPage {}));
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    assert!(
        commands_to_page(&mut harness).is_empty(),
        "skip is not offered on_page"
    );
    viewer(&mut harness, ViewerTransportIntent::RevealPage, now);
    viewer(&mut harness, ViewerTransportIntent::RevealPage, now);
    assert_eq!(
        commands_to_page(&mut harness),
        vec![PlayerSessionMessage::RevealPage {}]
    );
}

#[test]
fn no_command_goes_to_the_page_during_a_countdown_or_without_an_ad() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    commands_to_page(&mut harness);
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    state(&mut harness, &current, showing(countdown(4)));
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    viewer(&mut harness, ViewerTransportIntent::RevealPage, now);
    assert!(commands_to_page(&mut harness).is_empty());
}

#[test]
fn seek_and_speed_of_the_viewer_do_nothing_during_an_ad_but_pause_and_play_work() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    state(&mut harness, &current, showing(countdown(5)));
    harness.player.take_calls();
    commands_to_page(&mut harness);

    let rate = player_core::PlaybackRate::new(2.0).expect("rate");
    viewer(
        &mut harness,
        ViewerTransportIntent::Seek(Duration::from_millis(100)),
        now,
    );
    viewer(&mut harness, ViewerTransportIntent::SetRate(rate), now);
    assert_eq!(
        harness.player.take_calls(),
        Vec::new(),
        "the player got nothing"
    );
    assert!(
        commands_to_page(&mut harness).is_empty(),
        "the page got nothing"
    );

    viewer(&mut harness, ViewerTransportIntent::Pause, now);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);
    assert_eq!(
        commands_to_page(&mut harness),
        vec![PlayerSessionMessage::Pause {}]
    );
    viewer(&mut harness, ViewerTransportIntent::Play, now);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);

    // Реклама кончилась — перемотка и скорость снова работают.
    state(&mut harness, &current, PageAdState::None {});
    viewer(&mut harness, ViewerTransportIntent::SetRate(rate), now);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::SetRate(2.0)]);
    viewer(
        &mut harness,
        ViewerTransportIntent::Seek(Duration::from_millis(100)),
        now,
    );
    assert!(
        harness
            .player
            .take_calls()
            .contains(&PlayerCall::Seek(Duration::from_millis(100)))
    );
}

/// Запрет касается зрителя; страница сама ведёт время и скорость.
#[test]
fn the_page_still_leads_its_own_seek_and_speed_during_an_ad() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    state(&mut harness, &current, showing(countdown(5)));
    harness.player.take_calls();

    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::SetRate(PagePlaybackRate::from_permille(1_500).expect("rate")),
        )],
        None,
    );
    harness.media.drive(now);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::SetRate(1.5)]);
}

#[test]
fn the_end_of_the_session_and_a_new_session_remove_the_plate() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    state(&mut harness, &current, showing(AdSkip::Available {}));
    assert!(harness.media.ad_plate(now).is_some());
    harness.media.on_session_ended(&current, TAB_GONE_END);
    assert_eq!(
        harness.media.ad_plate(now),
        None,
        "the tab left: the plate goes"
    );
    // Слово, опоздавшее после ухода вкладки, плашку не возвращает.
    state(&mut harness, &current, showing(AdSkip::Available {}));
    assert_eq!(harness.media.ad_plate(now), None);
    // Страницы нет — команды плашки никуда не идут.
    commands_to_page(&mut harness);
    viewer(&mut harness, ViewerTransportIntent::SkipAd, now);
    assert!(commands_to_page(&mut harness).is_empty());

    let (mut second, current, _writer) = loaded_harness();
    state(&mut second, &current, showing(AdSkip::OnPage {}));
    second.media.on_session_opened(&session(2));
    assert_eq!(second.media.ad_plate(now), None, "a new page has no ad yet");
}
