//! Формат последнего удачного кадра AC-3 / E-AC-3, который decoder сообщает наружу.
//!
//! Вещательный AC-3 посреди потока переключает 2.0 ↔ 5.1 (реклама ↔ фильм), а
//! E-AC-3 и half-rate потоки могут сменить частоту. player-core пересобирает audio
//! output при смене decoded spec-а без потери A/V sync (`session/audio_spec_change.rs`),
//! поэтому adapter **не приводит** кадры к первому формату: каждый кадр уходит в своём
//! настоящем layout-е и частоте, а `decoded_output_spec` описывает ровно последний
//! buffer (контракт `audio_core::AudioDecoder`).
//!
//! Здесь живёт только то, что нужно между кадрами: формат для `decoded_output_spec`
//! и длительность последнего кадра для маскировки битого packet-а тишиной.

use audio_core::AudioChannelLayout;
use tracing::info;

/// Формат и длительность последнего удачно декодированного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PublishedFrameFormat {
    /// Частота дискретизации кадра.
    pub(super) sample_rate: u32,
    /// Canonical layout кадра.
    pub(super) layout: AudioChannelLayout,
    /// Samples на канал в кадре (для тишины вместо битого packet-а).
    pub(super) frames_per_packet: usize,
}

/// Владелец опубликованного формата decoder-а.
#[derive(Debug, Default)]
pub(super) struct LastFrameFormat {
    /// `None` до первого удачного кадра.
    published: Option<PublishedFrameFormat>,
}

impl LastFrameFormat {
    /// Возвращает формат последнего удачного кадра, если кадр уже был.
    pub(super) fn published(&self) -> Option<PublishedFrameFormat> {
        self.published
    }

    /// Публикует формат удачного кадра; смену формата пишет в диагностический лог.
    pub(super) fn publish(&mut self, current: PublishedFrameFormat, track_id: u32) {
        if let Some(previous) = self.published
            && (previous.sample_rate, previous.layout) != (current.sample_rate, current.layout)
        {
            info!(
                track_id,
                previous_sample_rate = previous.sample_rate,
                previous_layout = %previous.layout,
                sample_rate = current.sample_rate,
                layout = %current.layout,
                "AC-3 family program format changed mid-stream"
            );
        }
        self.published = Some(current);
    }

    /// Тишина длиной последнего удачного кадра в его формате.
    ///
    /// `None`, пока кадра не было: тогда длительность и формат неизвестны.
    pub(super) fn concealment_silence(&self) -> Option<Vec<f32>> {
        self.published.map(|published| {
            vec![0.0; published.frames_per_packet * published.layout.channel_count() as usize]
        })
    }
}
