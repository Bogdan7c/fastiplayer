//! Shared exact lifecycle plan → fetch → reconstruct для одной selected axis.

use std::sync::Arc;

use bytes::Bytes;
use media_core::{
    ExactPresentationWindow, PacketPresentationWindow, TimeBase, TrackId, TrackTimestamp,
};
use smooth_streaming_fmp4::{
    SmoothFragmentIndex, SmoothFragmentPlanRequest, SmoothFragmentReconstructionRequest,
    SmoothReconstructedFragment, SmoothTrackIdentity, SmoothTrackMappingRequest,
    SmoothTrackMediaKind, SmoothTrackSelection, map_smooth_track, plan_smooth_fragment,
    reconstruct_smooth_fragment,
};
use smooth_streaming_manifest_core::SmoothManifest;
use source_core::{CancellationToken, HttpRequestTarget};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveResourceSecretForwarding, AdaptiveTransportError,
};

use super::SmoothFragmentSourcePolicy;

/// Успешный следующий lifecycle item shared cursor-а.
pub(super) enum SmoothCursorItem {
    Initialization {
        sequence: u64,
        bytes: Bytes,
    },
    Media {
        sequence: u64,
        media: SmoothCursorMedia,
    },
    EndOfStream,
}

/// Axis-shaped publication-ready media item.
pub(super) enum SmoothCursorMedia {
    Video {
        bytes: Bytes,
    },
    Audio {
        bytes: Bytes,
        presentation_window: PacketPresentationWindow,
    },
}

/// Read failure различает non-latching cancellation и latched failure.
pub(super) enum SmoothCursorReadError {
    Cancelled,
    Failed(SmoothCursorFailureKind),
}

/// Fixed secret-safe failure taxonomy trait-visible source-а.
#[derive(Clone, Copy)]
pub(super) enum SmoothCursorFailureKind {
    Mapping,
    MappingInvariant,
    Planning,
    TargetResolution,
    Fetch,
    Reconstruction,
    SequenceOverflow,
    PresentationWindow,
}

impl SmoothCursorFailureKind {
    /// Возвращает bounded static reason без target/path/input деталей.
    pub(super) const fn reason(self) -> &'static str {
        match self {
            Self::Mapping => "smooth track mapping failed",
            Self::MappingInvariant => "smooth track mapping invariant failed",
            Self::Planning => "smooth fragment planning failed",
            Self::TargetResolution => "smooth fragment target resolution failed",
            Self::Fetch => "smooth fragment fetch failed",
            Self::Reconstruction => "smooth fragment reconstruction failed",
            Self::SequenceOverflow => "smooth fragment sequence overflow",
            Self::PresentationWindow => "smooth audio presentation window invalid",
        }
    }
}

/// Независимый cursor одной selected component axis.
pub(super) struct SmoothFragmentCursor {
    http: AdaptiveHttpContext,
    effective_manifest_target: HttpRequestTarget,
    fragment_secret_forwarding: AdaptiveResourceSecretForwarding,
    manifest: Arc<SmoothManifest>,
    selection: SmoothTrackSelection,
    expected_identity: SmoothTrackIdentity,
    media_kind: SmoothTrackMediaKind,
    timescale_ticks_per_second: u32,
    reconstructed_track_id: u32,
    initialization_bytes: Option<Bytes>,
    fragment_count: usize,
    next_fragment_index: usize,
    policy: SmoothFragmentSourcePolicy,
    failure: Option<SmoothCursorFailureKind>,
}

/// Уже remapped immutable proof construction stage-а.
#[derive(Clone, Copy)]
pub(super) struct SmoothCursorTrackProof {
    pub(super) identity: SmoothTrackIdentity,
    pub(super) media_kind: SmoothTrackMediaKind,
    pub(super) fragment_count: usize,
    pub(super) timescale_ticks_per_second: u32,
    pub(super) reconstructed_track_id: u32,
}

/// Owned значения, необходимые cursor-у после split seed-а.
pub(super) struct SmoothFragmentCursorRequest {
    pub(super) http: AdaptiveHttpContext,
    pub(super) effective_manifest_target: HttpRequestTarget,
    pub(super) fragment_secret_forwarding: AdaptiveResourceSecretForwarding,
    pub(super) manifest: Arc<SmoothManifest>,
    pub(super) selection: SmoothTrackSelection,
    pub(super) expected_identity: SmoothTrackIdentity,
    pub(super) media_kind: SmoothTrackMediaKind,
    pub(super) timescale_ticks_per_second: u32,
    pub(super) reconstructed_track_id: u32,
    pub(super) initialization_bytes: Bytes,
    pub(super) fragment_count: usize,
    pub(super) first_fragment_index: usize,
    pub(super) policy: SmoothFragmentSourcePolicy,
}

impl SmoothFragmentCursor {
    /// Создаёт cursor без I/O; первый pull остаётся initialization segment-ом.
    pub(super) fn new(request: SmoothFragmentCursorRequest) -> Self {
        Self {
            http: request.http,
            effective_manifest_target: request.effective_manifest_target,
            fragment_secret_forwarding: request.fragment_secret_forwarding,
            manifest: request.manifest,
            selection: request.selection,
            expected_identity: request.expected_identity,
            media_kind: request.media_kind,
            timescale_ticks_per_second: request.timescale_ticks_per_second,
            reconstructed_track_id: request.reconstructed_track_id,
            initialization_bytes: Some(request.initialization_bytes),
            fragment_count: request.fragment_count,
            next_fragment_index: request.first_fragment_index,
            policy: request.policy,
            failure: None,
        }
    }

    /// Возвращает следующий lifecycle item, не продвигая состояние при cancellation.
    pub(super) fn next(
        &mut self,
        caller_cancellation: &CancellationToken,
    ) -> Result<SmoothCursorItem, SmoothCursorReadError> {
        if let Some(failure) = self.failure {
            return Err(SmoothCursorReadError::Failed(failure));
        }
        if self.is_cancelled(caller_cancellation) {
            return Err(SmoothCursorReadError::Cancelled);
        }
        if let Some(initialization_bytes) = self.initialization_bytes.take() {
            if self.is_cancelled(caller_cancellation) {
                self.initialization_bytes = Some(initialization_bytes);
                return Err(SmoothCursorReadError::Cancelled);
            }
            return Ok(SmoothCursorItem::Initialization {
                sequence: 0,
                bytes: initialization_bytes,
            });
        }
        if self.next_fragment_index >= self.fragment_count {
            return Ok(SmoothCursorItem::EndOfStream);
        }
        self.next_media(caller_cancellation)
    }

    /// Возвращает общий retained cancellation без раскрытия HTTP context.
    pub(super) fn cancellation(&self) -> &CancellationToken {
        self.http.cancellation()
    }

    /// Выполняет ровно один selected fragment transaction.
    fn next_media(
        &mut self,
        caller_cancellation: &CancellationToken,
    ) -> Result<SmoothCursorItem, SmoothCursorReadError> {
        let retained_cancellation = self.http.cancellation().clone();
        let combined_cancellation =
            || retained_cancellation.is_cancelled() || caller_cancellation.is_cancelled();
        if combined_cancellation() {
            return Err(SmoothCursorReadError::Cancelled);
        }

        let manifest = Arc::clone(&self.manifest);
        let mapped = map_smooth_track(SmoothTrackMappingRequest::new(
            &manifest,
            self.selection,
            &combined_cancellation,
        ))
        .map_err(|error| {
            if combined_cancellation()
                || matches!(
                    error,
                    smooth_streaming_fmp4::SmoothTrackMappingError::Cancelled
                )
            {
                SmoothCursorReadError::Cancelled
            } else {
                self.latch(SmoothCursorFailureKind::Mapping)
            }
        })?;
        if mapped.identity() != self.expected_identity
            || mapped.identity().media_kind() != self.media_kind
            || mapped.timescale_ticks_per_second() != self.timescale_ticks_per_second
            || mapped.reconstructed_track_id().get() != self.reconstructed_track_id
        {
            return Err(self.latch(SmoothCursorFailureKind::MappingInvariant));
        }
        let plan = plan_smooth_fragment(SmoothFragmentPlanRequest::new(
            &mapped,
            SmoothFragmentIndex::new(self.next_fragment_index),
            &combined_cancellation,
        ))
        .map_err(|error| {
            if combined_cancellation()
                || matches!(
                    error,
                    smooth_streaming_fmp4::SmoothFragmentPlanError::Cancelled
                )
            {
                SmoothCursorReadError::Cancelled
            } else {
                self.latch(SmoothCursorFailureKind::Planning)
            }
        })?;
        if combined_cancellation() {
            return Err(SmoothCursorReadError::Cancelled);
        }
        let fragment_target = self
            .effective_manifest_target
            .resolve_reference(plan.relative_path().transport_relative_path())
            .map_err(|_| self.latch(SmoothCursorFailureKind::TargetResolution))?;
        if combined_cancellation() {
            return Err(SmoothCursorReadError::Cancelled);
        }

        let fetch = AdaptiveResourceFetchRequest::full(
            self.http.source_generation(),
            fragment_target,
            self.http
                .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
            AdaptiveResourcePurpose::MediaSegment,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        )
        .with_secret_forwarding(self.fragment_secret_forwarding);
        let fetched = self.http.fetch_resource_blocking(fetch).map_err(|error| {
            if matches!(error, AdaptiveTransportError::Cancelled)
                || retained_cancellation.is_cancelled()
            {
                SmoothCursorReadError::Cancelled
            } else {
                self.latch(SmoothCursorFailureKind::Fetch)
            }
        })?;
        if combined_cancellation() {
            return Err(SmoothCursorReadError::Cancelled);
        }

        let fragment = reconstruct_smooth_fragment(SmoothFragmentReconstructionRequest::new(
            fetched.bytes(),
            &plan,
            &self.policy.inspection_limits,
            self.policy.write_limits,
            &combined_cancellation,
        ))
        .map_err(|error| {
            if combined_cancellation()
                || matches!(
                    error,
                    smooth_streaming_fmp4::SmoothFragmentReconstructionError::Cancelled
                )
            {
                SmoothCursorReadError::Cancelled
            } else {
                self.latch(SmoothCursorFailureKind::Reconstruction)
            }
        })?;
        if combined_cancellation() {
            return Err(SmoothCursorReadError::Cancelled);
        }

        let media = self.materialize_media(fragment)?;
        if combined_cancellation() {
            return Err(SmoothCursorReadError::Cancelled);
        }
        let current_fragment_index = self.next_fragment_index;
        let sequence = u64::try_from(current_fragment_index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| self.latch(SmoothCursorFailureKind::SequenceOverflow))?;
        let next_fragment_index = current_fragment_index
            .checked_add(1)
            .ok_or_else(|| self.latch(SmoothCursorFailureKind::SequenceOverflow))?;
        self.next_fragment_index = next_fragment_index;
        Ok(SmoothCursorItem::Media { sequence, media })
    }

    /// Переводит исчерпывающий F2 outcome в axis-shaped neutral bytes/window.
    fn materialize_media(
        &mut self,
        fragment: SmoothReconstructedFragment,
    ) -> Result<SmoothCursorMedia, SmoothCursorReadError> {
        match (self.media_kind, fragment) {
            (SmoothTrackMediaKind::Video, SmoothReconstructedFragment::Admitted(fragment)) => {
                Ok(SmoothCursorMedia::Video {
                    bytes: fragment.into_media_segment_bytes().into(),
                })
            }
            (
                SmoothTrackMediaKind::Video,
                SmoothReconstructedFragment::PendingAudioPresentationWindow(_),
            ) => Err(self.latch(SmoothCursorFailureKind::MappingInvariant)),
            (SmoothTrackMediaKind::Audio, SmoothReconstructedFragment::Admitted(fragment)) => {
                Ok(SmoothCursorMedia::Audio {
                    bytes: fragment.into_media_segment_bytes().into(),
                    presentation_window: PacketPresentationWindow::Unbounded,
                })
            }
            (
                SmoothTrackMediaKind::Audio,
                SmoothReconstructedFragment::PendingAudioPresentationWindow(pending),
            ) => {
                let presentation_window = build_presentation_window(
                    pending.manifest_window(),
                    self.reconstructed_track_id,
                    self.timescale_ticks_per_second,
                )
                .map_err(|failure| self.latch(failure))?;
                Ok(SmoothCursorMedia::Audio {
                    bytes: pending.into_unchanged_media_segment_bytes().into(),
                    presentation_window: PacketPresentationWindow::Bounded(presentation_window),
                })
            }
        }
    }

    /// Латчит fixed failure до всех последующих pulls.
    fn latch(&mut self, failure: SmoothCursorFailureKind) -> SmoothCursorReadError {
        self.failure = Some(failure);
        SmoothCursorReadError::Failed(failure)
    }

    /// Объединяет retained и per-call cancellation.
    fn is_cancelled(&self, caller_cancellation: &CancellationToken) -> bool {
        self.http.cancellation().is_cancelled() || caller_cancellation.is_cancelled()
    }

    /// Test-only переводит finite cursor к EOS после retained initialization.
    #[cfg(test)]
    pub(super) fn end_after_initialization_for_test(&mut self) {
        self.fragment_count = self.next_fragment_index;
    }
}

/// Строит exact F3A window без saturation или clock rescale.
fn build_presentation_window(
    window: smooth_streaming_fmp4::SmoothManifestWindow,
    reconstructed_track_id: u32,
    timescale_ticks_per_second: u32,
) -> Result<ExactPresentationWindow, SmoothCursorFailureKind> {
    let start =
        i64::try_from(window.start()).map_err(|_| SmoothCursorFailureKind::PresentationWindow)?;
    let end_exclusive = i64::try_from(window.end_exclusive())
        .map_err(|_| SmoothCursorFailureKind::PresentationWindow)?;
    let time_base = TimeBase::new(1, timescale_ticks_per_second)
        .ok_or(SmoothCursorFailureKind::PresentationWindow)?;
    let track_id = TrackId::new(reconstructed_track_id);
    ExactPresentationWindow::new(
        TrackTimestamp::new(track_id, start, time_base),
        TrackTimestamp::new(track_id, end_exclusive, time_base),
    )
    .map_err(|_| SmoothCursorFailureKind::PresentationWindow)
}

/// Повторно remap-ит selected row и извлекает immutable cursor proof без HTTP.
pub(super) fn remap_track_proof(
    manifest: &SmoothManifest,
    selection: SmoothTrackSelection,
    cancellation: &dyn Fn() -> bool,
) -> Result<SmoothCursorTrackProof, smooth_streaming_fmp4::SmoothTrackMappingError> {
    let mapped = map_smooth_track(SmoothTrackMappingRequest::new(
        manifest,
        selection,
        cancellation,
    ))?;
    let identity = mapped.identity();
    let fragment_count = manifest
        .streams()
        .get(selection.stream_ordinal.get())
        .map(|stream| stream.timeline().fragment_count())
        .ok_or(smooth_streaming_fmp4::SmoothTrackMappingError::StreamNotFound)?;
    Ok(SmoothCursorTrackProof {
        identity,
        media_kind: identity.media_kind(),
        fragment_count,
        timescale_ticks_per_second: mapped.timescale_ticks_per_second(),
        reconstructed_track_id: mapped.reconstructed_track_id().get(),
    })
}
