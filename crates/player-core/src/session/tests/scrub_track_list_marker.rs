//! A13-2: TracksChanged после принятого demux seek-а принадлежит самому live-scrub seek-у
//! и не должен его отменять (иначе видео без звука навсегда остаётся на старой позиции).

use super::test_support::*;
use super::*;

/// Поднимает live-scrub landing: demuxer уже принял seek, scrub ещё активен.
fn session_with_accepted_live_scrub_landing() -> PlayerSession {
    let mut session = PlayerSession::new();
    install_fake_media_with_seek_request_log(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let _ = session.take_events();
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();
    assert!(session.seek_commit().is_some(), "seek must be accepted");
    session
}

fn same_topology_update() -> media_core::DemuxTrackListUpdate {
    media_core::DemuxTrackListUpdate::new(
        vec![fake_track(1, TrackKind::Video)],
        Some(Duration::from_secs(30)),
    )
}

/// Первый TracksChanged после принятого seek-а - маркер этого же seek-а: цель перемотки
/// сохраняется, scrub остаётся активным, seek-транзакция переживает сброс пайплайна.
#[test]
fn first_track_list_marker_keeps_active_live_scrub_seek() {
    let mut session = session_with_accepted_live_scrub_landing();

    session.handle_demux_track_list_update(same_topology_update());

    assert!(
        session.seek_commit().is_some(),
        "The seek marker itself should not cancel the active rewinding."
    );
    assert!(session.snapshot().timeline.scrubbing);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(8)),
        "rewinding target must be maintained"
    );
}

/// Второй (и дальнейшие) TracksChanged за время того же scrub-а - не маркер seek-а,
/// а настоящая смена дорожек: прежняя защита S19 отменяет scrub без hidden commit-а.
#[test]
fn second_track_list_update_still_cancels_live_scrub() {
    let mut session = session_with_accepted_live_scrub_landing();
    session.handle_demux_track_list_update(same_topology_update());

    session.handle_demux_track_list_update(same_topology_update());

    assert!(session.seek_commit().is_none());
    assert!(!session.snapshot().timeline.scrubbing);
}
