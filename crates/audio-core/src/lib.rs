//! Нейтральные audio-контракты для orchestration слоя плеера.
//!
//! Этот crate не знает о CPAL, Symphonia, Opus, WGPU или UI shell. Он описывает
//! только данные и trait-boundary, через которые `player-core` управляет audio
//! decode/output lifecycle.

#![forbid(unsafe_code)]

mod channel_layout;
mod decode_capability;
mod tempo;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use thiserror::Error;

pub use channel_layout::{AudioChannelLayout, AudioChannelLayoutError, AudioChannelPosition};
pub use decode_capability::{
    AudioDecodeCapability, AudioDecodeCapabilityProvider, AudioDecodeCapabilityQueryError,
    AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily, AudioDecodeCodecFamilyQuery,
};
pub use tempo::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoFrameSpan,
    AudioTempoOutputProgressMapping, AudioTempoOutputSegmentCollection,
    AudioTempoOutputSegmentSpan, AudioTempoOutputSegmentSpans, AudioTempoPcmFormat,
    AudioTempoProcessReport, AudioTempoProcessor, AudioTempoProcessorConfig,
    AudioTempoProcessorError, AudioTempoProcessorFactory, AudioTempoProcessorHandle,
    AudioTempoRatio, AudioTempoRatioInvalidReason, AudioTempoReportFrameCounts,
    AudioTempoSampleRateHz, AudioTempoSegment, AudioTempoSegmentId, AudioTempoStretchedOutput,
};

/// Codec-neutral параметры audio track-а, по которым factory создаёт decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDecoderConfig {
    /// Track ID нужен backend-ам для проверки выбранного track-а и packet metadata.
    track_id: u32,

    /// Container codec id (`A_OPUS`, `A_AAC`, `A_FLAC` и т.д.).
    codec_id: String,

    /// Sample rate из track metadata, если demuxer уже смог его определить.
    sample_rate: Option<u32>,

    /// Количество каналов из track metadata, если demuxer уже смог его определить.
    channels: Option<u32>,

    /// Codec private / extra data из container track metadata.
    codec_private: Option<Vec<u8>>,
}

impl AudioDecoderConfig {
    /// Создаёт config с уже известным decoded audio spec и без codec-private данных.
    #[must_use]
    pub fn new(
        track_id: u32,
        codec_id: impl Into<String>,
        sample_rate: u32,
        channels: u32,
    ) -> Self {
        Self::from_track_metadata(track_id, codec_id, Some(sample_rate), Some(channels))
    }

    /// Создаёт config из container metadata, где audio spec может быть ещё неполным.
    #[must_use]
    pub fn from_track_metadata(
        track_id: u32,
        codec_id: impl Into<String>,
        sample_rate: Option<u32>,
        channels: Option<u32>,
    ) -> Self {
        Self {
            track_id,
            codec_id: codec_id.into(),
            sample_rate,
            channels,
            codec_private: None,
        }
    }

    /// Добавляет codec-private bytes, если container сообщил decoder extra data.
    #[must_use]
    pub fn with_codec_private(mut self, codec_private: Option<Vec<u8>>) -> Self {
        self.codec_private = codec_private;
        self
    }

    /// Возвращает Track ID в числовом виде для backend adapter-ов.
    #[must_use]
    pub const fn track_id(&self) -> u32 {
        self.track_id
    }

    /// Возвращает исходный container codec id.
    #[must_use]
    pub fn codec_id(&self) -> &str {
        &self.codec_id
    }

    /// Возвращает sample rate из track metadata, если container сообщил его до decode.
    #[must_use]
    pub const fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    /// Возвращает channel count из track metadata, если container сообщил его до decode.
    #[must_use]
    pub const fn channels(&self) -> Option<u32> {
        self.channels
    }

    /// Возвращает codec-private bytes без раскрытия storage ownership.
    #[must_use]
    pub fn codec_private(&self) -> Option<&[u8]> {
        self.codec_private.as_deref()
    }

    /// Проверяет общие инварианты metadata до обращения к concrete decoder backend-у.
    pub fn validate_probe_metadata(&self) -> Result<()> {
        self.validate_optional_metadata_value(self.sample_rate, "sample_rate")?;
        self.validate_optional_metadata_value(self.channels, "channels")?;

        Ok(())
    }

    /// Проверяет, что metadata value либо отсутствует, либо содержит ненулевое значение.
    fn validate_optional_metadata_value(
        &self,
        value: Option<u32>,
        value_name: &'static str,
    ) -> Result<()> {
        if value == Some(0) {
            return Err(AudioDecoderError::InvalidConfig {
                codec_id: self.codec_id.clone(),
                reason: format!("{value_name} must be greater than 0 when present"),
            }
            .into());
        }

        Ok(())
    }
}

/// Временная база исходного audio track-а для decoder packet timing metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacketTimeBase {
    /// Числитель длительности одного track unit-а в секундах.
    numer: u32,

    /// Знаменатель длительности одного track unit-а в секундах.
    denom: u32,
}

impl AudioPacketTimeBase {
    /// Создаёт audio packet time base только из ненулевых частей.
    #[must_use]
    pub const fn new(numer: u32, denom: u32) -> Option<Self> {
        if numer == 0 || denom == 0 {
            None
        } else {
            Some(Self { numer, denom })
        }
    }

    /// Возвращает числитель временной базы.
    #[must_use]
    pub const fn numer(self) -> u32 {
        self.numer
    }

    /// Возвращает знаменатель временной базы.
    #[must_use]
    pub const fn denom(self) -> u32 {
        self.denom
    }
}

/// Raw timing metadata encoded audio packet-а в исходных units container track-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacketTiming {
    /// Временная база, в которой выражены raw packet units.
    time_base: Option<AudioPacketTimeBase>,

    /// Presentation timestamp в исходных signed track units.
    pts_units: i64,

    /// Decode timestamp в исходных signed track units, если container сообщил DTS.
    dts_units: Option<i64>,

    /// Packet duration в исходных unsigned track units, если container сообщил duration.
    duration_units: Option<u64>,
}

impl AudioPacketTiming {
    /// Создаёт явно отсутствующий raw timing без sample-rate догадок.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            time_base: None,
            pts_units: 0,
            dts_units: None,
            duration_units: None,
        }
    }

    /// Создаёт timing из raw container units одного audio track-а.
    #[must_use]
    pub const fn from_track_units(
        time_base: AudioPacketTimeBase,
        pts_units: i64,
        dts_units: Option<i64>,
        duration_units: Option<u64>,
    ) -> Self {
        Self {
            time_base: Some(time_base),
            pts_units,
            dts_units,
            duration_units,
        }
    }

    /// Возвращает временную базу raw units, если demuxer смог её сохранить.
    #[must_use]
    pub const fn time_base(&self) -> Option<AudioPacketTimeBase> {
        self.time_base
    }

    /// Возвращает raw PTS units для backend packet boundary.
    #[must_use]
    pub const fn pts_units(&self) -> i64 {
        self.pts_units
    }

    /// Возвращает raw DTS units для backend packet boundary.
    #[must_use]
    pub const fn dts_units(&self) -> Option<i64> {
        self.dts_units
    }

    /// Возвращает raw duration units для backend packet boundary.
    #[must_use]
    pub const fn duration_units(&self) -> Option<u64> {
        self.duration_units
    }
}

/// Codec-neutral ссылка на encoded audio packet.
#[derive(Debug, Clone, Copy)]
pub struct EncodedAudioPacket<'a> {
    /// Track ID packet-а.
    track_id: u32,

    /// Raw timing metadata для concrete decoder packet API.
    timing: AudioPacketTiming,

    /// Encoded codec bytes.
    data: &'a [u8],
}

impl<'a> EncodedAudioPacket<'a> {
    /// Создаёт immutable packet view без привязки к конкретному decoder backend-у.
    #[must_use]
    pub const fn new(track_id: u32, timing: AudioPacketTiming, data: &'a [u8]) -> Self {
        Self {
            track_id,
            timing,
            data,
        }
    }

    /// Создаёт packet view для backend-ов/tests, где container timing отсутствует.
    #[must_use]
    pub const fn without_timing(track_id: u32, data: &'a [u8]) -> Self {
        Self::new(track_id, AudioPacketTiming::unknown(), data)
    }

    /// Возвращает track id packet-а.
    #[must_use]
    pub const fn track_id(&self) -> u32 {
        self.track_id
    }

    /// Возвращает encoded bytes.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Возвращает raw timing metadata packet-а.
    #[must_use]
    pub const fn timing(&self) -> AudioPacketTiming {
        self.timing
    }
}

/// Codec-neutral audio decoder contract для playback pipeline.
pub trait AudioDecoder: Send {
    /// Декодирует один encoded audio packet в interleaved PCM f32.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>>;

    /// Сбрасывает codec state после seek или discontinuity.
    fn reset(&mut self) -> Result<()>;

    /// Возвращает sample rate decoded PCM.
    fn sample_rate(&self) -> u32;

    /// Возвращает количество interleaved channels decoded PCM.
    fn channels(&self) -> u32;

    /// Возвращает neutral layout interleaved decoded PCM.
    ///
    /// После успешного непустого decode layout обязан описывать lanes именно
    /// того buffer-а, который вернул [`Self::decode`].
    fn channel_layout(&self) -> Option<AudioChannelLayout>;

    /// Возвращает атомарный spec последнего decoded PCM buffer-а.
    ///
    /// До первого decode backend может ещё не знать sample rate. `None`
    /// сохраняет это состояние отдельно от ошибочного или пустого packet-а.
    fn decoded_output_spec(&self) -> Option<AudioOutputSpec> {
        let sample_rate = self.sample_rate();
        let channel_layout = self.channel_layout()?;
        (sample_rate > 0 && channel_layout.channel_count() == self.channels())
            .then(|| AudioOutputSpec::new(sample_rate, channel_layout))
    }
}

/// Владеющий handle codec-neutral decoder-а, который можно хранить в player pipeline.
pub type AudioDecoderHandle = Box<dyn AudioDecoder + Send>;

/// Нейтральная фабрика audio decoder-а без знания о Symphonia/Opus registry.
pub trait AudioDecoderFactory: Send + Sync {
    /// Создаёт decoder под выбранный audio track config.
    fn create_decoder(&self, config: AudioDecoderConfig) -> Result<AudioDecoderHandle>;
}

/// Ошибки audio decoder factory, которые caller может downcast-ить из anyhow.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioDecoderError {
    /// Codec id не удалось связать с concrete decoder backend-ом.
    #[error("Audio codec {codec_id} is not supported by the audio decoder factory")]
    UnsupportedCodec {
        /// Container codec id, для которого factory не смог создать decoder.
        codec_id: String,
    },

    /// Track metadata непригодна для создания decoder-а.
    #[error("Invalid audio decoder config for {codec_id}: {reason}")]
    InvalidConfig {
        /// Container codec id, для которого config оказался некорректным.
        codec_id: String,

        /// Человекочитаемая причина отказа.
        reason: String,
    },

    /// Backend декодировал PCM, но его channel semantics нельзя безопасно
    /// выразить или преобразовать через neutral positional layout.
    #[error("Unsupported decoded PCM channel layout for {codec_id}: {layout}")]
    UnsupportedDecodedChannelLayout {
        /// Container codec id активного decoder-а.
        codec_id: String,

        /// Диагностическое описание concrete layout-а без concrete type в API.
        layout: String,
    },
}

/// Decoded PCM spec, по которому composition layer создаёт audio output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOutputSpec {
    /// Sample rate decoded PCM, полученный только после первого decode.
    pub sample_rate: u32,

    /// Позиции и канонический порядок interleaved channels decoded PCM.
    pub channel_layout: AudioChannelLayout,
}

impl AudioOutputSpec {
    /// Создаёт единый decoded PCM spec без дублирования channel count-а.
    #[must_use]
    pub const fn new(sample_rate: u32, channel_layout: AudioChannelLayout) -> Self {
        Self {
            sample_rate,
            channel_layout,
        }
    }

    /// Возвращает количество scalar lanes в interleaved frame.
    #[must_use]
    pub const fn channels(self) -> u32 {
        self.channel_layout.channel_count()
    }
}

/// Намерение записи PCM, по которому output выбирает обработку амплитуды.
///
/// Тип не содержит playback rate или имя tempo backend-а: concrete output
/// знает только происхождение samples, необходимое для своей output policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputWriteIntent {
    /// PCM пришёл напрямую от decoder-а и не должен меняться защитой tempo-output.
    DirectDecodedPcm,

    /// PCM произведён tempo processor-ом и требует limiter/soft-clip защиты.
    TempoProcessed,
}

/// Количество PCM frames на входной оси concrete audio output-а.
///
/// Один frame содержит `AudioOutputSpec::channels` interleaved samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioOutputInputFrameCount(usize);

impl AudioOutputInputFrameCount {
    /// Создаёт typed input-frame count.
    #[must_use]
    pub const fn new(frame_count: usize) -> Self {
        Self(frame_count)
    }

    /// Возвращает количество input frames.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Количество PCM frames на оси output stream-а после channel conversion/resampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioOutputStreamFrameCount(usize);

impl AudioOutputStreamFrameCount {
    /// Создаёт typed output-stream frame count.
    #[must_use]
    pub const fn new(frame_count: usize) -> Self {
        Self(frame_count)
    }

    /// Возвращает количество output-stream frames.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Результат одной записи PCM после всех преобразований concrete output-а.
///
/// Input и output counts намеренно имеют разные типы: channel conversion и
/// resampling могут менять число scalar samples и frames, поэтому caller не
/// должен сравнивать исходный `samples.len()` с количеством samples в ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOutputWriteReport {
    /// Сколько input PCM frames было передано output-у.
    submitted_input_frames: AudioOutputInputFrameCount,

    /// Сколько output-stream frames подготовлено после conversion/resampling.
    prepared_output_frames: AudioOutputStreamFrameCount,

    /// Сколько полных output-stream frames фактически поставлено в очередь.
    queued_output_frames: AudioOutputStreamFrameCount,
}

impl AudioOutputWriteReport {
    /// Создаёт отчёт и проверяет, что queued output не превышает prepared output.
    pub fn try_new(
        submitted_input_frames: AudioOutputInputFrameCount,
        prepared_output_frames: AudioOutputStreamFrameCount,
        queued_output_frames: AudioOutputStreamFrameCount,
    ) -> std::result::Result<Self, AudioOutputWriteError> {
        if queued_output_frames > prepared_output_frames {
            return Err(AudioOutputWriteError::InvalidFrameAccounting {
                prepared_output_frames: prepared_output_frames.get(),
                queued_output_frames: queued_output_frames.get(),
            });
        }

        Ok(Self {
            submitted_input_frames,
            prepared_output_frames,
            queued_output_frames,
        })
    }

    /// Создаёт отчёт о полной записи всех подготовленных output frames.
    #[must_use]
    pub const fn complete(
        submitted_input_frames: AudioOutputInputFrameCount,
        output_frames: AudioOutputStreamFrameCount,
    ) -> Self {
        Self {
            submitted_input_frames,
            prepared_output_frames: output_frames,
            queued_output_frames: output_frames,
        }
    }

    /// Возвращает количество переданных input frames.
    #[must_use]
    pub const fn submitted_input_frames(self) -> AudioOutputInputFrameCount {
        self.submitted_input_frames
    }

    /// Возвращает количество подготовленных output-stream frames.
    #[must_use]
    pub const fn prepared_output_frames(self) -> AudioOutputStreamFrameCount {
        self.prepared_output_frames
    }

    /// Возвращает количество поставленных в очередь output-stream frames.
    #[must_use]
    pub const fn queued_output_frames(self) -> AudioOutputStreamFrameCount {
        self.queued_output_frames
    }

    /// Подтверждает, что output поставил в очередь все подготовленные frames.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.prepared_output_frames.0 == self.queued_output_frames.0
    }

    /// Возвращает число потерянных output-stream frames.
    #[must_use]
    pub const fn dropped_output_frames(self) -> AudioOutputStreamFrameCount {
        AudioOutputStreamFrameCount(
            self.prepared_output_frames
                .0
                .saturating_sub(self.queued_output_frames.0),
        )
    }
}

/// Ошибки neutral output-write boundary до принятия PCM устройством.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputWriteError {
    /// Concrete output получил невозможное нулевое число каналов.
    #[error("Audio output {boundary} channel count must be greater than zero")]
    InvalidChannelCount {
        /// Имя input/output границы с некорректным channel count.
        boundary: &'static str,
    },

    /// Output не может безопасно преобразовать известный input layout в
    /// channel count выбранного stream-а.
    #[error(
        "Audio output cannot convert input layout {input_layout} to {output_channels} output channels"
    )]
    UnsupportedChannelConversion {
        /// Authoritative layout decoded PCM.
        input_layout: AudioChannelLayout,

        /// Число interleaved lanes, которое запросил concrete output stream.
        output_channels: u32,
    },

    /// Input interleaved slice заканчивается посреди PCM frame.
    #[error(
        "Audio output input has {input_samples} samples, which is not divisible by {input_channels} input channels"
    )]
    InputNotFrameAligned {
        /// Число scalar samples во входном slice.
        input_samples: usize,

        /// Число каналов одного input frame.
        input_channels: usize,
    },

    /// Подготовленный output заканчивается посреди stream frame.
    #[error(
        "Audio output prepared {prepared_output_samples} samples, which is not divisible by {output_channels} output channels"
    )]
    PreparedOutputNotFrameAligned {
        /// Число scalar samples после conversion/resampling.
        prepared_output_samples: usize,

        /// Число каналов output stream-а.
        output_channels: usize,
    },

    /// Фактически queued output заканчивается посреди stream frame.
    #[error(
        "Audio output queued {queued_output_samples} samples, which is not divisible by {output_channels} output channels"
    )]
    QueuedOutputNotFrameAligned {
        /// Число scalar samples, фактически поставленных в ring.
        queued_output_samples: usize,

        /// Число каналов output stream-а.
        output_channels: usize,
    },

    /// Concrete output сообщил невозможное количество queued frames.
    #[error(
        "Audio output queued {queued_output_frames} frames after preparing only {prepared_output_frames} frames"
    )]
    InvalidFrameAccounting {
        /// Число подготовленных output-stream frames.
        prepared_output_frames: usize,

        /// Число поставленных в очередь output-stream frames.
        queued_output_frames: usize,
    },
}

/// Нейтральный snapshot audible progress и конца уже переданного output PCM.
///
/// `submitted_output_end_position` включает samples в ring buffer и samples,
/// которые callback уже передал устройству, но DAC ещё не воспроизвёл. Silence,
/// которым callback закрывает underrun, не является submitted media PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOutputClockTiming {
    /// Output-clock позиция, которая уже предположительно дошла до DAC.
    audible_output_position: Duration,

    /// Output-clock позиция конца всего реально принятого output PCM.
    submitted_output_end_position: Duration,
}

impl AudioOutputClockTiming {
    /// Создаёт согласованный snapshot и не допускает отрицательный output tail.
    #[must_use]
    pub fn new(audible_output_position: Duration, submitted_output_end_position: Duration) -> Self {
        Self {
            audible_output_position,
            submitted_output_end_position: submitted_output_end_position
                .max(audible_output_position),
        }
    }

    /// Возвращает позицию output clock, уже дошедшую до DAC.
    #[must_use]
    pub fn audible_output_position(self) -> Duration {
        self.audible_output_position
    }

    /// Возвращает позицию конца всего принятого output PCM.
    #[must_use]
    pub fn submitted_output_end_position(self) -> Duration {
        self.submitted_output_end_position
    }

    /// Возвращает длительность принятого, но ещё не слышимого PCM tail.
    #[must_use]
    pub fn submitted_output_tail(self) -> Duration {
        self.submitted_output_end_position
            .saturating_sub(self.audible_output_position)
    }
}

/// Нейтральная фабрика audio output-а без знания о CPAL или concrete backend-е.
pub trait AudioOutputFactory: Send + Sync {
    /// Создаёт output под decoded audio spec.
    fn create_output(&self, spec: AudioOutputSpec) -> Result<Box<dyn PlayerAudioOutput>>;
}

/// Нейтральный output contract, которым управляет playback pipeline.
///
/// Output создаётся внутри playback worker-а и дальше остаётся thread-local.
/// Этот contract поэтому не требует `Send`: concrete backend вроде CPAL может
/// владеть `!Send` stream-ом, не добавляя proxy thread в горячий путь записи PCM.
pub trait PlayerAudioOutput {
    /// Записывает interleaved PCM согласно policy и возвращает counts в обеих осях.
    fn write_samples(
        &mut self,
        samples: &[f32],
        intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError>;

    /// Запускает output stream и снимает lifecycle freeze без clock jump-а.
    ///
    /// Если backend фактически продолжал работать в logical pause, concrete
    /// output не должен повторно вызывать несовместимый backend `play`.
    fn play(&mut self) -> Result<()>;

    /// Ставит output stream на паузу и атомарно фиксирует его clock snapshot.
    ///
    /// Concrete output обязан сериализовать freeze с data callback-ом. После
    /// успешного возврата `PlayerAudioClock::now` и `output_timing` не должны
    /// продвигаться до следующего успешного `play`.
    fn pause_and_freeze_clock(&mut self) -> Result<AudioOutputClockTiming>;

    /// Очищает queued samples для seek generation и возвращает ack generation.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> Result<u64>;

    /// Применяет volume для последующих samples.
    fn set_volume(&mut self, volume: f32);

    /// Возвращает текущий уровень output buffer-а в миллисекундах.
    fn buffer_level_ms(&self) -> f64;

    /// Возвращает playback clock как нейтральный shared handle.
    fn clock(&self) -> Arc<dyn PlayerAudioClock>;
}

/// Нейтральный playback clock для A/V sync и EOF-drain diagnostics.
pub trait PlayerAudioClock: Send + Sync {
    /// Возвращает текущую playback позицию относительно clock base.
    fn now(&self) -> Duration;

    /// Возвращает audible clock и конец всего уже принятого output PCM.
    fn output_timing(&self) -> AudioOutputClockTiming;

    /// Сбрасывает clock state после seek/output clear.
    fn reset(&self);

    /// Возвращает количество output callbacks, где stream недополучил samples.
    fn underrun_callbacks(&self) -> u64;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        AudioDecoderConfig, AudioDecoderError, AudioOutputClockTiming, AudioOutputInputFrameCount,
        AudioOutputStreamFrameCount, AudioOutputWriteError, AudioOutputWriteReport,
        AudioPacketTimeBase, AudioPacketTiming, EncodedAudioPacket,
    };

    #[test]
    fn output_clock_timing_preserves_non_negative_submitted_tail() {
        let audible_position = Duration::from_millis(75);
        let stale_submitted_position = Duration::from_millis(50);

        let timing = AudioOutputClockTiming::new(audible_position, stale_submitted_position);

        assert_eq!(timing.audible_output_position(), audible_position);
        assert_eq!(timing.submitted_output_end_position(), audible_position);
        assert_eq!(timing.submitted_output_tail(), Duration::ZERO);
    }

    #[test]
    fn output_write_report_keeps_input_and_stream_frame_domains_separate() {
        let input_frames = AudioOutputInputFrameCount::new(1_024);
        let resampled_output_frames = AudioOutputStreamFrameCount::new(1_115);
        let complete_report =
            AudioOutputWriteReport::complete(input_frames, resampled_output_frames);

        assert_eq!(complete_report.submitted_input_frames(), input_frames);
        assert_eq!(
            complete_report.prepared_output_frames(),
            resampled_output_frames
        );
        assert_eq!(
            complete_report.queued_output_frames(),
            resampled_output_frames
        );
        assert!(complete_report.is_complete());
        assert_eq!(
            complete_report.dropped_output_frames(),
            AudioOutputStreamFrameCount::new(0)
        );

        let partial_report = AudioOutputWriteReport::try_new(
            input_frames,
            resampled_output_frames,
            AudioOutputStreamFrameCount::new(1_114),
        )
        .expect("queued output below prepared output should form a partial report");
        assert!(!partial_report.is_complete());
        assert_eq!(
            partial_report.dropped_output_frames(),
            AudioOutputStreamFrameCount::new(1)
        );
    }

    #[test]
    fn output_write_report_rejects_impossible_frame_accounting() {
        let error = AudioOutputWriteReport::try_new(
            AudioOutputInputFrameCount::new(1_024),
            AudioOutputStreamFrameCount::new(1_024),
            AudioOutputStreamFrameCount::new(1_025),
        )
        .expect_err("queued output cannot exceed prepared output");

        assert_eq!(
            error,
            AudioOutputWriteError::InvalidFrameAccounting {
                prepared_output_frames: 1_024,
                queued_output_frames: 1_025,
            }
        );
    }

    #[test]
    fn decoder_config_accepts_missing_probe_values_but_rejects_zero_values() {
        let deferred_config = AudioDecoderConfig::from_track_metadata(2, "A_AAC", None, None);
        deferred_config
            .validate_probe_metadata()
            .expect("absent probe values should remain valid for lazy decode");

        let invalid_config = AudioDecoderConfig::from_track_metadata(2, "A_AAC", Some(0), Some(2));
        let error = invalid_config
            .validate_probe_metadata()
            .expect_err("zero sample_rate should be rejected when present");

        assert_eq!(
            error
                .downcast_ref::<AudioDecoderError>()
                .expect("config validation should keep typed error"),
            &AudioDecoderError::InvalidConfig {
                codec_id: "A_AAC".to_string(),
                reason: "sample_rate must be greater than 0 when present".to_string(),
            }
        );
    }

    #[test]
    fn packet_timing_preserves_container_time_units() {
        let time_base =
            AudioPacketTimeBase::new(1, 1_000).expect("container time base should be valid");
        let timing = AudioPacketTiming::from_track_units(time_base, 1_234, Some(1_200), Some(23));
        let packet = EncodedAudioPacket::new(2, timing, b"aac");

        assert_eq!(packet.track_id(), 2);
        assert_eq!(packet.data(), b"aac");
        assert_eq!(packet.timing().time_base(), Some(time_base));
        assert_eq!(packet.timing().pts_units(), 1_234);
        assert_eq!(packet.timing().dts_units(), Some(1_200));
        assert_eq!(packet.timing().duration_units(), Some(23));
    }

    #[test]
    fn decoder_error_carries_unsupported_codec_info() {
        let error: anyhow::Error = AudioDecoderError::UnsupportedCodec {
            codec_id: "A_NOT_A_REAL_CODEC".to_string(),
        }
        .into();

        assert_eq!(
            error
                .downcast_ref::<AudioDecoderError>()
                .expect("unsupported codec should stay typed"),
            &AudioDecoderError::UnsupportedCodec {
                codec_id: "A_NOT_A_REAL_CODEC".to_string(),
            }
        );
    }
}
