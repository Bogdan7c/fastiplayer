//! Worker-owned P4 open, readiness proof и neutral A/V composition.

use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use demux_api::{
    CompositeAvDemuxer, CompositeAvPublicTrackIds, CompositeAvTrackSelection,
    ProgressiveAsyncSeekHandle, ProgressiveDemuxer, ProgressiveRuntimeGeneration,
    ProgressiveSeekController,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaMetadata,
    TrackId, TrackInfo, TrackKind,
};
use smooth_streaming_manifest_core::SmoothTime;
use web_media_core::{ComponentVariantCatalog, ComponentVariantSelection};
use web_media_transport_api::SourceGeneration;

use crate::source::{SmoothBuiltSourcePair, SmoothSelectedSourceFactory};
use crate::{SmoothAlignedSpan, SmoothSelectedFragmentSources};

use super::error::SmoothVodDemuxBuildError;
use super::factory::{
    SmoothAudioDemuxOpenRequest, SmoothIsoBmffDemuxFactory, SmoothVideoDemuxOpenRequest,
};
use super::policy::SmoothVodDemuxPolicy;
use super::seek::{SmoothSeekPlan, smooth_ticks_to_duration};

/// Prepared nonblocking Smooth VOD runtime плюс retained C3 projection.
pub struct SmoothVodOpenResult {
    catalog: ComponentVariantCatalog,
    selection: ComponentVariantSelection,
    source_generation: SourceGeneration,
    aligned_span: SmoothAlignedSpan,
    duration: Duration,
    demuxer: ProgressiveDemuxer,
}

impl SmoothVodOpenResult {
    /// Возвращает retained immutable catalog для UI/reopen.
    #[must_use]
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        &self.catalog
    }

    /// Возвращает canonical installed C3 selection.
    #[must_use]
    pub const fn selection(&self) -> &ComponentVariantSelection {
        &self.selection
    }

    /// Возвращает source generation transport-а.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }

    /// Возвращает exact manifest component span evidence.
    #[must_use]
    pub const fn aligned_span(&self) -> SmoothAlignedSpan {
        self.aligned_span
    }

    /// Возвращает VOD duration, представленную стандартным runtime clock.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Возвращает cloneable generation-fenced seek control до type erasure.
    #[must_use]
    pub fn async_seek_handle(&self) -> ProgressiveAsyncSeekHandle {
        self.demuxer
            .async_seek_handle()
            .expect("Smooth VOD runtime всегда создаётся с receipt capability")
    }

    /// Передаёт player composition только neutral demuxer.
    #[must_use]
    pub fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        Box::new(self.demuxer)
    }
}

impl fmt::Debug for SmoothVodOpenResult {
    /// Debug не раскрывает selected identities или source internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothVodOpenResult")
            .field("source_generation", &self.source_generation)
            .field("aligned_span", &self.aligned_span)
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

impl SmoothSelectedFragmentSources {
    /// Запускает blocking S28A open и дальнейший demux только на worker-е.
    pub fn into_progressive_demuxer(
        self,
        factory: Arc<dyn SmoothIsoBmffDemuxFactory>,
        policy: SmoothVodDemuxPolicy,
    ) -> std::result::Result<SmoothVodOpenResult, SmoothVodDemuxBuildError> {
        let (
            catalog,
            selection,
            source_generation,
            aligned_span,
            video_source,
            audio_source,
            source_factory,
        ) = self.into_demux_parts();
        let cancellation = video_source.cancellation().clone();
        let worker_cancellation = cancellation.clone();
        let duration = smooth_time_to_duration(aligned_span.end_exclusive());
        let preview_factory = source_factory.clone();
        let seek_controller = ProgressiveSeekController::new(move |request| {
            Ok(SmoothSeekPlan::for_request(&preview_factory, request, duration)?.result)
        });

        let demuxer = ProgressiveDemuxer::new_deferred_receipted_seekable(
            move || {
                let runtime = SmoothTransactionalVodDemuxer::new(
                    SmoothBuiltSourcePair {
                        video: video_source,
                        audio: audio_source,
                    },
                    source_factory,
                    factory,
                    worker_cancellation,
                    policy,
                    duration,
                )?;
                Ok(Box::new(runtime) as Box<dyn Demuxer + Send>)
            },
            seek_controller,
            cancellation,
            policy.progressive_limits,
            policy.retry_hint,
            ProgressiveRuntimeGeneration::new(source_generation.value()),
            policy.asynchronous_seek_limits,
        )
        .map_err(SmoothVodDemuxBuildError::ProgressiveStartup)?;

        Ok(SmoothVodOpenResult {
            catalog,
            selection,
            source_generation,
            aligned_span,
            duration,
            demuxer,
        })
    }
}

/// Worker-owned active composite и immutable replacement ingredients.
struct SmoothTransactionalVodDemuxer {
    current: CompositeAvDemuxer,
    source_factory: SmoothSelectedSourceFactory,
    adapter_factory: Arc<dyn SmoothIsoBmffDemuxFactory>,
    cancellation: source_core::CancellationToken,
    policy: SmoothVodDemuxPolicy,
    public_track_ids: CompositeAvPublicTrackIds,
    duration: Duration,
}

impl SmoothTransactionalVodDemuxer {
    /// Открывает initial pair и фиксирует collision-safe public track IDs.
    fn new(
        initial_sources: SmoothBuiltSourcePair,
        source_factory: SmoothSelectedSourceFactory,
        adapter_factory: Arc<dyn SmoothIsoBmffDemuxFactory>,
        cancellation: source_core::CancellationToken,
        policy: SmoothVodDemuxPolicy,
        duration: Duration,
    ) -> anyhow::Result<Self> {
        let opened = open_components(
            initial_sources,
            adapter_factory.as_ref(),
            cancellation.clone(),
            policy.sniff_budget,
        )?;
        let current = CompositeAvDemuxer::new(
            opened.video,
            opened.audio,
            CompositeAvTrackSelection::new(opened.video_track_id, opened.audio_track_id),
            policy.lead_policy,
        )
        .context("Smooth initial A/V composition readiness failed")?;
        let public_track_ids = CompositeAvPublicTrackIds::new(
            current.public_video_track_id(),
            current.public_audio_track_id(),
        );
        Ok(Self {
            current,
            source_factory,
            adapter_factory,
            cancellation,
            policy,
            public_track_ids,
            duration,
        })
    }
}

impl Demuxer for SmoothTransactionalVodDemuxer {
    /// Делегирует stable merged tracks.
    fn tracks(&self) -> &[TrackInfo] {
        self.current.tracks()
    }

    /// Публикует authoritative root duration вместо zero-duration fMP4 init.
    fn duration(&self) -> Option<Duration> {
        Some(self.duration)
    }

    /// Делегирует merged metadata.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.current.media_metadata()
    }

    /// P5 поддерживает только atomic replacement внутри progressive worker-а.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Делегирует packet/readiness interleave.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        self.current.next_event()
    }

    /// Legacy boundary остаётся точным Accurate request.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    /// Готовит обе оси offside и публикует replacement единственным swap-ом.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        let plan = SmoothSeekPlan::for_request(&self.source_factory, request, self.duration)?;
        let replacement_sources = self
            .source_factory
            .build_at(plan.video_fragment_index, plan.audio_fragment_index)?;
        let opened = open_components(
            replacement_sources,
            self.adapter_factory.as_ref(),
            self.cancellation.clone(),
            self.policy.sniff_budget,
        )?;
        let replacement = CompositeAvDemuxer::new_with_public_track_ids(
            opened.video,
            opened.audio,
            CompositeAvTrackSelection::new(opened.video_track_id, opened.audio_track_id),
            self.public_track_ids,
            self.policy.lead_policy,
        )
        .context("Smooth replacement A/V composition readiness failed")?;
        if replacement.tracks() != self.current.tracks() {
            bail!("Smooth replacement changed stable public track layout");
        }
        self.current = replacement;
        Ok(plan.result)
    }
}

/// Открытая pair до composite publication.
struct SmoothOpenedComponents {
    video: Box<dyn Demuxer + Send>,
    audio: Box<dyn Demuxer + Send>,
    video_track_id: TrackId,
    audio_track_id: TrackId,
}

/// Полностью открывает обе component axis до любого active-state mutation.
fn open_components(
    sources: SmoothBuiltSourcePair,
    factory: &dyn SmoothIsoBmffDemuxFactory,
    cancellation: source_core::CancellationToken,
    sniff_budget: demux_api::DemuxSniffBudget,
) -> anyhow::Result<SmoothOpenedComponents> {
    let video_cancellation = cancellation.clone();
    let (video_outcome, audio_outcome) = thread::scope(|scope| {
        let video_worker = scope.spawn(|| {
            factory.open_video(SmoothVideoDemuxOpenRequest::new(
                Box::new(sources.video),
                video_cancellation,
                sniff_budget,
            ))
        });
        let audio_worker = scope.spawn(|| {
            factory.open_audio(SmoothAudioDemuxOpenRequest::new(
                Box::new(sources.audio),
                cancellation,
                sniff_budget,
            ))
        });

        // Явно join-им обе ветки, чтобы panic превратился в обычную preparation-ошибку,
        // а scope не размотал весь media-open worker.
        (video_worker.join(), audio_worker.join())
    });
    let video = video_outcome
        .map_err(|_| anyhow!("Smooth video ISO-BMFF readiness worker panicked"))?
        .context("Smooth video ISO-BMFF readiness failed")?;
    let video_track_id = exactly_one_track(video.tracks(), TrackKind::Video, "video")?;
    let audio = audio_outcome
        .map_err(|_| anyhow!("Smooth audio ISO-BMFF readiness worker panicked"))?
        .context("Smooth audio ISO-BMFF readiness failed")?;
    let audio_track_id = exactly_one_track(audio.tracks(), TrackKind::Audio, "audio")?;
    Ok(SmoothOpenedComponents {
        video,
        audio,
        video_track_id,
        audio_track_id,
    })
}

/// Требует ровно один track правильной axis до publication.
fn exactly_one_track(
    tracks: &[TrackInfo],
    expected_kind: TrackKind,
    component_name: &'static str,
) -> anyhow::Result<TrackId> {
    let [track] = tracks else {
        bail!("Smooth {component_name} demuxer must expose exactly one track");
    };
    if track.kind != expected_kind {
        bail!("Smooth {component_name} demuxer exposed wrong track kind");
    }
    Ok(track.id)
}

/// Переводит exact rational manifest time в floor nanosecond runtime duration.
fn smooth_time_to_duration(time: SmoothTime) -> Duration {
    smooth_ticks_to_duration(time.ticks(), time.timescale().get())
}
