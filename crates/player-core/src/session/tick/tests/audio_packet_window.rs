use bytes::Bytes;
use media_core::{
    ExactPresentationWindow, Packet, PacketPresentationWindow, TimeBase, TrackId, TrackKind,
    TrackTimestamp,
};

use super::*;

#[test]
fn bounded_packet_window_reaches_pending_audio_queue_unchanged() {
    let mut session = PlayerSession::new();
    let track_id = TrackId::new(2);
    let time_base = TimeBase::new(1, 48_000).expect("test time base должна быть валидной");
    let exact_window = ExactPresentationWindow::new(
        TrackTimestamp::new(track_id, 480, time_base),
        TrackTimestamp::new(track_id, 1_440, time_base),
    )
    .expect("test presentation window должно быть валидным");
    let packet = Packet::new_unbounded(
        track_id,
        TrackKind::Audio,
        Duration::from_millis(10),
        None,
        false,
        Bytes::from_static(b"audio"),
    )
    .with_track_timestamps(Some(TrackTimestamp::new(track_id, 480, time_base)), None)
    .try_with_bounded_presentation_window(exact_window)
    .expect("packet и exact window должны иметь один presentation clock");

    assert_eq!(
        route_demuxed_packet(&mut session, packet),
        DemuxPacketRouteOutcome::Queued
    );

    let pending_packet = session
        .pipeline
        .pop_pending_audio_packet_front()
        .expect("audio packet должен попасть в pending queue");
    assert_eq!(
        pending_packet.presentation_window(),
        PacketPresentationWindow::Bounded(exact_window)
    );
}
