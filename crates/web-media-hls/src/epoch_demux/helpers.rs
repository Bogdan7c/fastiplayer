//! Чистые helpers packet landing, replay accounting и stable track layout.

use media_core::{DemuxReadEvent, MediaTime, Packet, PacketKeyframe, TrackInfo, TrackKind};

use crate::seek::{HlsSeekAnchor, HlsSeekAnchorKind};

pub(super) fn packet_matches_anchor(packet: &Packet, anchor: HlsSeekAnchor) -> bool {
    let kind_matches = match anchor.kind {
        HlsSeekAnchorKind::VideoRandomAccessPoint => {
            packet.kind == TrackKind::Video && packet.keyframe == PacketKeyframe::Keyframe
        }
        HlsSeekAnchorKind::AudioPacket => packet.kind == TrackKind::Audio,
    };
    kind_matches && MediaTime::from_duration(packet.pts) == anchor.position
}

/// Сохраняет только audio, которое по presentation time уже принадлежит выбранному video RAP.
pub(super) fn packet_is_replayable_after_video_anchor(
    packet: &Packet,
    anchor: HlsSeekAnchor,
) -> bool {
    anchor.kind == HlsSeekAnchorKind::VideoRandomAccessPoint
        && packet.kind == TrackKind::Audio
        && MediaTime::from_duration(packet.pts) >= anchor.position
}

pub(super) fn event_encoded_bytes(event: &DemuxReadEvent) -> usize {
    match event {
        DemuxReadEvent::Packet(packet) => packet.data.len(),
        DemuxReadEvent::EndOfStream
        | DemuxReadEvent::TracksChanged(_)
        | DemuxReadEvent::MediaMetadataChanged(_)
        | DemuxReadEvent::TemporarilyUnavailable(_) => 0,
    }
}

pub(super) fn layout_ordinals(tracks: &[TrackInfo]) -> Vec<(TrackKind, usize)> {
    let mut video_index = 0;
    let mut audio_index = 0;
    tracks
        .iter()
        .map(|track| match track.kind {
            TrackKind::Video => {
                let ordinal = video_index;
                video_index += 1;
                (TrackKind::Video, ordinal)
            }
            TrackKind::Audio => {
                let ordinal = audio_index;
                audio_index += 1;
                (TrackKind::Audio, ordinal)
            }
        })
        .collect()
}
