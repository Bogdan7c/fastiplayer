use audio_core::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoPcmFormat,
    AudioTempoProcessReport, AudioTempoProcessor, AudioTempoProcessorConfig,
    AudioTempoProcessorFactory, AudioTempoProcessorHandle, AudioTempoRatio,
    AudioTempoReportFrameCounts, AudioTempoSegment, AudioTempoStretchedOutput,
};
use thiserror::Error;
use timestretch::{QualityMode, StreamProcessor, StretchError, StretchParams};

/// Такой же realtime FFT, как в `StreamProcessor::try_from_tempo_low_latency`.
///
/// В `timestretch 0.4.0` один `QualityMode::LowLatency` снижает cost, но не
/// меняет FFT/hop. Для S36 gate нам нужен явный bounded latency профиль.
const LOW_LATENCY_STREAM_FFT_SIZE: usize = 1024;
const LOW_LATENCY_STREAM_HOP_SIZE: usize = LOW_LATENCY_STREAM_FFT_SIZE / 4;

/// Balanced-геометрия по замерам на громком brick-wall треке
/// (`examples/fft_geometry_probe.rs`, `timestretch 0.5.0`): 0 клик-швов на
/// 1.05x-1.25x и ~19 за 30 s на 2x-4x против 36-45 у библиотечного default
/// 4096/512, при ~3x меньшем CPU (44x realtime). Замедление 0.25x-0.5x
/// остаётся артефактным у всех геометрий — R10 edge-rate follow-up.
/// Цена окна — внутренний lag вокодера ~256 ms и мягче атаки на перкуссии.
const BALANCED_STREAM_FFT_SIZE: usize = 8192;
const BALANCED_STREAM_HOP_SIZE: usize = BALANCED_STREAM_FFT_SIZE / 2;

/// Runtime profile concrete backend-а без протаскивания `timestretch` types наружу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestretchQualityMode {
    /// Минимизирует callback cost для будущего realtime path.
    LowLatency,
    /// Оставляет более качественный default profile backend-а для offline/sanity checks.
    Balanced,
    /// Включает HPSS, adaptive phase locking и residual branch backend-а.
    MaxQuality,
}

impl TimestretchQualityMode {
    fn to_backend_quality_mode(self) -> QualityMode {
        match self {
            Self::LowLatency => QualityMode::LowLatency,
            Self::Balanced => QualityMode::Balanced,
            Self::MaxQuality => QualityMode::MaxQuality,
        }
    }
}

/// Настройки concrete prototype-а, которые не являются пользовательским config-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestretchTempoSettings {
    quality_mode: TimestretchQualityMode,
}

impl TimestretchTempoSettings {
    /// Realtime-oriented profile для будущей audio callback интеграции.
    pub const REALTIME_DEFAULT: Self = Self {
        quality_mode: TimestretchQualityMode::LowLatency,
    };

    /// Quality-first default для текущего session-thread pipeline.
    ///
    /// Tempo processing идёт на session thread с ~200 ms output ring buffer
    /// запаса, а не в realtime callback, поэтому low latency здесь не нужна.
    /// `LowLatency` в `timestretch 0.4.0` дополнительно отключает HPSS,
    /// adaptive phase locking и residual branch — это слышимые PV-артефакты.
    pub const SESSION_THREAD_DEFAULT: Self = Self {
        quality_mode: TimestretchQualityMode::Balanced,
    };

    /// Создаёт settings с явно выбранным quality mode.
    #[must_use]
    pub const fn with_quality_mode(quality_mode: TimestretchQualityMode) -> Self {
        Self { quality_mode }
    }

    /// Возвращает выбранный quality mode для diagnostics/tests.
    #[must_use]
    pub const fn quality_mode(self) -> TimestretchQualityMode {
        self.quality_mode
    }
}

impl Default for TimestretchTempoSettings {
    fn default() -> Self {
        Self::SESSION_THREAD_DEFAULT
    }
}

/// Ошибки concrete adapter-а без молчаливого схлопывания backend failure в `bool`.
#[derive(Debug, Error)]
pub enum TimestretchTempoError {
    /// `timestretch 0.4.0` публично принимает только mono/stereo.
    #[error("timestretch 0.4.0 supports only mono/stereo, got {channel_count} channels")]
    UnsupportedChannelCount { channel_count: u32 },

    /// Backend или наш report вернул sample count, который не является целым числом frames.
    #[error(
        "interleaved sample count {sample_count} is not divisible by channel count {channel_count}"
    )]
    OutputSampleCountNotFrameAligned {
        sample_count: usize,
        channel_count: u32,
    },

    /// Frame count не помещается в neutral `u64` contract.
    #[error("frame count overflows u64 for {sample_count} samples and {channel_count} channels")]
    FrameCountOverflow {
        sample_count: usize,
        channel_count: u32,
    },

    /// Latency backend-а должна быть конечной и неотрицательной.
    #[error("timestretch reported invalid latency {latency_secs} seconds")]
    InvalidLatency { latency_secs: f64 },

    /// Inverted backend ratio не смог вернуться в neutral project ratio.
    #[error("failed to build project tempo ratio {multiplier}: {message}")]
    InvalidProjectRatio { multiplier: f64, message: String },

    /// Concrete backend сохранил typed `timestretch` error для диагностики.
    #[error("timestretch backend failed: {source}")]
    Backend {
        #[from]
        source: StretchError,
    },
}

/// Snapshot ratio state после последней operation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimestretchRatioSnapshot {
    /// Текущий smoothed project ratio: media-progress per output-progress.
    pub current_project_ratio: AudioTempoRatio,
    /// Target project ratio, который runtime запросил через segment.
    pub target_project_ratio: AudioTempoRatio,
    /// Текущий smoothed backend stretch ratio: output-duration per media-duration.
    pub current_backend_stretch_ratio: f64,
    /// Target backend stretch ratio после inversion project ratio.
    pub target_backend_stretch_ratio: f64,
}

/// Bounded output budget для одного `StreamProcessor::process_into` call-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestretchOutputCapacityBudget {
    input_sample_count: usize,
    ratio_limited_output_samples: usize,
    pending_output_capacity_samples: usize,
}

impl TimestretchOutputCapacityBudget {
    /// Количество input samples, для которого рассчитан budget.
    #[must_use]
    pub const fn input_sample_count(self) -> usize {
        self.input_sample_count
    }

    /// Верхняя оценка новых samples от текущего chunk-а.
    #[must_use]
    pub const fn ratio_limited_output_samples(self) -> usize {
        self.ratio_limited_output_samples
    }

    /// Capacity internal pending-output ring, которую backend может дренировать в caller buffer.
    #[must_use]
    pub const fn pending_output_capacity_samples(self) -> usize {
        self.pending_output_capacity_samples
    }

    /// Дополнительная capacity, которую caller должен иметь перед `process_into`.
    #[must_use]
    pub fn additional_output_capacity_samples(self) -> usize {
        self.ratio_limited_output_samples
            .saturating_add(self.pending_output_capacity_samples)
    }
}

/// Factory concrete timestretch processor-а для composition layer-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimestretchTempoProcessorFactory {
    settings: TimestretchTempoSettings,
}

impl TimestretchTempoProcessorFactory {
    /// Создаёт factory с явно выбранным runtime profile.
    #[must_use]
    pub const fn with_settings(settings: TimestretchTempoSettings) -> Self {
        Self { settings }
    }

    /// Возвращает settings, которые получит каждый новый processor.
    #[must_use]
    pub const fn settings(self) -> TimestretchTempoSettings {
        self.settings
    }
}

impl AudioTempoProcessorFactory for TimestretchTempoProcessorFactory {
    fn create_processor(
        &self,
        config: AudioTempoProcessorConfig,
    ) -> anyhow::Result<AudioTempoProcessorHandle> {
        Ok(Box::new(TimestretchTempoProcessor::with_settings(
            config,
            self.settings,
        )?))
    }
}

/// Concrete owner `timestretch::StreamProcessor` state-а для S36 prototype.
pub struct TimestretchTempoProcessor {
    pcm_format: AudioTempoPcmFormat,
    settings: TimestretchTempoSettings,
    active_segment: AudioTempoSegment,
    processor: StreamProcessor,
}

impl TimestretchTempoProcessor {
    /// Создаёт processor с realtime-oriented default settings.
    pub fn new(config: AudioTempoProcessorConfig) -> Result<Self, TimestretchTempoError> {
        Self::with_settings(config, TimestretchTempoSettings::default())
    }

    /// Создаёт processor и явно фиксирует concrete backend settings.
    pub fn with_settings(
        config: AudioTempoProcessorConfig,
        settings: TimestretchTempoSettings,
    ) -> Result<Self, TimestretchTempoError> {
        let pcm_format = config.pcm_format();
        ensure_supported_channel_count(pcm_format.channel_count())?;

        let active_segment = config.initial_segment();
        let processor = StreamProcessor::new(build_backend_params(
            pcm_format,
            active_segment.ratio(),
            settings,
        ));

        Ok(Self {
            pcm_format,
            settings,
            active_segment,
            processor,
        })
    }

    /// Возвращает concrete settings, с которыми создан processor.
    #[must_use]
    pub const fn settings(&self) -> TimestretchTempoSettings {
        self.settings
    }

    /// Меняет active tempo segment без пересоздания processor-а.
    ///
    /// `timestretch` сглаживает `current_ratio` примерно за 50 ms. Поэтому
    /// target меняется сразу, а reports показывают smoothed effective ratio.
    pub fn set_segment(&mut self, segment: AudioTempoSegment) -> Result<(), TimestretchTempoError> {
        let backend_stretch_ratio = backend_stretch_ratio_for_project_ratio(segment.ratio());
        self.processor.set_stretch_ratio(backend_stretch_ratio)?;
        self.active_segment = segment;
        Ok(())
    }

    /// Возвращает ratio snapshot без знания о private fields backend-а.
    pub fn ratio_snapshot(&self) -> Result<TimestretchRatioSnapshot, TimestretchTempoError> {
        let current_backend_stretch_ratio = self.processor.current_stretch_ratio();
        let target_backend_stretch_ratio = self.processor.target_stretch_ratio();

        Ok(TimestretchRatioSnapshot {
            current_project_ratio: project_ratio_from_backend_stretch_ratio(
                current_backend_stretch_ratio,
            )?,
            target_project_ratio: project_ratio_from_backend_stretch_ratio(
                target_backend_stretch_ratio,
            )?,
            current_backend_stretch_ratio,
            target_backend_stretch_ratio,
        })
    }

    /// Строит bounded capacity budget для следующего `process_decoded_media_into`.
    #[must_use]
    pub fn output_capacity_budget(
        &self,
        input_sample_count: usize,
    ) -> TimestretchOutputCapacityBudget {
        let (_, _, _, pending_output_capacity_samples) = self.processor.capacities();
        let ratio_hint = self
            .processor
            .current_stretch_ratio()
            .max(self.processor.target_stretch_ratio())
            .max(1.0);
        let ratio_limited_output_samples =
            ((input_sample_count as f64) * ratio_hint).ceil() as usize;

        TimestretchOutputCapacityBudget {
            input_sample_count,
            ratio_limited_output_samples,
            pending_output_capacity_samples,
        }
    }

    /// Возвращает raw `StreamProcessor::capacities` для diagnostics/tests.
    #[must_use]
    pub fn backend_capacities(&self) -> (usize, usize, usize, usize) {
        self.processor.capacities()
    }

    /// Возвращает algorithmic latency backend-а в neutral frame count.
    pub fn processor_latency_frames(&self) -> Result<AudioTempoFrameCount, TimestretchTempoError> {
        latency_frames_from_secs(
            self.processor.latency_secs(),
            self.pcm_format.sample_rate_hz().get(),
        )
    }

    /// Обрабатывает decoded media в caller-owned output buffer.
    ///
    /// Метод appends в `output`. Caller обязан заранее выделить capacity по
    /// `output_capacity_budget`; если capacity мала, backend возвращает typed
    /// `StretchError::BufferOverflow`.
    pub fn process_decoded_media_into(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
        output: &mut Vec<f32>,
    ) -> Result<AudioTempoProcessReport, TimestretchTempoError> {
        let output_len_before = output.len();
        self.processor
            .process_into(decoded_media.interleaved_samples(), output)?;
        self.report_for_operation(decoded_media.frame_count(), output_len_before, output.len())
    }

    /// Дренирует backend tail. Это не realtime callback path и может realloc-ить caller buffer.
    pub fn flush_into(
        &mut self,
        output: &mut Vec<f32>,
    ) -> Result<AudioTempoProcessReport, TimestretchTempoError> {
        let output_len_before = output.len();
        self.processor.flush_into(output)?;
        self.report_for_operation(AudioTempoFrameCount::ZERO, output_len_before, output.len())
    }

    /// Сбрасывает algorithmic state после seek/discontinuity и сохраняет текущий target segment.
    pub fn reset(&mut self) -> Result<AudioTempoProcessReport, TimestretchTempoError> {
        self.processor = StreamProcessor::new(build_backend_params(
            self.pcm_format,
            self.active_segment.ratio(),
            self.settings,
        ));
        self.report_from_counts(
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::ZERO,
            self.current_pending_output_frames()?,
        )
    }

    fn report_for_operation(
        &self,
        consumed_decoded_media: AudioTempoFrameCount,
        output_len_before: usize,
        output_len_after: usize,
    ) -> Result<AudioTempoProcessReport, TimestretchTempoError> {
        let produced_sample_count = output_len_after.saturating_sub(output_len_before);
        let produced_stretched_output =
            frame_count_from_interleaved_samples(produced_sample_count, self.channel_count())?;
        let pending_processor_output = self.current_pending_output_frames()?;

        self.report_from_counts(
            consumed_decoded_media,
            produced_stretched_output,
            pending_processor_output,
        )
    }

    fn report_from_counts(
        &self,
        consumed_decoded_media: AudioTempoFrameCount,
        produced_stretched_output: AudioTempoFrameCount,
        pending_processor_output: AudioTempoFrameCount,
    ) -> Result<AudioTempoProcessReport, TimestretchTempoError> {
        let effective_ratio =
            project_ratio_from_backend_stretch_ratio(self.processor.current_stretch_ratio())?;
        let report_segment =
            AudioTempoSegment::new(self.active_segment.segment_id(), effective_ratio);

        Ok(AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            report_segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media,
                produced_stretched_output,
                pending_processor_output,
                processor_latency: self.processor_latency_frames()?,
            },
        ))
    }

    fn current_pending_output_frames(&self) -> Result<AudioTempoFrameCount, TimestretchTempoError> {
        let (_, pending_output_samples, _, _) = self.processor.capacities();
        frame_count_from_interleaved_samples(pending_output_samples, self.channel_count())
    }

    fn channel_count(&self) -> AudioTempoChannelCount {
        self.pcm_format.channel_count()
    }
}

impl AudioTempoProcessor for TimestretchTempoProcessor {
    fn set_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        TimestretchTempoProcessor::set_segment(self, segment)?;
        Ok(self.report_from_counts(
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::ZERO,
            self.current_pending_output_frames()?,
        )?)
    }

    fn process_decoded_media(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<AudioTempoStretchedOutput> {
        let capacity_budget =
            self.output_capacity_budget(decoded_media.interleaved_samples().len());
        let mut interleaved_samples =
            Vec::with_capacity(capacity_budget.additional_output_capacity_samples());
        let report = self.process_decoded_media_into(decoded_media, &mut interleaved_samples)?;

        AudioTempoStretchedOutput::new(interleaved_samples, report, self.pcm_format)
    }

    fn flush(&mut self) -> anyhow::Result<AudioTempoStretchedOutput> {
        let (_, pending_output_samples, _, pending_output_capacity_samples) =
            self.backend_capacities();
        let mut interleaved_samples = Vec::with_capacity(
            pending_output_samples.saturating_add(pending_output_capacity_samples),
        );
        let report = self.flush_into(&mut interleaved_samples)?;

        AudioTempoStretchedOutput::new(interleaved_samples, report, self.pcm_format)
    }

    fn reset(&mut self) -> anyhow::Result<AudioTempoProcessReport> {
        Ok(TimestretchTempoProcessor::reset(self)?)
    }
}

fn build_backend_params(
    pcm_format: AudioTempoPcmFormat,
    project_ratio: AudioTempoRatio,
    settings: TimestretchTempoSettings,
) -> StretchParams {
    let params = StretchParams::new(backend_stretch_ratio_for_project_ratio(project_ratio))
        .with_sample_rate(pcm_format.sample_rate_hz().get())
        .with_channels(pcm_format.channel_count().get())
        .with_quality_mode(settings.quality_mode().to_backend_quality_mode());

    match settings.quality_mode() {
        TimestretchQualityMode::LowLatency => params
            .with_fft_size(LOW_LATENCY_STREAM_FFT_SIZE)
            .with_hop_size(LOW_LATENCY_STREAM_HOP_SIZE),
        TimestretchQualityMode::Balanced => params
            .with_fft_size(BALANCED_STREAM_FFT_SIZE)
            .with_hop_size(BALANCED_STREAM_HOP_SIZE),
        TimestretchQualityMode::MaxQuality => params,
    }
}

fn ensure_supported_channel_count(
    channel_count: AudioTempoChannelCount,
) -> Result<(), TimestretchTempoError> {
    match channel_count.get() {
        1 | 2 => Ok(()),
        unsupported_channel_count => Err(TimestretchTempoError::UnsupportedChannelCount {
            channel_count: unsupported_channel_count,
        }),
    }
}

fn backend_stretch_ratio_for_project_ratio(project_ratio: AudioTempoRatio) -> f64 {
    1.0 / project_ratio.as_f64()
}

fn project_ratio_from_backend_stretch_ratio(
    backend_stretch_ratio: f64,
) -> Result<AudioTempoRatio, TimestretchTempoError> {
    let project_ratio = 1.0 / backend_stretch_ratio;
    AudioTempoRatio::new(project_ratio).map_err(|error| {
        TimestretchTempoError::InvalidProjectRatio {
            multiplier: project_ratio,
            message: error.to_string(),
        }
    })
}

fn frame_count_from_interleaved_samples(
    sample_count: usize,
    channel_count: AudioTempoChannelCount,
) -> Result<AudioTempoFrameCount, TimestretchTempoError> {
    let channel_count_usize = usize::try_from(channel_count.get()).map_err(|_| {
        TimestretchTempoError::FrameCountOverflow {
            sample_count,
            channel_count: channel_count.get(),
        }
    })?;

    if sample_count % channel_count_usize != 0 {
        return Err(TimestretchTempoError::OutputSampleCountNotFrameAligned {
            sample_count,
            channel_count: channel_count.get(),
        });
    }

    let frame_count = sample_count / channel_count_usize;
    Ok(AudioTempoFrameCount::new(
        u64::try_from(frame_count).map_err(|_| TimestretchTempoError::FrameCountOverflow {
            sample_count,
            channel_count: channel_count.get(),
        })?,
    ))
}

fn latency_frames_from_secs(
    latency_secs: f64,
    sample_rate_hz: u32,
) -> Result<AudioTempoFrameCount, TimestretchTempoError> {
    if !latency_secs.is_finite() || latency_secs < 0.0 {
        return Err(TimestretchTempoError::InvalidLatency { latency_secs });
    }

    let latency_frames = (latency_secs * f64::from(sample_rate_hz)).round();
    if !(0.0..=(u64::MAX as f64)).contains(&latency_frames) {
        return Err(TimestretchTempoError::InvalidLatency { latency_secs });
    }

    Ok(AudioTempoFrameCount::new(latency_frames as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_core::{
        AudioTempoChannelCount, AudioTempoPcmFormat, AudioTempoSampleRateHz, AudioTempoSegmentId,
    };

    #[test]
    fn default_settings_use_quality_first_balanced_profile() {
        assert_eq!(
            TimestretchTempoSettings::default().quality_mode(),
            TimestretchQualityMode::Balanced
        );
        assert_eq!(
            TimestretchTempoSettings::SESSION_THREAD_DEFAULT.quality_mode(),
            TimestretchQualityMode::Balanced
        );
        assert_eq!(
            TimestretchTempoSettings::REALTIME_DEFAULT.quality_mode(),
            TimestretchQualityMode::LowLatency
        );
    }

    #[test]
    fn project_ratio_is_inverted_for_backend_stretch_ratio() {
        let double_speed = AudioTempoRatio::new(2.0).unwrap();
        let half_speed = AudioTempoRatio::new(0.5).unwrap();

        assert_eq!(backend_stretch_ratio_for_project_ratio(double_speed), 0.5);
        assert_eq!(backend_stretch_ratio_for_project_ratio(half_speed), 2.0);
    }

    #[test]
    fn reset_preserves_active_segment_ratio() {
        let pcm_format = AudioTempoPcmFormat::new(
            AudioTempoSampleRateHz::new(48_000).unwrap(),
            AudioTempoChannelCount::new(2).unwrap(),
        );
        let initial_segment = AudioTempoSegment::new(
            AudioTempoSegmentId::new(1),
            AudioTempoRatio::new(2.0).unwrap(),
        );
        let config = AudioTempoProcessorConfig::new(pcm_format, initial_segment);
        let mut processor = TimestretchTempoProcessor::new(config).unwrap();

        let report = processor.reset().unwrap();

        assert_eq!(report.effective_ratio(), initial_segment.ratio());
        assert_eq!(
            processor.ratio_snapshot().unwrap().target_project_ratio,
            initial_segment.ratio()
        );
    }

    #[test]
    fn factory_creates_neutral_processor_trait_object() {
        let pcm_format = AudioTempoPcmFormat::new(
            AudioTempoSampleRateHz::new(48_000).unwrap(),
            AudioTempoChannelCount::new(2).unwrap(),
        );
        let initial_segment =
            AudioTempoSegment::new(AudioTempoSegmentId::new(1), AudioTempoRatio::NORMAL);
        let config = AudioTempoProcessorConfig::new(pcm_format, initial_segment);
        let factory = TimestretchTempoProcessorFactory::default();

        let mut processor = factory.create_processor(config).unwrap();
        let report = processor.reset().unwrap();

        assert_eq!(report.segment_id(), initial_segment.segment_id());
        assert_eq!(report.effective_ratio(), AudioTempoRatio::NORMAL);
    }

    #[test]
    fn trait_set_segment_updates_target_ratio_without_recreating_processor() {
        let pcm_format = AudioTempoPcmFormat::new(
            AudioTempoSampleRateHz::new(48_000).unwrap(),
            AudioTempoChannelCount::new(2).unwrap(),
        );
        let initial_segment =
            AudioTempoSegment::new(AudioTempoSegmentId::new(1), AudioTempoRatio::NORMAL);
        let updated_segment = AudioTempoSegment::new(
            AudioTempoSegmentId::new(2),
            AudioTempoRatio::new(2.0).unwrap(),
        );
        let config = AudioTempoProcessorConfig::new(pcm_format, initial_segment);
        let mut processor = TimestretchTempoProcessor::new(config).unwrap();

        let report = AudioTempoProcessor::set_segment(&mut processor, updated_segment).unwrap();

        assert_eq!(report.segment_id(), updated_segment.segment_id());
        assert_eq!(
            processor.ratio_snapshot().unwrap().target_project_ratio,
            updated_segment.ratio()
        );
    }
}
