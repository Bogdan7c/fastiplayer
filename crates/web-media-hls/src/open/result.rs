//! Public HLS VOD open result без раскрытия progressive runtime internals.

use std::fmt;
use std::time::Duration;

use demux_api::{ProgressiveAsyncSeekHandle, ProgressiveDemuxReadinessPort, ProgressiveDemuxer};
use media_core::Demuxer;

use crate::{
    HlsInitialPositionProofCapability, HlsSubtitleRenditionDescriptor, HlsVodStartDisposition,
};

/// Typed initial-readiness boundary HLS result-а до стирания concrete demuxer type.
#[derive(Clone, Debug)]
pub enum HlsInitialReadinessCapability {
    /// Синхронный HLS runtime уже имеет initial topology и не требует queue wait-а.
    AlreadySynchronous,
    /// Progressive runtime публикует readiness через event-driven queue port.
    Progressive(ProgressiveDemuxReadinessPort),
}

/// Неустановленный результат: manifest profile уже validated, media bytes живут за worker boundary.
pub struct HlsVodOpenResult {
    demuxer: ProgressiveDemuxer,
    subtitles: Vec<HlsSubtitleRenditionDescriptor>,
    duration: Duration,
    initial_position_proof: HlsInitialPositionProofCapability,
    start_disposition: HlsVodStartDisposition,
    initial_readiness: HlsInitialReadinessCapability,
}

impl HlsVodOpenResult {
    pub(super) fn new(
        demuxer: ProgressiveDemuxer,
        subtitles: Vec<HlsSubtitleRenditionDescriptor>,
        duration: Duration,
        initial_position_proof: HlsInitialPositionProofCapability,
        start_disposition: HlsVodStartDisposition,
    ) -> Self {
        let initial_readiness =
            HlsInitialReadinessCapability::Progressive(demuxer.readiness_port());
        Self {
            demuxer,
            subtitles,
            duration,
            initial_position_proof,
            start_disposition,
            initial_readiness,
        }
    }

    /// Возвращает generation-fenced worker seek handle для receipted preparation.
    pub fn async_seek_handle(&self) -> Option<ProgressiveAsyncSeekHandle> {
        self.demuxer.async_seek_handle()
    }

    /// Возвращает opaque capability exact deferred initial-position proof-а.
    #[must_use]
    pub fn initial_position_proof(&self) -> HlsInitialPositionProofCapability {
        self.initial_position_proof.clone()
    }

    /// Возвращает честный итог caller-owned start policy до player mutation.
    #[must_use]
    pub const fn start_disposition(&self) -> HlsVodStartDisposition {
        self.start_disposition
    }

    /// Возвращает non-consuming initial-readiness capability до type erasure.
    #[must_use]
    pub fn initial_readiness(&self) -> HlsInitialReadinessCapability {
        self.initial_readiness.clone()
    }

    /// Передаёт единственный nonblocking demux runtime app-owned staged-open owner-у.
    ///
    /// App coordinator обязан дождаться initial `TracksChanged` и завершить capability preflight до player
    /// mutation: первая key/segment/container ошибка приходит раньше этого lifecycle event.
    pub fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        Box::new(self.demuxer)
    }

    /// Subtitle renditions остаются descriptors only.
    pub fn subtitle_renditions(&self) -> &[HlsSubtitleRenditionDescriptor] {
        &self.subtitles
    }

    /// Manifest-derived finite VOD duration до первого demux event.
    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

impl fmt::Debug for HlsVodOpenResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsVodOpenResult")
            .field("subtitle_renditions", &self.subtitles)
            .field("duration", &self.duration)
            .field("initial_position_proof", &self.initial_position_proof)
            .field("start_disposition", &self.start_disposition)
            .field("initial_readiness", &self.initial_readiness)
            .finish_non_exhaustive()
    }
}
