//! Consume provider seed, canonical C3 selection и безопасный split двух sources.

use std::sync::Arc;

use bytes::Bytes;
use smooth_streaming_fmp4::{SmoothTrackIdentity, SmoothTrackMediaKind, SmoothTrackSelection};
use smooth_streaming_manifest_core::SmoothManifest;
use source_core::HttpRequestTarget;
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveResourceSecretForwarding};
use web_media_core::{
    ComponentKind, ComponentVariantCatalog, ComponentVariantExactIdentity,
    ComponentVariantSelection, ComponentVariantSelectionRequest,
};
use web_media_transport_api::SourceGeneration;

use crate::model::{SmoothAlignedSpan, SmoothPreparedCatalog, SmoothRuntimeRow, SmoothRuntimeSeed};

use super::audio::SmoothAudioFragmentSource;
use super::cursor::{
    SmoothCursorTrackProof, SmoothFragmentCursor, SmoothFragmentCursorRequest, remap_track_proof,
};
use super::error::SmoothFragmentSourceBuildError;
use super::policy::SmoothFragmentSourcePolicy;
use super::video::SmoothVideoFragmentSource;

/// Selected catalog metadata и private lazy sources до explicit split.
pub struct SmoothSelectedFragmentSources {
    catalog: ComponentVariantCatalog,
    selection: ComponentVariantSelection,
    source_generation: SourceGeneration,
    aligned_span: SmoothAlignedSpan,
    video_source: SmoothVideoFragmentSource,
    audio_source: SmoothAudioFragmentSource,
    source_factory: SmoothSelectedSourceFactory,
}

/// Cloneable selected seed, из которого P5 атомарно строит replacement pair.
#[derive(Clone)]
pub(crate) struct SmoothSelectedSourceFactory {
    http: AdaptiveHttpContext,
    effective_manifest_target: HttpRequestTarget,
    fragment_secret_forwarding: AdaptiveResourceSecretForwarding,
    manifest: Arc<SmoothManifest>,
    video: SmoothSelectedTrackSeed,
    audio: SmoothSelectedTrackSeed,
    policy: SmoothFragmentSourcePolicy,
}

/// Cloneable component seed без повторного init construction.
#[derive(Clone)]
struct SmoothSelectedTrackSeed {
    selection: SmoothTrackSelection,
    expected_identity: SmoothTrackIdentity,
    media_kind: SmoothTrackMediaKind,
    timescale_ticks_per_second: u32,
    reconstructed_track_id: u32,
    initialization_bytes: Bytes,
    fragment_count: usize,
}

/// Named pair, подготовленная из одного immutable selected seed.
pub(crate) struct SmoothBuiltSourcePair {
    pub(crate) video: SmoothVideoFragmentSource,
    pub(crate) audio: SmoothAudioFragmentSource,
}

impl SmoothSelectedFragmentSources {
    /// Возвращает retained immutable catalog.
    #[must_use]
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        &self.catalog
    }

    /// Возвращает canonical exact C3 selection.
    #[must_use]
    pub const fn selection(&self) -> &ComponentVariantSelection {
        &self.selection
    }

    /// Возвращает retained source generation.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }

    /// Возвращает exact aligned presentation span.
    #[must_use]
    pub const fn aligned_span(&self) -> SmoothAlignedSpan {
        self.aligned_span
    }

    /// Явно отделяет metadata handoff от source ownership.
    #[must_use]
    pub fn into_source_parts(self) -> SmoothFragmentSourceParts {
        SmoothFragmentSourceParts {
            video: self.video_source,
            audio: self.audio_source,
        }
    }

    /// Передаёт P4 retained metadata и sources одним ownership handoff.
    pub(crate) fn into_demux_parts(
        self,
    ) -> (
        ComponentVariantCatalog,
        ComponentVariantSelection,
        SourceGeneration,
        SmoothAlignedSpan,
        SmoothVideoFragmentSource,
        SmoothAudioFragmentSource,
        SmoothSelectedSourceFactory,
    ) {
        (
            self.catalog,
            self.selection,
            self.source_generation,
            self.aligned_span,
            self.video_source,
            self.audio_source,
            self.source_factory,
        )
    }
}

impl std::fmt::Debug for SmoothSelectedFragmentSources {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmoothSelectedFragmentSources")
            .field("source_generation", &self.source_generation)
            .field("aligned_span", &self.aligned_span)
            .finish_non_exhaustive()
    }
}

/// Named owned source pair после explicit metadata discard.
pub struct SmoothFragmentSourceParts {
    /// Ordinary finite video source.
    pub video: SmoothVideoFragmentSource,
    /// F3A window-aware finite audio source.
    pub audio: SmoothAudioFragmentSource,
}

impl std::fmt::Debug for SmoothFragmentSourceParts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmoothFragmentSourceParts")
            .finish_non_exhaustive()
    }
}

impl SmoothPreparedCatalog {
    /// Consume prepared seed и строит sources только для canonical exact selection.
    pub fn into_selected_fragment_sources(
        self,
        selection: ComponentVariantSelection,
        policy: SmoothFragmentSourcePolicy,
    ) -> Result<SmoothSelectedFragmentSources, SmoothFragmentSourceBuildError> {
        let canonical_selection = self
            .catalog
            .select_exact(selection.exact_selection_request())
            .map_err(SmoothFragmentSourceBuildError::Selection)?;
        let ComponentVariantSelectionRequest::VideoAndAudio {
            video: selected_video,
            audio: selected_audio,
        } = canonical_selection.exact_selection_request()
        else {
            return Err(SmoothFragmentSourceBuildError::SelectionLayout);
        };

        let SmoothRuntimeSeed {
            http,
            effective_manifest_target,
            fragment_secret_forwarding,
            manifest,
            video_rows,
            audio_rows,
        } = self.runtime_seed;
        let video_row = take_runtime_row(video_rows, &selected_video, ComponentKind::Video)?;
        let audio_row = take_runtime_row(audio_rows, &selected_audio, ComponentKind::Audio)?;
        let retained_cancellation = http.cancellation().clone();
        let is_cancelled = || retained_cancellation.is_cancelled();
        let video_proof = build_track_proof(
            &manifest,
            &video_row,
            SmoothTrackMediaKind::Video,
            &is_cancelled,
        )?;
        let audio_proof = build_track_proof(
            &manifest,
            &audio_row,
            SmoothTrackMediaKind::Audio,
            &is_cancelled,
        )?;

        let source_factory = SmoothSelectedSourceFactory {
            http,
            effective_manifest_target,
            fragment_secret_forwarding,
            manifest,
            policy,
            video: SmoothSelectedTrackSeed::new(video_row, video_proof),
            audio: SmoothSelectedTrackSeed::new(audio_row, audio_proof),
        };
        let initial_sources = source_factory.build_at(0, 0)?;

        Ok(SmoothSelectedFragmentSources {
            catalog: self.catalog,
            selection: canonical_selection,
            source_generation: self.source_generation,
            aligned_span: self.aligned_span,
            video_source: initial_sources.video,
            audio_source: initial_sources.audio,
            source_factory,
        })
    }
}

/// Требует ровно одну runtime row для selected exact identity.
fn take_runtime_row(
    rows: Box<[SmoothRuntimeRow]>,
    selected: &ComponentVariantExactIdentity,
    component: ComponentKind,
) -> Result<SmoothRuntimeRow, SmoothFragmentSourceBuildError> {
    let mut selected_row = None;
    for row in rows.into_vec() {
        if &row.exact_identity == selected && selected_row.replace(row).is_some() {
            return Err(SmoothFragmentSourceBuildError::RuntimeRowDuplicate { component });
        }
    }
    selected_row.ok_or(SmoothFragmentSourceBuildError::RuntimeRowMissing { component })
}

/// Повторный F2 remap проверяет init identity и immutable track proof.
fn build_track_proof(
    manifest: &smooth_streaming_manifest_core::SmoothManifest,
    row: &SmoothRuntimeRow,
    expected_kind: SmoothTrackMediaKind,
    cancellation: &dyn Fn() -> bool,
) -> Result<SmoothCursorTrackProof, SmoothFragmentSourceBuildError> {
    let proof = remap_track_proof(manifest, row.selection, cancellation).map_err(|error| {
        if cancellation()
            || matches!(
                error,
                smooth_streaming_fmp4::SmoothTrackMappingError::Cancelled
            )
        {
            SmoothFragmentSourceBuildError::Cancelled
        } else {
            SmoothFragmentSourceBuildError::Mapping(error)
        }
    })?;
    if proof.media_kind != expected_kind {
        return Err(SmoothFragmentSourceBuildError::RuntimeTrackKindMismatch);
    }
    if proof.identity != row.initialization_identity {
        return Err(SmoothFragmentSourceBuildError::InitializationIdentityMismatch);
    }
    Ok(proof)
}

impl SmoothSelectedTrackSeed {
    /// Один раз переводит non-clone init owner в cheap shared bytes.
    fn new(row: SmoothRuntimeRow, proof: SmoothCursorTrackProof) -> Self {
        Self {
            selection: row.selection,
            expected_identity: proof.identity,
            media_kind: proof.media_kind,
            timescale_ticks_per_second: proof.timescale_ticks_per_second,
            reconstructed_track_id: proof.reconstructed_track_id,
            initialization_bytes: row.initialization_bytes,
            fragment_count: proof.fragment_count,
        }
    }
}

impl SmoothSelectedSourceFactory {
    /// Строит обе component sources offside с independently selected anchors.
    pub(crate) fn build_at(
        &self,
        video_fragment_index: usize,
        audio_fragment_index: usize,
    ) -> Result<SmoothBuiltSourcePair, SmoothFragmentSourceBuildError> {
        if video_fragment_index >= self.video.fragment_count
            || audio_fragment_index >= self.audio.fragment_count
        {
            return Err(SmoothFragmentSourceBuildError::FragmentIndexOutOfRange);
        }
        Ok(SmoothBuiltSourcePair {
            video: SmoothVideoFragmentSource {
                cursor: build_cursor(self, &self.video, video_fragment_index),
            },
            audio: SmoothAudioFragmentSource {
                cursor: build_cursor(self, &self.audio, audio_fragment_index),
            },
        })
    }

    /// Возвращает immutable manifest для pure P5 anchor lookup.
    pub(crate) fn manifest(&self) -> &SmoothManifest {
        &self.manifest
    }

    /// Возвращает video selection без раскрытия runtime row storage.
    pub(crate) const fn video_selection(&self) -> SmoothTrackSelection {
        self.video.selection
    }

    /// Возвращает audio selection без раскрытия runtime row storage.
    pub(crate) const fn audio_selection(&self) -> SmoothTrackSelection {
        self.audio.selection
    }
}

/// Строит независимый first-fragment video proof source, не consuming retained row.
pub(crate) fn build_video_probe_source(
    seed: &SmoothRuntimeSeed,
    row: &SmoothRuntimeRow,
    policy: SmoothFragmentSourcePolicy,
) -> Result<SmoothVideoFragmentSource, SmoothFragmentSourceBuildError> {
    let cancellation = seed.http.cancellation().clone();
    let proof = build_track_proof(&seed.manifest, row, SmoothTrackMediaKind::Video, &|| {
        cancellation.is_cancelled()
    })?;
    let track = SmoothSelectedTrackSeed::new(row.clone(), proof);
    Ok(SmoothVideoFragmentSource {
        cursor: build_probe_cursor(seed, &track, policy),
    })
}

/// Строит независимый first-fragment audio proof source, не consuming retained row.
pub(crate) fn build_audio_probe_source(
    seed: &SmoothRuntimeSeed,
    row: &SmoothRuntimeRow,
    policy: SmoothFragmentSourcePolicy,
) -> Result<SmoothAudioFragmentSource, SmoothFragmentSourceBuildError> {
    let cancellation = seed.http.cancellation().clone();
    let proof = build_track_proof(&seed.manifest, row, SmoothTrackMediaKind::Audio, &|| {
        cancellation.is_cancelled()
    })?;
    let track = SmoothSelectedTrackSeed::new(row.clone(), proof);
    Ok(SmoothAudioFragmentSource {
        cursor: build_probe_cursor(seed, &track, policy),
    })
}

fn build_probe_cursor(
    seed: &SmoothRuntimeSeed,
    track: &SmoothSelectedTrackSeed,
    policy: SmoothFragmentSourcePolicy,
) -> SmoothFragmentCursor {
    SmoothFragmentCursor::new(SmoothFragmentCursorRequest {
        http: seed.http.clone(),
        effective_manifest_target: seed.effective_manifest_target.clone(),
        fragment_secret_forwarding: seed.fragment_secret_forwarding,
        manifest: Arc::clone(&seed.manifest),
        selection: track.selection,
        expected_identity: track.expected_identity,
        media_kind: track.media_kind,
        timescale_ticks_per_second: track.timescale_ticks_per_second,
        reconstructed_track_id: track.reconstructed_track_id,
        initialization_bytes: track.initialization_bytes.clone(),
        fragment_count: track.fragment_count,
        first_fragment_index: 0,
        policy,
    })
}

/// Строит один cursor из cloneable selected seed без I/O.
fn build_cursor(
    factory: &SmoothSelectedSourceFactory,
    track: &SmoothSelectedTrackSeed,
    first_fragment_index: usize,
) -> SmoothFragmentCursor {
    SmoothFragmentCursor::new(SmoothFragmentCursorRequest {
        http: factory.http.clone(),
        effective_manifest_target: factory.effective_manifest_target.clone(),
        fragment_secret_forwarding: factory.fragment_secret_forwarding,
        manifest: Arc::clone(&factory.manifest),
        selection: track.selection,
        expected_identity: track.expected_identity,
        media_kind: track.media_kind,
        timescale_ticks_per_second: track.timescale_ticks_per_second,
        reconstructed_track_id: track.reconstructed_track_id,
        initialization_bytes: track.initialization_bytes.clone(),
        fragment_count: track.fragment_count,
        first_fragment_index,
        policy: factory.policy.clone(),
    })
}
