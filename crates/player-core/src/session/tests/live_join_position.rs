//! F3: часы live-воспроизведения стартуют с позиции входа в эфир (hold-back источника),
//! а не с самого live edge. Иначе запас данных, который источник отдаёт «до края»,
//! обрезается по часам, и плеер голодает на каждом новом сегменте.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use media_core::{DynamicMediaTimelineEpoch, DynamicMediaTimelineState, MediaTime, TimelineRange};

use super::live_same_item_restore::{begin_live_restore, fake_live_media, install_correlated_live};
use crate::{InstalledLiveEdgeAdjustmentReason, InstalledMediaStateRestoreOutcome, PlayerSession};

/// Состояние без DVR с hold-back: край `edge`, вход за `hold_back` секунд до него.
fn held_back_state(edge: u64, hold_back: u64) -> DynamicMediaTimelineState {
    DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(edge))
        .with_join_position(MediaTime::from_secs(edge - hold_back))
        .expect("join position is not after live edge")
}

#[test]
fn initial_install_starts_clock_at_join_position_not_at_live_edge() {
    let (prepared_media, _publisher) =
        fake_live_media(61, held_back_state(40, 6), Arc::new(AtomicUsize::new(0)));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    let snapshot = session.snapshot();
    assert_eq!(snapshot.current_position, Duration::from_secs(34));
    // Public live edge остаётся настоящим краем эфира: UI видит, что мы отстаём на 6 с.
    assert_eq!(snapshot.timeline.live_edge, Some(MediaTime::from_secs(40)));
}

#[test]
fn join_position_also_applies_when_dvr_range_is_published() {
    let range = TimelineRange::new(MediaTime::from_secs(20), MediaTime::from_secs(40))
        .expect("ordered range");
    let state = DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(40), range)
        .expect("valid DVR")
        .with_join_position(MediaTime::from_secs(34))
        .expect("join inside edge");
    let (prepared_media, _publisher) = fake_live_media(62, state, Arc::new(AtomicUsize::new(0)));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    assert_eq!(session.snapshot().current_position, Duration::from_secs(34));
}

#[test]
fn source_without_hold_back_still_starts_at_live_edge() {
    let (prepared_media, _publisher) = fake_live_media(
        63,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(40)),
        Arc::new(AtomicUsize::new(0)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    assert_eq!(session.snapshot().current_position, Duration::from_secs(40));
}

#[test]
fn live_restore_without_dvr_lands_on_fresh_join_position() {
    let (prepared_media, publisher) =
        fake_live_media(64, held_back_state(100, 6), Arc::new(AtomicUsize::new(0)));
    let mut session = PlayerSession::new();
    let (request_id, media_instance_id) =
        install_correlated_live(&mut session, prepared_media, true, 641);
    publisher
        .publish(DynamicMediaTimelineEpoch::new(2), held_back_state(125, 6))
        .expect("fresh edge is published");

    let outcome_rx = begin_live_restore(
        &mut session,
        request_id,
        media_instance_id,
        Duration::from_secs(100),
    );

    assert_eq!(
        outcome_rx.recv().expect("no-DVR adjustment"),
        InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
            media_instance_id,
            requested_position: Duration::from_secs(100),
            live_edge: Duration::from_secs(119),
            reason: InstalledLiveEdgeAdjustmentReason::DvrWindowUnavailable,
        }
    );
    assert_eq!(
        session.snapshot().current_position,
        Duration::from_secs(119)
    );
}
