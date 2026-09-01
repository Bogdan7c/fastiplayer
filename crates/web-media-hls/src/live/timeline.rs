use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    MediaTime, Packet, TimelineRange, dynamic_media_timeline,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::{
    HlsLiveComponentSnapshot, HlsLiveSegmentIdentity, HlsLiveTimelineEvidence,
    HlsLiveVideoDecodeStartEvidence,
};

/// Общий owner main/audio snapshots, evidence и neutral timeline publication.
pub(crate) struct HlsLiveTimelineCoordinator {
    state: Mutex<HlsLiveTimelineCoordinatorState>,
}

struct HlsLiveTimelineCoordinatorState {
    transport: HlsLiveTransportSnapshot,
    main: HlsLiveComponentSnapshot,
    audio: Option<HlsLiveComponentSnapshot>,
    main_evidence: HlsLiveTimelineEvidence,
    audio_evidence: Option<HlsLiveTimelineEvidence>,
    source_epoch: DynamicMediaTimelineEpoch,
    publisher: DynamicMediaTimelinePublisher,
}

#[derive(Clone)]
pub(crate) struct HlsLiveTransportSnapshot {
    pub http: AdaptiveHttpContext,
    pub generation: SourceGeneration,
}

impl HlsLiveTimelineCoordinator {
    /// Создаёт связанный neutral port и live owner до публикации demux runtime.
    pub fn new(
        main: HlsLiveComponentSnapshot,
        audio: Option<HlsLiveComponentSnapshot>,
        port_generation: DynamicMediaTimelinePortGeneration,
        source_epoch: DynamicMediaTimelineEpoch,
        main_has_video: bool,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
    ) -> (Arc<Self>, DynamicMediaTimelinePort) {
        let live_edge = shared_live_edge(&main, audio.as_ref());
        let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation,
            source_epoch,
            state: DynamicMediaTimelineState::without_dvr(live_edge),
        });
        let audio_evidence = audio.as_ref().map(|_| HlsLiveTimelineEvidence::new(false));
        let coordinator = Arc::new(Self {
            state: Mutex::new(HlsLiveTimelineCoordinatorState {
                transport: HlsLiveTransportSnapshot { http, generation },
                main,
                audio,
                main_evidence: HlsLiveTimelineEvidence::new(main_has_video),
                audio_evidence,
                source_epoch,
                publisher,
            }),
        });
        (coordinator, port)
    }

    /// Атомарно заменяет main/audio snapshots одного accepted refresh.
    pub fn replace_snapshots(
        &self,
        main: HlsLiveComponentSnapshot,
        audio: Option<HlsLiveComponentSnapshot>,
        source_epoch: DynamicMediaTimelineEpoch,
        replacement_transport: Option<HlsLiveTransportSnapshot>,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        if source_epoch < state.source_epoch {
            return Err(anyhow::anyhow!("stale HLS live source epoch"));
        }
        if let Some(replacement_transport) = replacement_transport.as_ref()
            && replacement_transport.generation.value() <= state.transport.generation.value()
        {
            return Err(anyhow::anyhow!("stale HLS live transport generation"));
        }
        state.main_evidence.retain_snapshot(&main);
        match (&mut state.audio_evidence, &audio) {
            (Some(evidence), Some(snapshot)) => evidence.retain_snapshot(snapshot),
            (None, Some(_)) => {
                state.audio_evidence = Some(HlsLiveTimelineEvidence::new(false));
            }
            (_, None) => state.audio_evidence = None,
        }
        state.main = main;
        state.audio = audio;
        state.source_epoch = source_epoch;
        if let Some(replacement_transport) = replacement_transport {
            state.transport = replacement_transport;
        }
        publish_latest(&mut state)
    }

    /// Принимает packet только вместе с exact source segment identity.
    pub fn observe_main_packet(
        &self,
        identity: HlsLiveSegmentIdentity,
        packet: &Packet,
        video_decode_start: HlsLiveVideoDecodeStartEvidence,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        state.main_evidence.observe_packet_with_video_decode_start(
            identity,
            packet,
            video_decode_start,
        );
        publish_latest(&mut state)
    }

    /// Принимает alternate-audio packet вместе с его независимой identity.
    pub fn observe_audio_packet(
        &self,
        identity: HlsLiveSegmentIdentity,
        packet: &Packet,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        state
            .audio_evidence
            .as_mut()
            .context("HLS live audio evidence отсутствует")?
            .observe_packet(identity, packet);
        publish_latest(&mut state)
    }

    /// Удаляет доказательство сразу после observed 404/410 конкретного segment-а.
    pub fn expire_segment(&self, identity: HlsLiveSegmentIdentity, is_audio: bool) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        if is_audio {
            state
                .audio_evidence
                .as_mut()
                .context("HLS live audio evidence отсутствует")?
                .expire(identity);
        } else {
            state.main_evidence.expire(identity);
        }
        publish_latest(&mut state)
    }

    pub fn main_snapshot(&self) -> Result<HlsLiveComponentSnapshot> {
        self.state
            .lock()
            .map(|state| state.main.clone())
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))
    }

    pub fn main_runtime_snapshot(
        &self,
    ) -> Result<(HlsLiveComponentSnapshot, HlsLiveTransportSnapshot)> {
        self.state
            .lock()
            .map(|state| (state.main.clone(), state.transport.clone()))
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))
    }

    pub fn audio_snapshot(&self) -> Result<Option<HlsLiveComponentSnapshot>> {
        self.state
            .lock()
            .map(|state| state.audio.clone())
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))
    }

    pub fn audio_runtime_snapshot(
        &self,
    ) -> Result<Option<(HlsLiveComponentSnapshot, HlsLiveTransportSnapshot)>> {
        self.state
            .lock()
            .map(|state| {
                state
                    .audio
                    .clone()
                    .map(|audio| (audio, state.transport.clone()))
            })
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))
    }

    pub fn main_anchor_for(
        &self,
        target: std::time::Duration,
    ) -> Result<Option<(HlsLiveSegmentIdentity, std::time::Duration)>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        Ok(state.main_evidence.anchor_for(&state.main, target))
    }

    pub fn audio_anchor_for(
        &self,
        target: std::time::Duration,
    ) -> Result<Option<(HlsLiveSegmentIdentity, std::time::Duration)>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        Ok(state
            .audio_evidence
            .as_ref()
            .zip(state.audio.as_ref())
            .and_then(|(evidence, snapshot)| evidence.anchor_for(snapshot, target)))
    }
}

fn publish_latest(state: &mut HlsLiveTimelineCoordinatorState) -> Result<()> {
    let live_edge = shared_live_edge(&state.main, state.audio.as_ref());
    let main_range = state.main_evidence.proven_range(&state.main);
    let audio_range = state
        .audio
        .as_ref()
        .zip(state.audio_evidence.as_ref())
        .and_then(|(snapshot, evidence)| evidence.proven_range(snapshot));
    let seekable_range = match (main_range, state.audio.as_ref(), audio_range) {
        (Some(main), None, _) => Some(main),
        (Some(main), Some(_), Some(audio)) => intersect_ranges(main, audio),
        _ => None,
    };
    let timeline = match seekable_range {
        Some(range) if range.start < range.end && range.end <= live_edge => {
            DynamicMediaTimelineState::with_dvr(live_edge, range)
                .context("HLS live DVR intersection violated neutral timeline contract")?
        }
        _ => DynamicMediaTimelineState::without_dvr(live_edge),
    };
    state
        .publisher
        .publish(state.source_epoch, timeline)
        .context("HLS live timeline publisher rejected refresh")?;
    Ok(())
}

fn shared_live_edge(
    main: &HlsLiveComponentSnapshot,
    audio: Option<&HlsLiveComponentSnapshot>,
) -> MediaTime {
    let edge = audio.map_or(main.manifest_live_edge, |audio| {
        main.manifest_live_edge.min(audio.manifest_live_edge)
    });
    MediaTime::from_duration(edge)
}

fn intersect_ranges(left: TimelineRange, right: TimelineRange) -> Option<TimelineRange> {
    let range = TimelineRange {
        start: left.start.max(right.start),
        end: left.end.min(right.end),
    };
    (range.start < range.end).then_some(range)
}
