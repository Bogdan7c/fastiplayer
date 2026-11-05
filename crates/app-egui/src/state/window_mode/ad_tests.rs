//! Решение 206 у входа окна: пока реклама на экране, перемотка и скорость заперты со всех
//! входов, а пауза, «играть» и громкость работают; обычное окно не меняется вовсе.

use std::time::Duration;

use player_core::{PlaybackRate, PlaybackState, PlayerCommand, ScrubCommitPolicy, SeekRequest};

use super::*;
use crate::ad_plate::{AdActionProgress, AdPodLabel, AdSkipOffer};

const POSITION: Duration = Duration::from_secs(20);

fn plate(offer: AdSkipOffer, progress: AdActionProgress) -> AdPlate {
    AdPlate {
        pod: AdPodLabel::Unknown,
        offer,
        progress,
        locks_transport: true,
    }
}

fn countdown_plate() -> AdPlate {
    plate(
        AdSkipOffer::Countdown { seconds: 5 },
        AdActionProgress::Idle,
    )
}

fn rate() -> PlaybackRate {
    PlaybackRate::new(1.5).expect("valid rate")
}

/// Намерения зрителя, которые окно успело положить в ящик.
fn taken_intents(mode: &mut WindowMode) -> Vec<ViewerTransportIntent> {
    let TransportRoute::PageLed(outbox) = &mut mode.transport else {
        panic!("the browser window is page-led");
    };
    outbox.take()
}

fn route(mode: &mut WindowMode, command: PlayerCommand) -> CommandRoute {
    mode.route_command(command, PlaybackState::Playing, POSITION)
}

fn is_refused(route: &CommandRoute) -> bool {
    matches!(route, CommandRoute::RefusedDuringAd)
}

#[test]
fn seek_and_speed_from_every_input_are_refused_and_nothing_reaches_the_session() {
    let mut mode = WindowMode::browser_session();
    assert!(mode.note_ad_plate(Some(countdown_plate())));
    // Клик по таймлайну и перетаскивание без живого скраба, стрелки, колесо, MPRIS Seek /
    // SetPosition — все это `PlayerCommand::Seek`; кнопки и хоткеи скорости —
    // `SetPlaybackRate`.
    for command in [
        PlayerCommand::Seek(SeekRequest::absolute(Duration::from_secs(3).into())),
        PlayerCommand::Seek(SeekRequest::relative(Duration::from_secs(5))),
        PlayerCommand::Seek(SeekRequest::accurate(Duration::from_secs(7))),
        PlayerCommand::SetPlaybackRate(rate()),
    ] {
        let verdict = route(&mut mode, command.clone());
        assert!(is_refused(&verdict), "{command:?}");
    }
    assert!(
        taken_intents(&mut mode).is_empty(),
        "a refused command leaves no viewer intent"
    );
}

#[test]
fn pause_play_toggle_and_volume_work_during_an_ad() {
    let mut mode = WindowMode::browser_session();
    mode.note_ad_plate(Some(countdown_plate()));
    for command in [
        PlayerCommand::Pause,
        PlayerCommand::Play,
        PlayerCommand::TogglePlayback,
    ] {
        assert!(matches!(route(&mut mode, command), CommandRoute::ToSession));
    }
    let volume = route(&mut mode, PlayerCommand::SetVolume(0.4));
    assert!(matches!(
        volume,
        CommandRoute::ToPlayer(PlayerCommand::SetVolume(_))
    ));
    assert_eq!(
        taken_intents(&mut mode),
        [
            ViewerTransportIntent::Pause,
            ViewerTransportIntent::Play,
            ViewerTransportIntent::Pause,
        ]
    );
}

#[test]
fn seek_and_speed_work_again_when_the_ad_is_gone() {
    let mut mode = WindowMode::browser_session();
    mode.note_ad_plate(Some(countdown_plate()));
    assert!(mode.note_ad_plate(None), "the plate disappeared: a change");
    assert!(matches!(
        route(&mut mode, PlayerCommand::SetPlaybackRate(rate())),
        CommandRoute::ToSession
    ));
    assert!(matches!(
        route(
            &mut mode,
            PlayerCommand::Seek(SeekRequest::absolute(Duration::from_secs(3).into()))
        ),
        CommandRoute::ToSession
    ));
    assert_eq!(
        taken_intents(&mut mode),
        [
            ViewerTransportIntent::SetRate(rate()),
            ViewerTransportIntent::Seek(Duration::from_secs(3)),
        ]
    );
}

#[test]
fn a_scrub_started_during_an_ad_is_refused_whole_even_if_the_ad_ends_mid_gesture() {
    let mut mode = WindowMode::browser_session();
    mode.note_ad_plate(Some(countdown_plate()));
    let target = SeekRequest::absolute(Duration::from_secs(9).into());
    assert!(is_refused(&route(&mut mode, PlayerCommand::begin_scrub())));
    assert!(is_refused(&route(
        &mut mode,
        PlayerCommand::preview_scrub(target)
    )));
    // Реклама кончилась на середине жеста: остаток жеста всё равно не доходит до
    // плеера, у которого скраба нет.
    mode.note_ad_plate(None);
    assert!(is_refused(&route(
        &mut mode,
        PlayerCommand::UpdateScrub(target)
    )));
    assert!(is_refused(&route(
        &mut mode,
        PlayerCommand::preview_scrub(target)
    )));
    assert!(is_refused(&route(
        &mut mode,
        PlayerCommand::end_scrub(ScrubCommitPolicy::CommitLatestTarget)
    )));
    // Следующий жест — с чистого листа.
    assert!(matches!(
        route(&mut mode, PlayerCommand::begin_scrub()),
        CommandRoute::ToPlayer(PlayerCommand::BeginScrub { .. })
    ));
}

#[test]
fn a_plate_past_its_lock_limit_keeps_showing_but_lets_the_viewer_seek() {
    let mut mode = WindowMode::browser_session();
    let mut expired = countdown_plate();
    expired.locks_transport = false;
    mode.note_ad_plate(Some(expired));
    let seek = PlayerCommand::Seek(SeekRequest::absolute(Duration::from_secs(9).into()));
    assert!(!is_refused(&route(&mut mode, seek)));
    assert!(!is_refused(&route(
        &mut mode,
        PlayerCommand::SetPlaybackRate(rate())
    )));
    assert_eq!(mode.ad_gate.plate(), Some(&expired));
}

#[test]
fn a_scrub_started_before_the_ad_is_finished_so_the_player_is_not_left_scrubbing() {
    let mut mode = WindowMode::browser_session();
    let target = SeekRequest::absolute(Duration::from_secs(9).into());
    assert!(matches!(
        route(&mut mode, PlayerCommand::begin_scrub()),
        CommandRoute::ToPlayer(_)
    ));
    mode.note_ad_plate(Some(countdown_plate()));
    assert!(is_refused(&route(
        &mut mode,
        PlayerCommand::preview_scrub(target)
    )));
    assert!(matches!(
        route(
            &mut mode,
            PlayerCommand::end_scrub(ScrubCommitPolicy::CommitVisiblePreview)
        ),
        CommandRoute::ToPlayer(PlayerCommand::EndScrub { .. })
    ));
}

#[test]
fn the_standard_window_ignores_the_ad_plate_and_keeps_its_transport() {
    let mut standard = WindowMode::standard();
    assert!(!standard.note_ad_plate(Some(countdown_plate())));
    assert_eq!(standard.ad_gate.plate(), None);
    for command in [
        PlayerCommand::Seek(SeekRequest::absolute(Duration::from_secs(3).into())),
        PlayerCommand::SetPlaybackRate(rate()),
        PlayerCommand::begin_scrub(),
    ] {
        let verdict = route(&mut standard, command.clone());
        assert!(
            matches!(&verdict, CommandRoute::ToPlayer(sent) if *sent == command),
            "{command:?} -> {verdict:?}"
        );
    }
    assert!(!standard.request_ad_action(AdPlateAction::Skip));
    assert_eq!(
        standard.activate_ad_offer(),
        AdOfferActivation::NothingOffered
    );
}

#[test]
fn next_presses_the_offered_button_and_only_when_it_is_free() {
    let mut mode = WindowMode::browser_session();
    assert_eq!(mode.activate_ad_offer(), AdOfferActivation::NothingOffered);
    mode.note_ad_plate(Some(countdown_plate()));
    assert_eq!(mode.activate_ad_offer(), AdOfferActivation::NothingOffered);
    assert!(!mode.ad_offer_is_free());

    mode.note_ad_plate(Some(plate(AdSkipOffer::Skip, AdActionProgress::Idle)));
    assert!(mode.ad_offer_is_free());
    assert_eq!(mode.activate_ad_offer(), AdOfferActivation::Requested);
    // Подсказка «Skip on the page» кнопки не имеет: N ничего не просит у страницы.
    mode.note_ad_plate(Some(plate(AdSkipOffer::OnPage, AdActionProgress::Idle)));
    assert!(!mode.ad_offer_is_free());
    assert_eq!(mode.activate_ad_offer(), AdOfferActivation::NothingOffered);

    mode.note_ad_plate(Some(plate(AdSkipOffer::Skip, AdActionProgress::InFlight)));
    assert!(!mode.ad_offer_is_free());
    assert_eq!(mode.activate_ad_offer(), AdOfferActivation::AlreadyInFlight);

    assert_eq!(
        taken_intents(&mut mode),
        [ViewerTransportIntent::SkipAd],
        "the busy button sent nothing"
    );
}
