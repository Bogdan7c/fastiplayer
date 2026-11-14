use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    MediaTime, Packet, TimelineRange, dynamic_media_timeline,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::open::SelectedLiveResources;
use super::timeline_publication::{
    HlsStreamEnd, HlsTimelinePublication, PublishedHlsView, timeline_publication,
};
use super::{
    HlsLiveComponentSnapshot, HlsLiveRandomAccessSource, HlsLiveRefreshError,
    HlsLiveSegmentIdentity, HlsLiveTimelineEvidence, HlsLiveVideoDecodeStartEvidence, hold_back,
    rejoin,
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
    /// Докуда демуксеры дочитали, даже если те сегменты уже ушли из окна (решение 333).
    ///
    /// Доказательства пакетов живут только для сегментов текущего окна. Долгая пауза или
    /// склейка подряд оставили бы склейку без опоры, и она поставила бы окно далеко позади
    /// плеера. Поэтому перед каждой подменой снимков прочитанное «сворачивается» сюда, а
    /// склейка ставит сюда свою точку продолжения. Значение только растёт.
    read_until: std::time::Duration,
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
                read_until: join_position,
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
        check_replacement_fence(&state, source_epoch, replacement_transport.as_ref())?;
        commit_snapshots(&mut state, main, audio, source_epoch, replacement_transport)
    }

    /// Склейка эфира, потерявшего нить окна (решение 333, модуль `rejoin`).
    ///
    /// Всё делается под одной блокировкой: «докуда дочитано» берётся из того же состояния,
    /// которое тут же подменяется. Отметки демуксера о пакетах идут через этот же мьютекс,
    /// поэтому между замером и подменой прочитанное не сдвинется.
    ///
    /// Если ни один компонент ещё ничего не прочитал, продолжаем с позиции входа, с которой
    /// стартовали часы плеера.
    pub(super) fn rejoin_live_edge(
        &self,
        resources: &SelectedLiveResources,
        source_epoch: DynamicMediaTimelineEpoch,
        replacement_transport: Option<HlsLiveTransportSnapshot>,
    ) -> Result<HlsLiveRejoinOutcome, HlsLiveRejoinError> {
        let mut state = self.state.lock().map_err(|_| {
            HlsLiveRejoinError::Coordinator(anyhow::anyhow!(
                "HLS live timeline coordinator mutex poisoned"
            ))
        })?;
        check_replacement_fence(&state, source_epoch, replacement_transport.as_ref())
            .map_err(HlsLiveRejoinError::Coordinator)?;
        // Дальний из компонентов и из уже ушедших сегментов: тогда ни у одного компонента
        // метки времени не пойдут назад. Отставший компонент сам найдёт сегмент не раньше
        // своего прочитанного (`rejoin::first_segment_after`).
        let resume_at = read_until_now(&state);
        let previous_live_edge = shared_live_edge(&state.main, state.audio.as_ref());
        let rejoined =
            rejoin::rejoined_snapshots(resources, resume_at).map_err(HlsLiveRejoinError::Rejoin)?;
        let live_edge = shared_live_edge(&rejoined.main, rejoined.audio.as_ref());
        commit_snapshots(
            &mut state,
            rejoined.main,
            rejoined.audio,
            source_epoch,
            replacement_transport,
        )
        .map_err(HlsLiveRejoinError::Coordinator)?;
        // Из нового окна ещё ничего не прочитано, но продолжение — отсюда: вторая склейка
        // подряд не должна откатить окно назад.
        state.read_until = state.read_until.max(resume_at);
        Ok(HlsLiveRejoinOutcome {
            resume_at,
            join_position: rejoined.join_position,
            previous_live_edge: previous_live_edge.as_duration(),
            live_edge: live_edge.as_duration(),
        })
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

/// Что сделала склейка: для журнала и тестов, без адресов и содержимого плейлиста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsLiveRejoinOutcome {
    /// Докуда было дочитано: сюда поставлена точка входа нового окна.
    pub resume_at: Duration,
    /// Начало сегмента входа нового окна на шкале плеера.
    pub join_position: Duration,
    /// Край эфира до склейки.
    pub previous_live_edge: Duration,
    /// Край эфира после склейки.
    pub live_edge: Duration,
}

/// Почему склейка не состоялась: разные причины — разные роковые ошибки эфира.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HlsLiveRejoinError {
    /// Новый плейлист нельзя поставить на шкалу (переполнение, несовпадение плана).
    #[error("HLS live rejoin could not place the new window: {0}")]
    Rejoin(#[source] HlsLiveRefreshError),
    /// Состояние координатора недоступно или обновление устарело.
    #[error("HLS live rejoin was rejected by the timeline coordinator: {0}")]
    Coordinator(#[source] anyhow::Error),
}

/// Докуда дочитано на сейчас: прочитанное в текущем окне или запомненное раньше.
fn read_until_now(state: &HlsLiveTimelineCoordinatorState) -> Duration {
    let main_frontier = state.main_evidence.read_frontier(&state.main);
    let audio_frontier = state
        .audio
        .as_ref()
        .zip(state.audio_evidence.as_ref())
        .and_then(|(snapshot, evidence)| evidence.read_frontier(snapshot));
    main_frontier
        .max(audio_frontier)
        .map_or(state.read_until, |frontier| frontier.max(state.read_until))
}

/// Отвергает обновление, пришедшее из устаревшей эпохи или устаревшего транспорта.
fn check_replacement_fence(
    state: &HlsLiveTimelineCoordinatorState,
    source_epoch: DynamicMediaTimelineEpoch,
    replacement_transport: Option<&HlsLiveTransportSnapshot>,
) -> Result<()> {
    if source_epoch < state.source_epoch {
        return Err(anyhow::anyhow!("stale HLS live source epoch"));
    }
    if let Some(replacement_transport) = replacement_transport
        && replacement_transport.generation.value() <= state.transport.generation.value()
    {
        return Err(anyhow::anyhow!("stale HLS live transport generation"));
    }
    Ok(())
}

/// Подменяет снимки, чистит доказательства ушедших сегментов и публикует табло.
fn commit_snapshots(
    state: &mut HlsLiveTimelineCoordinatorState,
    main: HlsLiveComponentSnapshot,
    audio: Option<HlsLiveComponentSnapshot>,
    source_epoch: DynamicMediaTimelineEpoch,
    replacement_transport: Option<HlsLiveTransportSnapshot>,
) -> Result<()> {
    // Прочитанное запоминается до того, как доказательства ушедших сегментов удалятся.
    state.read_until = read_until_now(state);
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
    publish_latest(state)
}
