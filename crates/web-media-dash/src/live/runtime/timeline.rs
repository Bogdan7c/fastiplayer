//! Публикация S31L timeline только из реально доказанных packet/RAP диапазонов.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    MediaTime, Packet, TimelineRange, TrackKind, dynamic_media_timeline,
};

use super::DashLiveAvailability;

/// S31L publisher и proven packet/RAP evidence.
pub(super) struct DashLiveTimelineCoordinator {
    state: Mutex<DashLiveTimelineState>,
}

/// Всё состояние публикации принадлежит coordinator-у и меняется атомарно.
struct DashLiveTimelineState {
    availability: DashLiveAvailability,
    video_required: bool,
    audio_required: bool,
    video: Option<DashPacketEvidence>,
    audio: Option<DashPacketEvidence>,
    source_epoch: DynamicMediaTimelineEpoch,
    publisher: DynamicMediaTimelinePublisher,
}

/// Непрерывный подтверждённый диапазон одного component-а.
#[derive(Clone, Copy)]
struct DashPacketEvidence {
    range: TimelineRange,
}

impl DashLiveTimelineCoordinator {
    /// Создаёт neutral port до публикации runtime.
    pub(super) fn new(
        availability: DashLiveAvailability,
        has_video: bool,
        has_audio: bool,
        port_generation: DynamicMediaTimelinePortGeneration,
        source_epoch: DynamicMediaTimelineEpoch,
    ) -> Result<(Arc<Self>, DynamicMediaTimelinePort)> {
        let initial_timeline = DynamicMediaTimelineState::with_available_dvr(
            availability.live_edge,
            availability.manifest_range,
        )
        .context("DASH initial availability violated neutral timeline contract")?;
        let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation,
            source_epoch,
            state: initial_timeline,
        });
        Ok((
            Arc::new(Self {
                state: Mutex::new(DashLiveTimelineState {
                    availability,
                    video_required: has_video,
                    audio_required: has_audio,
                    video: None,
                    audio: None,
                    source_epoch,
                    publisher,
                }),
            }),
            port,
        ))
    }

    /// Обновляет manifest cap и отбрасывает expired evidence.
    pub(super) fn replace_availability(&self, availability: DashLiveAvailability) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live timeline mutex poisoned"))?;
        let manifest_range = availability.manifest_range;
        state.availability = availability;
        clamp_video_evidence(&mut state.video, manifest_range);
        clamp_evidence(&mut state.audio, manifest_range);
        publish_timeline(&mut state)
    }

    /// Наблюдает actual packet; video range начинается только с RAP.
    pub(super) fn observe_packet(&self, packet: &Packet) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live timeline mutex poisoned"))?;
        let packet_start = MediaTime::from_duration(packet.pts);
        let packet_end = MediaTime::from_duration(
            packet
                .duration
                .and_then(|duration| packet.pts.checked_add(duration))
                .unwrap_or(packet.pts),
        );
        let manifest = state.availability.manifest_range;
        if packet_end <= manifest.start || packet_start >= manifest.end {
            return Ok(());
        }
        let slot = match packet.kind {
            TrackKind::Video => &mut state.video,
            TrackKind::Audio => &mut state.audio,
        };
        let start = packet_start.max(manifest.start);
        let end = packet_end.min(manifest.end);
        match slot {
            Some(evidence) if start <= evidence.range.end => {
                evidence.range.end = evidence.range.end.max(end);
            }
            Some(_) => {
                *slot = (packet.kind != TrackKind::Video || packet.keyframe.is_known_keyframe())
                    .then_some(DashPacketEvidence {
                        range: TimelineRange { start, end },
                    });
            }
            None if packet.kind != TrackKind::Video || packet.keyframe.is_known_keyframe() => {
                *slot = Some(DashPacketEvidence {
                    range: TimelineRange { start, end },
                });
            }
            None => {}
        }
        publish_timeline(&mut state)
    }
}

/// Video start нельзя сдвинуть внутрь GOP: expired RAP invalidates весь evidence.
fn clamp_video_evidence(slot: &mut Option<DashPacketEvidence>, cap: TimelineRange) {
    if slot.is_some_and(|evidence| {
        evidence.range.start < cap.start
            || evidence.range.start >= cap.end
            || evidence.range.end <= cap.start
    }) {
        *slot = None;
        return;
    }
    clamp_evidence(slot, cap);
}

/// Clamps actual evidence к sliding manifest cap.
fn clamp_evidence(slot: &mut Option<DashPacketEvidence>, cap: TimelineRange) {
    let Some(evidence) = slot else {
        return;
    };
    let start = evidence.range.start.max(cap.start);
    let end = evidence.range.end.min(cap.end);
    if start < end {
        evidence.range = TimelineRange { start, end };
    } else {
        *slot = None;
    }
}

/// Публикует DVR только при наличии всех required component evidence.
fn publish_timeline(state: &mut DashLiveTimelineState) -> Result<()> {
    let proven = match (state.video_required, state.audio_required) {
        (true, true) => state
            .video
            .zip(state.audio)
            .and_then(|(video, audio)| intersect_evidence(video.range, audio.range)),
        (true, false) => state.video.map(|video| video.range),
        (false, true) => state.audio.map(|audio| audio.range),
        (false, false) => None,
    };
    let timeline = match proven {
        Some(range) if range.start < range.end && range.end <= state.availability.live_edge => {
            DynamicMediaTimelineState::with_available_and_seekable_dvr(
                state.availability.live_edge,
                state.availability.manifest_range,
                range,
            )
            .context("DASH proven DVR violated S31L")?
        }
        _ => DynamicMediaTimelineState::with_available_dvr(
            state.availability.live_edge,
            state.availability.manifest_range,
        )
        .context("DASH availability violated S31L")?,
    };
    state
        .publisher
        .publish(state.source_epoch, timeline)
        .context("DASH S31L publisher rejected state")?;
    Ok(())
}

fn intersect_evidence(left: TimelineRange, right: TimelineRange) -> Option<TimelineRange> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);
    (start < end).then_some(TimelineRange { start, end })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::time::Duration;

    use bytes::Bytes;
    use media_core::{
        DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration, MediaTime, Packet, TrackId,
        TrackKind,
    };

    use super::{DashLiveAvailability, DashLiveTimelineCoordinator};

    /// Формирует packet с exact duration для проверки evidence intersection.
    fn packet(
        kind: TrackKind,
        start_seconds: u64,
        duration_seconds: u64,
        keyframe: bool,
    ) -> Packet {
        let mut packet = Packet::new_unbounded(
            TrackId::new(1),
            kind,
            Duration::from_secs(start_seconds),
            None,
            keyframe,
            Bytes::new(),
        );
        packet.duration = Some(Duration::from_secs(duration_seconds));
        packet
    }

    #[test]
    fn av_dvr_is_withheld_until_video_rap_and_then_uses_proven_intersection() {
        let availability = DashLiveAvailability {
            live_edge: MediaTime::from_duration(Duration::from_secs(10)),
            manifest_range: media_core::TimelineRange {
                start: MediaTime::ZERO,
                end: MediaTime::from_duration(Duration::from_secs(10)),
            },
        };
        let generation = DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("test generation is non-zero"),
        );
        let (coordinator, port) = DashLiveTimelineCoordinator::new(
            availability,
            true,
            true,
            generation,
            DynamicMediaTimelineEpoch::new(0),
        )
        .expect("valid initial availability");

        assert_eq!(
            port.observe().snapshot.state.availability_range(),
            Some(media_core::TimelineRange {
                start: MediaTime::ZERO,
                end: MediaTime::from_duration(Duration::from_secs(10)),
            })
        );

        coordinator
            .observe_packet(&packet(TrackKind::Audio, 1, 7, false))
            .expect("audio evidence is accepted");
        coordinator
            .observe_packet(&packet(TrackKind::Video, 2, 4, false))
            .expect("non-RAP video packet is observed");
        assert_eq!(port.observe().snapshot.state.seekable_range(), None);

        coordinator
            .observe_packet(&packet(TrackKind::Video, 2, 4, true))
            .expect("video RAP starts evidence");
        assert_eq!(
            port.observe().snapshot.state.seekable_range(),
            Some(media_core::TimelineRange {
                start: MediaTime::from_duration(Duration::from_secs(2)),
                end: MediaTime::from_duration(Duration::from_secs(6)),
            })
        );

        coordinator
            .replace_availability(DashLiveAvailability {
                live_edge: MediaTime::from_duration(Duration::from_secs(10)),
                manifest_range: media_core::TimelineRange {
                    start: MediaTime::from_duration(Duration::from_secs(3)),
                    end: MediaTime::from_duration(Duration::from_secs(10)),
                },
            })
            .expect("sliding availability is accepted");
        assert_eq!(port.observe().snapshot.state.seekable_range(), None);
        assert_eq!(
            port.observe().snapshot.state.availability_range(),
            Some(media_core::TimelineRange {
                start: MediaTime::from_duration(Duration::from_secs(3)),
                end: MediaTime::from_duration(Duration::from_secs(10)),
            })
        );

        coordinator
            .observe_packet(&packet(TrackKind::Video, 8, 1, true))
            .expect("non-contiguous video RAP replaces old evidence");
        coordinator
            .observe_packet(&packet(TrackKind::Audio, 8, 1, false))
            .expect("audio evidence reaches the new RAP range");
        assert_eq!(
            port.observe().snapshot.state.seekable_range(),
            Some(media_core::TimelineRange {
                start: MediaTime::from_duration(Duration::from_secs(8)),
                end: MediaTime::from_duration(Duration::from_secs(9)),
            })
        );
    }
}
