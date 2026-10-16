//! Команда `SelectSubtitleTrack`: валидация id против дорожек media и деградация restore.
//!
//! Проверяем наблюдаемые результаты: snapshot выбора, события и итог установки.

use std::num::NonZeroU64;

use crossbeam_channel::bounded;
use media_core::TrackKind;

use super::test_support::{fake_track, install_fake_media};
use super::*;
use crate::media_install::AcceptedPlaybackIntent;
use crate::{
    InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome, InstalledPositionRestore,
    InstalledSubtitleRestore, InstalledTrackRestore, MediaInstallRequestId, MediaInstanceId,
    PlaybackIntent, PlaybackIntentRevision,
};

/// Сколько событий `SubtitleTrackSelected` среди переданных.
fn count_subtitle_selected(events: &[PlayerEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, PlayerEvent::SubtitleTrackSelected(_)))
        .count()
}

#[test]
fn selecting_a_track_without_installed_media_is_rejected_and_changes_nothing() {
    let mut session = PlayerSession::default();

    let error = session
        .dispatch_command(PlayerCommand::SelectSubtitleTrack(Some(TrackId::new(1))))
        .expect_err("no media means no subtitle track to select");

    assert_eq!(error.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(session.snapshot().selected_tracks.subtitle_track, None);
    assert_eq!(count_subtitle_selected(&session.take_events()), 0);
}

#[test]
fn unknown_id_is_rejected_without_touching_the_selection_or_publishing_selected() {
    let mut session = PlayerSession::default();
    // Ид 1 и 2 существуют, но это видео и аудио: субтитрами они не становятся.
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let selection_before = session.snapshot().selected_tracks.clone();
    let _install_events = session.take_events();

    for rejected_id in [1_u32, 2, 99] {
        let error = session
            .dispatch_command(PlayerCommand::SelectSubtitleTrack(Some(TrackId::new(
                rejected_id,
            ))))
            .expect_err("only subtitle tracks of the media are valid");
        assert_eq!(
            error.kind,
            PlayerErrorKind::InvalidCommand,
            "id {rejected_id}"
        );
    }

    assert_eq!(session.snapshot().selected_tracks, selection_before);
    let events = session.take_events();
    assert_eq!(count_subtitle_selected(&events), 0);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PlayerEvent::RecoverableError(_))),
        "the rejection is reported like other invalid runtime commands"
    );
}

#[test]
fn embedded_subtitle_track_id_is_accepted_while_video_audio_and_unknown_ids_stay_rejected() {
    let mut session = PlayerSession::default();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
            fake_track(3, TrackKind::Subtitle),
        ],
    );
    let _install_events = session.take_events();

    for rejected_id in [1_u32, 2, 99] {
        assert!(
            session
                .dispatch_command(PlayerCommand::SelectSubtitleTrack(Some(TrackId::new(
                    rejected_id,
                ))))
                .is_err(),
            "id {rejected_id} is not a subtitle track"
        );
    }
    assert_eq!(session.snapshot().selected_tracks.subtitle_track, None);
    let _rejection_events = session.take_events();

    session
        .dispatch_command(PlayerCommand::SelectSubtitleTrack(Some(TrackId::new(3))))
        .expect("an embedded subtitle track id is valid");

    assert_eq!(
        session.snapshot().selected_tracks.subtitle_track,
        Some(TrackId::new(3))
    );
    assert_eq!(count_subtitle_selected(&session.take_events()), 1);
}

#[test]
fn none_clears_the_selection_and_publishes_the_event_even_without_media() {
    let mut session = PlayerSession::default();
    // Выбор мог остаться от прежнего состояния: `None` обязан его снять.
    session.snapshot.selected_tracks.subtitle_track = Some(TrackId::new(5));

    session
        .dispatch_command(PlayerCommand::SelectSubtitleTrack(None))
        .expect("turning subtitles off is always valid");

    assert_eq!(session.snapshot().selected_tracks.subtitle_track, None);
    let events = session.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PlayerEvent::SubtitleTrackSelected(None)))
    );
}

#[test]
fn restore_of_an_unknown_subtitle_id_degrades_to_no_subtitles_and_still_applies() {
    let mut session = PlayerSession::default();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let request_id = MediaInstallRequestId::from_non_zero(NonZeroU64::new(701).expect("non-zero"));
    let media_instance_id = MediaInstanceId::from_non_zero(NonZeroU64::new(702).expect("non-zero"));
    session.playback_intent_control.register_staged_request(
        request_id,
        AcceptedPlaybackIntent {
            revision: PlaybackIntentRevision::INITIAL,
            intent: PlaybackIntent::StartPaused,
        },
    );
    session
        .playback_intent_control
        .commit_staged_request(request_id, media_instance_id, |_| {});
    session.snapshot.media_instance_id = Some(media_instance_id);
    let _install_events = session.take_events();

    let (outcome_tx, outcome_rx) = bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::Select(TrackId::new(99)),
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::KeepStart,
        },
        outcome_tx,
    );

    assert_eq!(
        outcome_rx.recv().expect("restore reports its outcome"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
    assert_eq!(session.snapshot().selected_tracks.subtitle_track, None);
    let events = session.take_events();
    assert_eq!(count_subtitle_selected(&events), 0);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::RecoverableError(_))),
        "a stale saved id is not an error the user should see"
    );
}
