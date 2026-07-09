use audio_core::{
    AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoPcmFormat, AudioTempoProcessReport,
    AudioTempoProcessor, AudioTempoProcessorConfig, AudioTempoProcessorFactory,
    AudioTempoProcessorHandle, AudioTempoReportFrameCounts, AudioTempoSegment,
    AudioTempoStretchedOutput,
};
use signalsmith_stretch::Stretch;

/// Runtime factory для `player-core` composition boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SignalsmithTempoProcessorFactory;

impl AudioTempoProcessorFactory for SignalsmithTempoProcessorFactory {
    fn create_processor(
        &self,
        config: AudioTempoProcessorConfig,
    ) -> anyhow::Result<AudioTempoProcessorHandle> {
        Ok(Box::new(SignalsmithTempoProcessor::new(config)))
    }
}

/// Concrete tempo processor над Signalsmith Stretch.
///
/// У Signalsmith нет ratio-setter-а: соотношение задаётся размерами
/// input/output каждого `process` вызова. Processor владеет целочисленным
/// учётом кадров внутри активного segment-а, поэтому долгосрочное соотношение
/// точное, а дробный остаток переносится между decoded packets.
pub struct SignalsmithTempoProcessor {
    stretch: Stretch,
    pcm_format: AudioTempoPcmFormat,
    active_segment: AudioTempoSegment,

    /// Decoded media frames, принятые в активном segment-е.
    consumed_frames_in_segment: u64,

    /// Stretched output frames, выданные в активном segment-е.
    produced_frames_in_segment: u64,
}

impl SignalsmithTempoProcessor {
    /// Создаёт processor с default-пресетом Signalsmith под PCM format.
    #[must_use]
    pub fn new(config: AudioTempoProcessorConfig) -> Self {
        let pcm_format = config.pcm_format();
        let stretch = Stretch::preset_default(
            pcm_format.channel_count().get(),
            pcm_format.sample_rate_hz().get(),
        );

        Self {
            stretch,
            pcm_format,
            active_segment: config.initial_segment(),
            consumed_frames_in_segment: 0,
            produced_frames_in_segment: 0,
        }
    }

    /// Возвращает суммарную латентность backend-а в frames.
    fn latency_frames(&self) -> u64 {
        (self.stretch.input_latency() + self.stretch.output_latency()) as u64
    }

    /// Считает, сколько output frames должен выдать текущий process вызов.
    ///
    /// Инвариант: `produced == floor(consumed / rate)` после каждого вызова,
    /// поэтому долгосрочное соотношение точно равно playback rate, а ошибка
    /// округления не накапливается.
    fn due_output_frames(&self) -> u64 {
        let rate = self.active_segment.ratio().as_f64();
        let target_produced = (self.consumed_frames_in_segment as f64 / rate).floor() as u64;
        target_produced.saturating_sub(self.produced_frames_in_segment)
    }

    /// Собирает typed report для одного process/flush вызова.
    fn report_for_operation(
        &self,
        consumed_frames: u64,
        produced_frames: u64,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        Ok(AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            self.active_segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media: AudioTempoFrameCount::new(consumed_frames),
                produced_stretched_output: AudioTempoFrameCount::new(produced_frames),
                pending_processor_output: AudioTempoFrameCount::new(
                    self.stretch.output_latency() as u64
                ),
                processor_latency: AudioTempoFrameCount::new(self.latency_frames()),
            },
        ))
    }
}

impl AudioTempoProcessor for SignalsmithTempoProcessor {
    /// Меняет active segment; новый rate применяется только к будущим frames.
    ///
    /// Счётчики заякориваются на границе segment-а: иначе учёт задним числом
    /// пересчитал бы всю историю по новому rate и потребовал бы burst output.
    fn set_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.active_segment = segment;
        self.consumed_frames_in_segment = 0;
        self.produced_frames_in_segment = 0;
        self.report_for_operation(0, 0)
    }

    fn process_decoded_media(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<AudioTempoStretchedOutput> {
        let channels = self.pcm_format.channel_count().get() as usize;
        let consumed_frames = decoded_media.frame_count().get();
        self.consumed_frames_in_segment = self
            .consumed_frames_in_segment
            .saturating_add(consumed_frames);

        let produced_frames = self.due_output_frames();
        self.produced_frames_in_segment = self
            .produced_frames_in_segment
            .saturating_add(produced_frames);

        let mut interleaved_samples = vec![0.0f32; produced_frames as usize * channels];
        self.stretch.process(
            decoded_media.interleaved_samples(),
            &mut interleaved_samples,
        );

        let report = self.report_for_operation(consumed_frames, produced_frames)?;
        AudioTempoStretchedOutput::new(interleaved_samples, report, self.pcm_format)
    }

    /// Выдаёт хвост backend-а (output latency) один раз на EOF boundary.
    fn flush(&mut self) -> anyhow::Result<AudioTempoStretchedOutput> {
        let channels = self.pcm_format.channel_count().get() as usize;
        let tail_frames = self.stretch.output_latency() as u64;
        let mut interleaved_samples = vec![0.0f32; tail_frames as usize * channels];
        self.stretch.flush(&mut interleaved_samples);
        self.produced_frames_in_segment =
            self.produced_frames_in_segment.saturating_add(tail_frames);

        let report = self.report_for_operation(0, tail_frames)?;
        AudioTempoStretchedOutput::new(interleaved_samples, report, self.pcm_format)
    }

    /// Сбрасывает внутреннее состояние backend-а на seek/media boundary.
    fn reset(&mut self) -> anyhow::Result<AudioTempoProcessReport> {
        self.stretch.reset();
        self.consumed_frames_in_segment = 0;
        self.produced_frames_in_segment = 0;
        self.report_for_operation(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use audio_core::{
        AudioTempoChannelCount, AudioTempoRatio, AudioTempoSampleRateHz, AudioTempoSegmentId,
    };

    use super::*;

    fn stereo_48k_format() -> AudioTempoPcmFormat {
        AudioTempoPcmFormat::new(
            AudioTempoSampleRateHz::new(48_000).expect("valid rate"),
            AudioTempoChannelCount::new(2).expect("valid channels"),
        )
    }

    fn processor_at_rate(rate: f64) -> SignalsmithTempoProcessor {
        let segment = AudioTempoSegment::new(
            AudioTempoSegmentId::new(1),
            AudioTempoRatio::new(rate).expect("valid ratio"),
        );
        SignalsmithTempoProcessor::new(AudioTempoProcessorConfig::new(stereo_48k_format(), segment))
    }

    fn process_packets(
        processor: &mut SignalsmithTempoProcessor,
        packet_frames: usize,
        packet_count: usize,
    ) -> u64 {
        let format = stereo_48k_format();
        let packet = vec![0.25f32; packet_frames * 2];
        let mut produced_frames = 0u64;
        for _ in 0..packet_count {
            let decoded = AudioTempoDecodedMedia::from_interleaved_samples(&packet, format)
                .expect("frame-aligned packet");
            let output = processor
                .process_decoded_media(decoded)
                .expect("process should succeed");
            produced_frames += output
                .report()
                .produced_stretched_output()
                .frame_count()
                .get();
        }
        produced_frames
    }

    #[test]
    fn long_run_output_matches_playback_rate_exactly() {
        // 2x: 100 пакетов по 960 frames -> ровно floor(96000 / 2) output frames.
        let mut processor = processor_at_rate(2.0);
        let produced = process_packets(&mut processor, 960, 100);
        assert_eq!(produced, 48_000);

        // 0.5x: output вдвое длиннее входа.
        let mut processor = processor_at_rate(0.5);
        let produced = process_packets(&mut processor, 960, 100);
        assert_eq!(produced, 192_000);
    }

    #[test]
    fn fractional_rate_accumulates_without_drift() {
        // 1.1x не делит 960 нацело: дробный остаток должен переноситься.
        let mut processor = processor_at_rate(1.1);
        let produced = process_packets(&mut processor, 960, 110);
        let expected = ((960u64 * 110) as f64 / 1.1).floor() as u64;
        assert!(
            produced.abs_diff(expected) <= 1,
            "produced={produced} expected~{expected}"
        );
    }

    #[test]
    fn segment_change_applies_only_to_future_frames() {
        let mut processor = processor_at_rate(1.0);
        let produced_before = process_packets(&mut processor, 960, 10);
        assert_eq!(produced_before, 9_600);

        // Смена на 2x не требует burst-пересчёта истории.
        processor
            .set_segment(AudioTempoSegment::new(
                AudioTempoSegmentId::new(2),
                AudioTempoRatio::new(2.0).expect("valid ratio"),
            ))
            .expect("set_segment should succeed");

        let produced_after = process_packets(&mut processor, 960, 10);
        assert_eq!(produced_after, 4_800);
    }

    #[test]
    fn factory_creates_neutral_trait_object_and_reports_latency() {
        let factory = SignalsmithTempoProcessorFactory;
        let segment = AudioTempoSegment::new(
            AudioTempoSegmentId::new(1),
            AudioTempoRatio::new(2.0).expect("valid ratio"),
        );
        let mut processor: AudioTempoProcessorHandle = factory
            .create_processor(AudioTempoProcessorConfig::new(stereo_48k_format(), segment))
            .expect("factory should create processor");

        let report = processor.reset().expect("reset should succeed");
        assert!(
            report.processor_latency().frame_count().get() > 0,
            "signalsmith должен отчитывать ненулевую латентность"
        );
    }

    #[test]
    fn reset_clears_segment_accounting() {
        let mut processor = processor_at_rate(2.0);
        process_packets(&mut processor, 960, 3);
        processor.reset().expect("reset should succeed");
        assert_eq!(processor.consumed_frames_in_segment, 0);
        assert_eq!(processor.produced_frames_in_segment, 0);

        // После reset учёт начинается заново без долга прошлых кадров.
        let produced = process_packets(&mut processor, 960, 10);
        assert_eq!(produced, 4_800);
    }

    #[test]
    fn flush_emits_bounded_tail() {
        let mut processor = processor_at_rate(2.0);
        process_packets(&mut processor, 960, 10);
        let tail = processor.flush().expect("flush should succeed");
        let tail_frames = tail
            .report()
            .produced_stretched_output()
            .frame_count()
            .get();
        assert!(tail_frames > 0, "flush должен выдать хвост");
        assert!(
            tail_frames < 48_000,
            "хвост bounded латентностью, не секундами: {tail_frames}"
        );
    }
}
