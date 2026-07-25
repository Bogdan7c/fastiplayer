//! Smooth intent wrapper над F1 initialization segment builder.

use core::fmt;

use symphonia_format_isomp4::{
    FragmentInitializationLimits, FragmentInitializationRequest, FragmentInitializationSegment,
    build_fragmented_initialization_segment,
};

use crate::{SmoothInitializationError, SmoothMappedTrack, SmoothTrackIdentity};

/// Полный init request с обязательными caller budgets и cancellation.
pub struct SmoothInitializationRequest<'track, 'manifest, 'policy> {
    track: &'track SmoothMappedTrack<'manifest>,
    limits: &'policy FragmentInitializationLimits,
    cancellation: &'policy dyn Fn() -> bool,
}

impl<'track, 'manifest, 'policy> SmoothInitializationRequest<'track, 'manifest, 'policy> {
    /// Создаёт request без hidden defaults.
    pub const fn new(
        track: &'track SmoothMappedTrack<'manifest>,
        limits: &'policy FragmentInitializationLimits,
        cancellation: &'policy dyn Fn() -> bool,
    ) -> Self {
        Self {
            track,
            limits,
            cancellation,
        }
    }
}

/// Init bytes, связанные с identity mapped track-а.
pub struct SmoothInitializationSegment {
    identity: SmoothTrackIdentity,
    segment: FragmentInitializationSegment,
}

impl SmoothInitializationSegment {
    /// Возвращает identity, для которой построен init.
    pub const fn identity(&self) -> SmoothTrackIdentity {
        self.identity
    }

    /// Даёт bytes init segment-а transport/demux composition owner-у.
    pub fn initialization_segment_bytes(&self) -> &[u8] {
        self.segment.as_bytes()
    }

    /// Передаёт ownership bytes без копирования.
    pub fn into_initialization_segment_bytes(self) -> Vec<u8> {
        self.segment.into_bytes()
    }
}

impl fmt::Debug for SmoothInitializationSegment {
    /// Не раскрывает содержимое init segment-а.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothInitializationSegment")
            .field("identity", &self.identity)
            .field("byte_length", &self.segment.as_bytes().len())
            .finish()
    }
}

/// Строит точный F1 init и проверяет cancellation перед публикацией.
pub fn build_smooth_initialization_segment(
    request: SmoothInitializationRequest<'_, '_, '_>,
) -> Result<SmoothInitializationSegment, SmoothInitializationError> {
    if (request.cancellation)() {
        return Err(SmoothInitializationError::Cancelled);
    }
    let f1_request = FragmentInitializationRequest::new(
        request.track.reconstructed_track_id(),
        request.track.fragment_timescale(),
        request.track.initialization_codec(),
        request.limits,
        request.cancellation,
    );
    let segment =
        build_fragmented_initialization_segment(f1_request).map_err(|error| match error {
            symphonia_format_isomp4::FragmentInitializationError::Cancelled => {
                SmoothInitializationError::Cancelled
            }
            _ if (request.cancellation)() => SmoothInitializationError::Cancelled,
            error => SmoothInitializationError::Contract(error),
        })?;
    if (request.cancellation)() {
        return Err(SmoothInitializationError::Cancelled);
    }
    Ok(SmoothInitializationSegment {
        identity: request.track.identity(),
        segment,
    })
}
