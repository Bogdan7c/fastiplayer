//! Per-fragment ISO-BMFF adapter с authoritative packet presentation windows.
//!
//! Каждый media fragment открывается как отдельный `init + media` inner demux.
//! Adapter не склеивает историю fragment-ов и не выводит provenance из PTS.

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{
    DemuxContainerId, DemuxHints, DemuxInput, DemuxOpenError, DemuxRegistry, DemuxRegistryError,
    DemuxSniffBudget, OrderedSegmentDiscontinuity, OrderedSegmentReadError, OrderedSegmentSequence,
    PresentationWindowOrderedSegment, PresentationWindowOrderedSegmentReadOutcome,
    PresentationWindowOrderedSegmentSource,
};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, Demuxer, MediaDemuxError,
    MediaMetadata, Packet, PacketPresentationWindow, PacketPresentationWindowAssignmentError,
    TimelineNotSeekableReason, TrackInfo,
};
use source_core::CancellationToken;
use thiserror::Error;

use crate::{DemuxerOptions, SymphoniaDemuxFactory};

#[cfg(test)]
mod tests;

/// Поле stable single-track snapshot-а, которое изменилось между fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationWindowOrderedTrackField {
    /// Число track-ов перестало быть ровно одним.
    TrackCount,
    /// Container track ID изменился.
    TrackId,
    /// Audio/video kind изменился.
    Kind,
    /// Codec identity изменилась.
    CodecId,
    /// Decoder initialization bytes изменились.
    CodecPrivate,
    /// Track time base изменилась.
    TimeBase,
    /// Audio sample rate изменилась.
    SampleRate,
    /// Audio channel layout/count изменился.
    Channels,
    /// Video layout/metadata изменились.
    VideoMetadata,
}

/// Typed failure нового per-fragment adapter-а.
#[derive(Debug, Error)]
pub enum PresentationWindowOrderedIsoMp4Error {
    /// Shared cancellation остановила lifecycle.
    #[error("window-aware ordered ISO-BMFF demux отменён")]
    Cancelled,
    /// Source завершился operational error-ом.
    #[error("window-aware ordered source завершился ошибкой")]
    Source(#[source] OrderedSegmentReadError),
    /// Начальные init/media ещё не готовы для synchronous open.
    #[error("начальные window-aware ordered segments временно недоступны")]
    InitialSegmentsTemporarilyUnavailable {
        /// Source-owned retry policy не теряется на границе synchronous open.
        retry_hint: DemuxRetryHint,
    },
    /// Source завершился до initialization segment-а.
    #[error("window-aware ordered source завершился без initialization segment")]
    MissingInitialization,
    /// Source завершился после init, но до первого media fragment-а.
    #[error("window-aware ordered source завершился без media fragment")]
    MissingMedia,
    /// Media пришёл раньше init.
    #[error("media fragment пришёл до initialization segment")]
    MediaBeforeInitialization,
    /// Повторный init запрещён.
    #[error("повторный initialization segment запрещён")]
    DuplicateInitialization,
    /// Segment bytes пусты.
    #[error("window-aware ordered segment пуст")]
    EmptySegment,
    /// Sequence не строго возрастает.
    #[error("window-aware ordered sequence не возрастает строго")]
    NonMonotonicSequence {
        /// Последний принятый sequence.
        previous: OrderedSegmentSequence,
        /// Новый sequence.
        next: OrderedSegmentSequence,
    },
    /// Новый timeline требует decoder/session reset, которым adapter не владеет.
    #[error("window-aware ordered discontinuity требует session reset")]
    DiscontinuityRequiresSessionReset,
    /// Static ISO container identity нельзя построить.
    #[error("не удалось построить ISO-BMFF registry identity")]
    RegistryIdentity,
    /// Symphonia factory нельзя зарегистрировать.
    #[error("не удалось зарегистрировать ISO-BMFF factory")]
    Registry(#[source] DemuxRegistryError),
    /// Isolated `init + media` не открылся как exact ISO-BMFF.
    #[error("не удалось открыть isolated ISO-BMFF fragment")]
    InnerOpen(#[source] DemuxOpenError),
    /// Inner demux read завершился backend error-ом.
    #[error("ошибка чтения isolated ISO-BMFF fragment")]
    InnerRead(#[source] anyhow::Error),
    /// Fragment изменил decoder-facing track snapshot.
    #[error("isolated ISO-BMFF fragment изменил stable track snapshot: {field:?}")]
    IncompatibleTrack {
        /// Первое несовместимое поле.
        field: PresentationWindowOrderedTrackField,
    },
    /// Inner demux потребовал decoder reset.
    #[error("isolated ISO-BMFF fragment опубликовал TracksChanged")]
    TracksChanged,
    /// Bounded window не согласуется с packet track clock.
    #[error("presentation window не согласуется с packet track clock")]
    PresentationWindowAssignment(#[source] PacketPresentationWindowAssignmentError),
}

/// Reader над двумя immutable `Bytes` без полной конкатенации.
struct InitializationMediaReader {
    initialization: Bytes,
    media: Bytes,
    position: usize,
}

impl InitializationMediaReader {
    /// Связывает ровно один init с ровно одним media fragment.
    fn new(initialization: Bytes, media: Bytes) -> Self {
        Self {
            initialization,
            media,
            position: 0,
        }
    }

    /// Копирует из borrowed immutable slice-а без clone/refcount операции.
    fn copy_from_slice(output: &mut [u8], bytes: &[u8], local_offset: usize) -> usize {
        let available = &bytes[local_offset.min(bytes.len())..];
        let copied = available.len().min(output.len());
        output[..copied].copy_from_slice(&available[..copied]);
        copied
    }

    /// Продвигает общий cursor с явной защитой от арифметического переполнения.
    fn advance_position(&mut self, copied: usize) -> io::Result<()> {
        self.position = self
            .position
            .checked_add(copied)
            .ok_or_else(|| io::Error::other("initialization/media reader position overflow"))?;
        Ok(())
    }
}

impl Read for InitializationMediaReader {
    /// Читает init, затем ровно один media fragment.
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.position < self.initialization.len() {
            let copied = Self::copy_from_slice(output, self.initialization.as_ref(), self.position);
            self.advance_position(copied)?;
            if copied == output.len() {
                return Ok(copied);
            }
            let media_copied = Self::copy_from_slice(&mut output[copied..], self.media.as_ref(), 0);
            self.advance_position(media_copied)?;
            return Ok(copied + media_copied);
        }
        let media_offset = self.position - self.initialization.len();
        let copied = Self::copy_from_slice(output, self.media.as_ref(), media_offset);
        self.advance_position(copied)?;
        Ok(copied)
    }
}

/// Inner demux и provenance его единственного media fragment-а.
struct ActiveFragment {
    demuxer: Box<dyn Demuxer + Send>,
    presentation_window: PacketPresentationWindow,
}

/// Window-aware ordered ISO-BMFF demuxer.
pub struct PresentationWindowOrderedIsoMp4Demuxer {
    source: Box<dyn PresentationWindowOrderedSegmentSource>,
    cancellation: CancellationToken,
    registry: Arc<DemuxRegistry>,
    sniff_budget: DemuxSniffBudget,
    required_container: DemuxContainerId,
    initialization: Bytes,
    last_sequence: OrderedSegmentSequence,
    tracks: Vec<TrackInfo>,
    active_fragment: Option<ActiveFragment>,
    source_terminal: bool,
    /// `TracksChanged` навсегда закрывает adapter с устаревшим decoder contract.
    tracks_changed_fence: bool,
}

impl PresentationWindowOrderedIsoMp4Demuxer {
    /// Synchronously принимает init и первый media fragment, чтобы `tracks()` был готов сразу.
    pub fn new(
        source: Box<dyn PresentationWindowOrderedSegmentSource>,
        cancellation: CancellationToken,
        sniff_budget: DemuxSniffBudget,
        demuxer_options: DemuxerOptions,
    ) -> Result<Self, PresentationWindowOrderedIsoMp4Error> {
        let registry = build_registry(demuxer_options)?;
        Self::new_with_registry(source, cancellation, sniff_budget, Arc::new(registry))
    }

    /// Открывает adapter через уже собранный composition-owned registry.
    ///
    /// Adapter сам фиксирует required ISO-BMFF identity, поэтому caller не
    /// может подменить backend по extension или произвольному container ID.
    pub fn new_with_registry(
        mut source: Box<dyn PresentationWindowOrderedSegmentSource>,
        cancellation: CancellationToken,
        sniff_budget: DemuxSniffBudget,
        registry: Arc<DemuxRegistry>,
    ) -> Result<Self, PresentationWindowOrderedIsoMp4Error> {
        ensure_active(&cancellation)?;
        let initialization_segment = pull_initial_segment(&mut *source, &cancellation)?;
        let (initialization_sequence, initialization) =
            accept_initialization(initialization_segment)?;
        ensure_active(&cancellation)?;
        let first_media_segment = pull_first_media(&mut *source, &cancellation)?;
        ensure_active(&cancellation)?;
        let (media_sequence, media, presentation_window) =
            accept_media(first_media_segment, initialization_sequence)?;

        let mut adapter = Self {
            source,
            cancellation,
            registry,
            sniff_budget,
            required_container: required_iso_bmff_container()?,
            initialization,
            last_sequence: media_sequence,
            tracks: Vec::new(),
            active_fragment: None,
            source_terminal: false,
            tracks_changed_fence: false,
        };
        let inner = adapter.open_inner(media)?;
        adapter.tracks = normalized_single_track(inner.tracks())?;
        ensure_active(&adapter.cancellation)?;
        adapter.active_fragment = Some(ActiveFragment {
            demuxer: inner,
            presentation_window,
        });
        Ok(adapter)
    }

    /// Открывает isolated `init + exactly one media` через production registry/factory path.
    fn open_inner(
        &self,
        media: Bytes,
    ) -> Result<Box<dyn Demuxer + Send>, PresentationWindowOrderedIsoMp4Error> {
        ensure_active(&self.cancellation)?;
        let reader = InitializationMediaReader::new(self.initialization.clone(), media);
        let demuxer = self
            .registry
            .open_required_container(
                DemuxInput::byte_stream(Box::new(reader)),
                DemuxHints::none(),
                self.sniff_budget,
                self.cancellation.clone(),
                self.required_container.clone(),
            )
            .map_err(PresentationWindowOrderedIsoMp4Error::InnerOpen)?;
        ensure_active(&self.cancellation)?;
        Ok(demuxer)
    }

    /// Pull-ит и открывает следующий media fragment либо публикует readiness/terminal.
    fn advance_fragment(
        &mut self,
    ) -> Result<Option<DemuxReadEvent>, PresentationWindowOrderedIsoMp4Error> {
        ensure_active(&self.cancellation)?;
        let outcome = self
            .source
            .next_segment(&self.cancellation)
            .map_err(map_source_error)?;
        ensure_active(&self.cancellation)?;
        match outcome {
            PresentationWindowOrderedSegmentReadOutcome::TemporarilyUnavailable(hint) => {
                Ok(Some(DemuxReadEvent::TemporarilyUnavailable(hint)))
            }
            PresentationWindowOrderedSegmentReadOutcome::EndOfStream => {
                self.source_terminal = true;
                Ok(Some(DemuxReadEvent::EndOfStream))
            }
            PresentationWindowOrderedSegmentReadOutcome::Segment(segment) => {
                let (sequence, media, presentation_window) =
                    accept_media(segment, self.last_sequence)?;
                let inner = self.open_inner(media)?;
                validate_stable_track(&self.tracks, inner.tracks())?;
                ensure_active(&self.cancellation)?;
                self.last_sequence = sequence;
                self.active_fragment = Some(ActiveFragment {
                    demuxer: inner,
                    presentation_window,
                });
                Ok(None)
            }
        }
    }

    /// Назначает provenance window перед публикацией packet-а.
    fn attach_window(
        &self,
        packet: Packet,
        presentation_window: PacketPresentationWindow,
    ) -> Result<Packet, PresentationWindowOrderedIsoMp4Error> {
        match presentation_window {
            PacketPresentationWindow::Unbounded => Ok(packet),
            PacketPresentationWindow::Bounded(window) => packet
                .try_with_bounded_presentation_window(window)
                .map_err(PresentationWindowOrderedIsoMp4Error::PresentationWindowAssignment),
        }
    }
}

impl Demuxer for PresentationWindowOrderedIsoMp4Demuxer {
    /// Возвращает первый validated stable single-track snapshot.
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Aggregate duration не выводится из per-fragment windows.
    fn duration(&self) -> Option<Duration> {
        None
    }

    /// Metadata текущего isolated inner-а не становится aggregate authority.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.active_fragment
            .as_ref()
            .and_then(|fragment| fragment.demuxer.media_metadata())
    }

    /// Ordered fragment source всегда non-seekable.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        }
    }

    /// Читает события, сохраняя provenance текущего isolated fragment-а.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        loop {
            ensure_active(&self.cancellation)?;
            if self.tracks_changed_fence {
                return Err(PresentationWindowOrderedIsoMp4Error::TracksChanged.into());
            }
            if self.source_terminal {
                return Ok(DemuxReadEvent::EndOfStream);
            }
            if self.active_fragment.is_none()
                && let Some(event) = self.advance_fragment()?
            {
                ensure_active(&self.cancellation)?;
                return Ok(event);
            }

            let Some(active) = self.active_fragment.as_mut() else {
                continue;
            };
            let (inner_event, presentation_window) = {
                let event = active
                    .demuxer
                    .next_event()
                    .map_err(PresentationWindowOrderedIsoMp4Error::InnerRead)?;
                (event, active.presentation_window)
            };
            match inner_event {
                DemuxReadEvent::Packet(packet) => {
                    let packet = self.attach_window(packet, presentation_window)?;
                    ensure_active(&self.cancellation)?;
                    return Ok(DemuxReadEvent::Packet(packet));
                }
                DemuxReadEvent::EndOfStream => {
                    self.active_fragment = None;
                }
                DemuxReadEvent::TracksChanged(_) => {
                    self.tracks_changed_fence = true;
                    return Err(PresentationWindowOrderedIsoMp4Error::TracksChanged.into());
                }
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    ensure_active(&self.cancellation)?;
                    return Ok(DemuxReadEvent::TemporarilyUnavailable(hint));
                }
                DemuxReadEvent::MediaMetadataChanged(metadata) => {
                    ensure_active(&self.cancellation)?;
                    return Ok(DemuxReadEvent::MediaMetadataChanged(metadata));
                }
            }
        }
    }

    /// Seek всегда typed rejected.
    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(MediaDemuxError::SeekUnavailable {
            reason: "window-aware ordered ISO-BMFF source не поддерживает seek".to_owned(),
        }
        .into())
    }
}

/// Строит production registry с единственным Symphonia factory.
fn build_registry(
    demuxer_options: DemuxerOptions,
) -> Result<DemuxRegistry, PresentationWindowOrderedIsoMp4Error> {
    let factory = SymphoniaDemuxFactory::new(demuxer_options)
        .map_err(|_| PresentationWindowOrderedIsoMp4Error::RegistryIdentity)?;
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(factory))
        .map_err(PresentationWindowOrderedIsoMp4Error::Registry)?;
    Ok(registry)
}

/// Возвращает единственную разрешённую container identity adapter-а.
fn required_iso_bmff_container() -> Result<DemuxContainerId, PresentationWindowOrderedIsoMp4Error> {
    DemuxContainerId::new("iso-bmff")
        .map_err(|_| PresentationWindowOrderedIsoMp4Error::RegistryIdentity)
}

/// Извлекает первый init outcome без скрытого ожидания.
fn pull_initial_segment(
    source: &mut dyn PresentationWindowOrderedSegmentSource,
    cancellation: &CancellationToken,
) -> Result<PresentationWindowOrderedSegment, PresentationWindowOrderedIsoMp4Error> {
    match source
        .next_segment(cancellation)
        .map_err(map_source_error)?
    {
        PresentationWindowOrderedSegmentReadOutcome::Segment(segment) => Ok(segment),
        PresentationWindowOrderedSegmentReadOutcome::TemporarilyUnavailable(retry_hint) => Err(
            PresentationWindowOrderedIsoMp4Error::InitialSegmentsTemporarilyUnavailable {
                retry_hint,
            },
        ),
        PresentationWindowOrderedSegmentReadOutcome::EndOfStream => {
            Err(PresentationWindowOrderedIsoMp4Error::MissingInitialization)
        }
    }
}

/// Извлекает первый media outcome для немедленного stable track snapshot-а.
fn pull_first_media(
    source: &mut dyn PresentationWindowOrderedSegmentSource,
    cancellation: &CancellationToken,
) -> Result<PresentationWindowOrderedSegment, PresentationWindowOrderedIsoMp4Error> {
    match source
        .next_segment(cancellation)
        .map_err(map_source_error)?
    {
        PresentationWindowOrderedSegmentReadOutcome::Segment(segment) => Ok(segment),
        PresentationWindowOrderedSegmentReadOutcome::TemporarilyUnavailable(retry_hint) => Err(
            PresentationWindowOrderedIsoMp4Error::InitialSegmentsTemporarilyUnavailable {
                retry_hint,
            },
        ),
        PresentationWindowOrderedSegmentReadOutcome::EndOfStream => {
            Err(PresentationWindowOrderedIsoMp4Error::MissingMedia)
        }
    }
}

/// Валидирует единственный initialization segment.
fn accept_initialization(
    segment: PresentationWindowOrderedSegment,
) -> Result<(OrderedSegmentSequence, Bytes), PresentationWindowOrderedIsoMp4Error> {
    reject_discontinuity(segment.discontinuity())?;
    match segment {
        PresentationWindowOrderedSegment::Initialization {
            sequence, bytes, ..
        } => {
            reject_empty(&bytes)?;
            Ok((sequence, bytes))
        }
        PresentationWindowOrderedSegment::Media { .. } => {
            Err(PresentationWindowOrderedIsoMp4Error::MediaBeforeInitialization)
        }
    }
}

/// Валидирует media lifecycle и извлекает immutable provenance.
fn accept_media(
    segment: PresentationWindowOrderedSegment,
    previous: OrderedSegmentSequence,
) -> Result<
    (OrderedSegmentSequence, Bytes, PacketPresentationWindow),
    PresentationWindowOrderedIsoMp4Error,
> {
    reject_discontinuity(segment.discontinuity())?;
    let sequence = segment.sequence();
    if sequence <= previous {
        return Err(PresentationWindowOrderedIsoMp4Error::NonMonotonicSequence {
            previous,
            next: sequence,
        });
    }
    match segment {
        PresentationWindowOrderedSegment::Initialization { .. } => {
            Err(PresentationWindowOrderedIsoMp4Error::DuplicateInitialization)
        }
        PresentationWindowOrderedSegment::Media {
            bytes,
            presentation_window,
            ..
        } => {
            reject_empty(&bytes)?;
            Ok((sequence, bytes, presentation_window))
        }
    }
}

/// Reject-ит timeline reset, которым adapter не владеет.
fn reject_discontinuity(
    discontinuity: OrderedSegmentDiscontinuity,
) -> Result<(), PresentationWindowOrderedIsoMp4Error> {
    match discontinuity {
        OrderedSegmentDiscontinuity::Continuous => Ok(()),
        OrderedSegmentDiscontinuity::StartsNewTimeline => {
            Err(PresentationWindowOrderedIsoMp4Error::DiscontinuityRequiresSessionReset)
        }
    }
}

/// Reject-ит пустые container bytes до probe/open.
fn reject_empty(bytes: &Bytes) -> Result<(), PresentationWindowOrderedIsoMp4Error> {
    if bytes.is_empty() {
        Err(PresentationWindowOrderedIsoMp4Error::EmptySegment)
    } else {
        Ok(())
    }
}

/// Преобразует source cancellation в единый adapter cancellation.
fn map_source_error(error: OrderedSegmentReadError) -> PresentationWindowOrderedIsoMp4Error {
    match error {
        OrderedSegmentReadError::Cancelled => PresentationWindowOrderedIsoMp4Error::Cancelled,
        error => PresentationWindowOrderedIsoMp4Error::Source(error),
    }
}

/// Проверяет shared cancellation fence.
fn ensure_active(
    cancellation: &CancellationToken,
) -> Result<(), PresentationWindowOrderedIsoMp4Error> {
    if cancellation.is_cancelled() {
        Err(PresentationWindowOrderedIsoMp4Error::Cancelled)
    } else {
        Ok(())
    }
}

/// Создаёт public snapshot без fragment-local duration authority.
fn normalized_single_track(
    tracks: &[TrackInfo],
) -> Result<Vec<TrackInfo>, PresentationWindowOrderedIsoMp4Error> {
    if tracks.len() != 1 {
        return Err(PresentationWindowOrderedIsoMp4Error::IncompatibleTrack {
            field: PresentationWindowOrderedTrackField::TrackCount,
        });
    }
    let mut track = tracks[0].clone();
    track.duration = None;
    Ok(vec![track])
}

/// Сравнивает только decoder-facing identity/layout; fragment duration не authoritative.
fn validate_stable_track(
    expected: &[TrackInfo],
    actual: &[TrackInfo],
) -> Result<(), PresentationWindowOrderedIsoMp4Error> {
    if actual.len() != 1 {
        return incompatible(PresentationWindowOrderedTrackField::TrackCount);
    }
    let expected = &expected[0];
    let actual = &actual[0];
    if expected.id != actual.id {
        return incompatible(PresentationWindowOrderedTrackField::TrackId);
    }
    if expected.kind != actual.kind {
        return incompatible(PresentationWindowOrderedTrackField::Kind);
    }
    if expected.codec_id != actual.codec_id {
        return incompatible(PresentationWindowOrderedTrackField::CodecId);
    }
    if expected.codec_private != actual.codec_private {
        return incompatible(PresentationWindowOrderedTrackField::CodecPrivate);
    }
    if expected.time_base != actual.time_base {
        return incompatible(PresentationWindowOrderedTrackField::TimeBase);
    }
    if expected.sample_rate != actual.sample_rate {
        return incompatible(PresentationWindowOrderedTrackField::SampleRate);
    }
    if expected.channels != actual.channels {
        return incompatible(PresentationWindowOrderedTrackField::Channels);
    }
    if expected.video != actual.video {
        return incompatible(PresentationWindowOrderedTrackField::VideoMetadata);
    }
    Ok(())
}

/// Создаёт typed drift error без повторения boilerplate.
fn incompatible(
    field: PresentationWindowOrderedTrackField,
) -> Result<(), PresentationWindowOrderedIsoMp4Error> {
    Err(PresentationWindowOrderedIsoMp4Error::IncompatibleTrack { field })
}
