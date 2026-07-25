use std::time::Duration;

use bytes::Bytes;
use media_core::{PacketPresentationWindow, TrackId};

/// Сырой audio packet, который ждёт decode из-за backpressure audio buffer.
pub(crate) struct PendingAudioPacket {
    /// Track ID нужен, чтобы не отправить packet неактивного audio track в decoder.
    track_id: TrackId,

    /// Presentation timestamp packet-а на абсолютной media timeline.
    pts: Duration,

    /// Raw packet timing в container units для decoder boundary.
    timing: audio_core::AudioPacketTiming,

    /// Exact presentation window переносится вместе с payload без интерпретации до PCM boundary.
    presentation_window: PacketPresentationWindow,

    /// Seek generation, в котором packet был прочитан из demuxer.
    generation: u64,

    /// Encoded audio bytes владеют shared payload-ом без копии между demuxer и player queue.
    encoded_bytes: Bytes,
}

impl PendingAudioPacket {
    /// Возвращает audio track, которому принадлежит packet.
    #[must_use]
    pub(crate) const fn track_id(&self) -> TrackId {
        self.track_id
    }

    /// Возвращает presentation timestamp на абсолютной media timeline.
    #[must_use]
    pub(crate) const fn pts(&self) -> Duration {
        self.pts
    }

    /// Возвращает raw container timing для decoder boundary.
    #[must_use]
    pub(crate) const fn timing(&self) -> audio_core::AudioPacketTiming {
        self.timing
    }

    /// Возвращает exact presentation window без его интерпретации.
    #[must_use]
    pub(crate) const fn presentation_window(&self) -> PacketPresentationWindow {
        self.presentation_window
    }

    /// Возвращает seek generation, в котором demuxer прочитал packet.
    #[must_use]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Заимствует encoded payload без копирования и передачи ownership.
    #[must_use]
    pub(crate) fn encoded_bytes(&self) -> &[u8] {
        &self.encoded_bytes
    }

    /// Создаёт test packet с явно неограниченным presentation window.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new_unbounded(
        track_id: TrackId,
        pts: Duration,
        _dts: Option<Duration>,
        _duration: Option<Duration>,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self::new_with_presentation_window(
            track_id,
            pts,
            PacketPresentationWindow::Unbounded,
            generation,
            encoded_bytes,
        )
    }

    /// Создаёт test packet с явным presentation window для lifecycle-проверок очереди.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new_with_presentation_window(
        track_id: TrackId,
        pts: Duration,
        presentation_window: PacketPresentationWindow,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            timing: audio_core::AudioPacketTiming::unknown(),
            presentation_window,
            generation,
            encoded_bytes,
        }
    }

    /// Создаёт ожидающий audio packet с raw container timing и exact window metadata.
    #[must_use]
    pub(crate) fn with_timing(
        track_id: TrackId,
        pts: Duration,
        timing: audio_core::AudioPacketTiming,
        presentation_window: PacketPresentationWindow,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            timing,
            presentation_window,
            generation,
            encoded_bytes,
        }
    }
}
