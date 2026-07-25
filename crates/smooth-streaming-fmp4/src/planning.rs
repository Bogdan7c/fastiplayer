//! Sealed Smooth fragment planning: manifest window, relative path и F1 intent.

use core::fmt;

use smooth_streaming_manifest_core::{
    SmoothCustomAttributesRender, SmoothFragmentUrlRenderContext,
};
use symphonia_format_isomp4::{
    FragmentBaseDecodeTime, FragmentMediaKind, FragmentSampleDefaults,
    FragmentTrackReconstructionIntent,
};

use crate::mapping::SmoothMappedMediaState;
use crate::{
    SmoothFragmentIndex, SmoothFragmentPlanError, SmoothMappedTrack, SmoothTrackIdentity,
    SmoothTrackMediaKind,
};

/// Точный manifest interval в clock выбранного stream-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmoothManifestWindow {
    start: u64,
    end_exclusive: u64,
    timescale_ticks_per_second: u32,
}

impl SmoothManifestWindow {
    /// Возвращает inclusive start.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Возвращает exclusive end.
    pub const fn end_exclusive(self) -> u64 {
        self.end_exclusive
    }

    /// Возвращает точный stream clock.
    pub const fn timescale_ticks_per_second(self) -> u32 {
        self.timescale_ticks_per_second
    }

    /// Возвращает exact manifest duration.
    pub const fn duration_ticks(self) -> u64 {
        self.end_exclusive - self.start
    }
}

/// Относительный путь, который нельзя случайно вывести через Display.
pub struct SmoothFragmentRelativePath(String);

impl SmoothFragmentRelativePath {
    /// Явно передаёт относительный path transport owner-у.
    pub fn transport_relative_path(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SmoothFragmentRelativePath {
    /// Редактирует содержимое и показывает только длину.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothFragmentRelativePath")
            .field("byte_length", &self.0.len())
            .finish()
    }
}

/// Запрос planning-а одного fragment-а.
pub struct SmoothFragmentPlanRequest<'track, 'manifest, 'policy> {
    track: &'track SmoothMappedTrack<'manifest>,
    fragment_index: SmoothFragmentIndex,
    cancellation: &'policy dyn Fn() -> bool,
}

impl<'track, 'manifest, 'policy> SmoothFragmentPlanRequest<'track, 'manifest, 'policy> {
    /// Создаёт request без URL/provider policy.
    pub const fn new(
        track: &'track SmoothMappedTrack<'manifest>,
        fragment_index: SmoothFragmentIndex,
        cancellation: &'policy dyn Fn() -> bool,
    ) -> Self {
        Self {
            track,
            fragment_index,
            cancellation,
        }
    }
}

/// Sealed plan связывает identity, path, window и exact F1 track intent.
pub struct SmoothFragmentPlan {
    identity: SmoothTrackIdentity,
    fragment_index: SmoothFragmentIndex,
    relative_path: SmoothFragmentRelativePath,
    manifest_window: SmoothManifestWindow,
    reconstruction_intent: FragmentTrackReconstructionIntent,
    media_state: SmoothMappedMediaState,
}

impl fmt::Debug for SmoothFragmentPlan {
    /// Не раскрывает path; nested Debug также length-only.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothFragmentPlan")
            .field("identity", &self.identity)
            .field("fragment_index", &self.fragment_index)
            .field("relative_path", &self.relative_path)
            .field("manifest_window", &self.manifest_window)
            .finish_non_exhaustive()
    }
}

impl SmoothFragmentPlan {
    /// Возвращает mapped identity.
    pub const fn identity(&self) -> SmoothTrackIdentity {
        self.identity
    }

    /// Возвращает timeline fragment index.
    pub const fn fragment_index(&self) -> SmoothFragmentIndex {
        self.fragment_index
    }

    /// Возвращает относительный transport path.
    pub const fn relative_path(&self) -> &SmoothFragmentRelativePath {
        &self.relative_path
    }

    /// Возвращает exact manifest window.
    pub const fn manifest_window(&self) -> SmoothManifestWindow {
        self.manifest_window
    }

    /// Возвращает sealed F1 intent внутри adapter-а.
    pub(crate) const fn reconstruction_intent(&self) -> FragmentTrackReconstructionIntent {
        self.reconstruction_intent
    }

    /// Возвращает закрытое media state для исчерпывающей admission matrix.
    pub(crate) const fn media_state(&self) -> SmoothMappedMediaState {
        self.media_state
    }
}

/// Строит plan исключительно из mapped track и manifest template/timeline.
pub fn plan_smooth_fragment(
    request: SmoothFragmentPlanRequest<'_, '_, '_>,
) -> Result<SmoothFragmentPlan, SmoothFragmentPlanError> {
    if (request.cancellation)() {
        return Err(SmoothFragmentPlanError::Cancelled);
    }
    let stream = request.track.stream();
    if request.fragment_index.get() >= stream.timeline().fragment_count() {
        return Err(SmoothFragmentPlanError::FragmentNotFound);
    }
    let fragment = stream
        .timeline()
        .fragment_at(request.fragment_index.get())
        .map_err(SmoothFragmentPlanError::Timeline)?;
    let start = fragment.start().ticks();
    let end_exclusive = start
        .checked_add(fragment.duration_ticks())
        .ok_or(SmoothFragmentPlanError::WindowOverflow)?;
    let relative_path = stream
        .url_template()
        .render_fragment_path(SmoothFragmentUrlRenderContext::new(
            /* bitrate */
            request.track.identity().bitrate().get(),
            /* start_time_ticks */
            start,
            /* custom_attributes */
            SmoothCustomAttributesRender::Values(request.track.custom_attributes()),
        ))
        .map_err(SmoothFragmentPlanError::PathRendering)?;
    let media_kind = match request.track.identity().media_kind() {
        SmoothTrackMediaKind::Video => FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        SmoothTrackMediaKind::Audio => FragmentMediaKind::AudioWithoutRandomAccessRequirement,
    };
    let reconstruction_intent = FragmentTrackReconstructionIntent::new(
        /* track_id */
        request.track.reconstructed_track_id(),
        /* base_decode_time */
        FragmentBaseDecodeTime::new(start),
        /* media_kind */
        media_kind,
        /* sample_defaults */
        FragmentSampleDefaults::absent(),
    );
    let plan = SmoothFragmentPlan {
        identity: request.track.identity(),
        fragment_index: request.fragment_index,
        relative_path: SmoothFragmentRelativePath(relative_path),
        manifest_window: SmoothManifestWindow {
            start,
            end_exclusive,
            timescale_ticks_per_second: request.track.timescale_ticks_per_second(),
        },
        reconstruction_intent,
        media_state: request.track.media_state(),
    };
    if (request.cancellation)() {
        return Err(SmoothFragmentPlanError::Cancelled);
    }
    Ok(plan)
}
