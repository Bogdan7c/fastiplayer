use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    MediaTime, Packet, TimelineRange, dynamic_media_timeline,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::timeline_publication::{
    HlsStreamEnd, HlsTimelinePublication, PublishedHlsView, timeline_publication,
};
use super::{
    HlsLiveComponentSnapshot, HlsLiveRandomAccessSource, HlsLiveSegmentIdentity,
    HlsLiveTimelineEvidence, HlsLiveVideoDecodeStartEvidence, hold_back,
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
    /// Позиция входа в эфир (hold-back), выбранная один раз при открытии. Она не
    /// пересчитывается на refresh: часы плеера уже стартовали с неё, а demux читает
    /// с соответствующего ей сегмента.
    join_position: std::time::Duration,
    /// Какой вид табло уже опубликован: после записи эфир не возвращается (16d1).
    published_view: PublishedHlsView,
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
        main_random_access: HlsLiveRandomAccessSource,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
    ) -> (Arc<Self>, DynamicMediaTimelinePort) {
        let live_edge = shared_live_edge(&main, audio.as_ref());
        let join_position = hold_back::join_position(&main, audio.as_ref());
        let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation,
            source_epoch,
            state: with_join(
                DynamicMediaTimelineState::without_dvr(live_edge),
                join_position,
            )
            .into(),
        });
        let audio_evidence = audio
            .as_ref()
            .map(|_| HlsLiveTimelineEvidence::new(HlsLiveRandomAccessSource::EveryAudioPacket));
        let coordinator = Arc::new(Self {
            state: Mutex::new(HlsLiveTimelineCoordinatorState {
                transport: HlsLiveTransportSnapshot { http, generation },
                main,
                audio,
                main_evidence: HlsLiveTimelineEvidence::new(main_random_access),
                audio_evidence,
                source_epoch,
                publisher,
                join_position,
                published_view: PublishedHlsView::Live,
            }),
        });
        (coordinator, port)
    }

    /// Позиция входа в эфир, выбранная при создании coordinator-а (см. `hold_back`).
    pub fn join_position(&self) -> Result<std::time::Duration> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        Ok(state.join_position)
    }

    /// Фиксирует random-access политику main-компонента по доказанной probe-ом форме дорожек.
    ///
    /// Owner знания о форме - открытый main demux; coordinator лишь принимает вывод и не
    /// заглядывает в дорожки сам. Вызывается один раз сразу после открытия, до первых пакетов.
    pub fn prove_main_random_access(&self, random_access: HlsLiveRandomAccessSource) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live timeline coordinator mutex poisoned"))?;
        state.main_evidence.set_random_access(random_access);
        Ok(())
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
                state.audio_evidence = Some(HlsLiveTimelineEvidence::new(
                    HlsLiveRandomAccessSource::EveryAudioPacket,
                ));
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
            .context("HLS live audio evidence is missing")?
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
                .context("HLS live audio evidence is missing")?
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

/// Единственная точка публикации табло: эфир, а после `ENDLIST` — запись (решение 325).
fn publish_latest(state: &mut HlsLiveTimelineCoordinatorState) -> Result<()> {
    let live_edge = shared_live_edge(&state.main, state.audio.as_ref());
    let main_range = state.main_evidence.proven_range(&state.main);
    let audio_range = state
        .audio
        .as_ref()
        .zip(state.audio_evidence.as_ref())
        .and_then(|(snapshot, evidence)| evidence.proven_range(snapshot));
    let proven_range = match (main_range, state.audio.as_ref(), audio_range) {
        (Some(main), None, _) => Some(main),
        (Some(main), Some(_), Some(audio)) => intersect_ranges(main, audio),
        _ => None,
    };
    let stream_end = HlsStreamEnd::of(&state.main, state.audio.as_ref());
    match timeline_publication(stream_end, state.published_view, live_edge, proven_range) {
        HlsTimelinePublication::Live(timeline) => {
            let timeline = with_join(timeline, state.join_position);
            state
                .publisher
                .publish(state.source_epoch, timeline)
                .context("HLS live timeline publisher rejected refresh")?;
        }
        HlsTimelinePublication::Recording(recording) => {
            state
                .publisher
                .publish_view(state.source_epoch, recording.into())
                .context("HLS live timeline publisher rejected the ended stream")?;
            state.published_view = PublishedHlsView::Recording;
        }
        HlsTimelinePublication::KeepPublishedRecording => {}
    }
    Ok(())
}

/// Добавляет hold-back позицию входа к состоянию.
///
/// Позиция берётся из тех же snapshot-ов, что и live edge, но edge общий для main/audio
/// (минимум), поэтому позицию прижимаем к нему: контракт media-core запрещает join правее
/// edge. После прижимания `with_join_position` не может отказать; запасной возврат
/// исходного состояния (join = edge) нужен только чтобы не вводить панику в production.
fn with_join(
    state: DynamicMediaTimelineState,
    join_position: std::time::Duration,
) -> DynamicMediaTimelineState {
    let join_position = join_position.min(state.live_edge().as_duration());
    state
        .with_join_position(MediaTime::from_duration(join_position))
        .unwrap_or(state)
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
