//! D5: выход processor-а обязан начинаться ровно в media time первого нового input frame.
//!
//! Два пути создания, оба встречаются в сессии: прайминг историей при первом
//! уходе с 1x (output начинается в H — конце истории) и свежий processor после
//! seek-а/старта на скорости != 1x (output начинается в PTS первого packet-а).
//!
//! Тесты строят ту же media-time модель, что и сессия: output processor-а
//! привязан к концу уже сыгранной истории H, растёт со скоростью segment-а, а при
//! смене скорости pending spans из report-а доигрываются по старой скорости.
//! Затем модель сверяется с реальным звуком: где на выходе Signalsmith оказались
//! тональные «вспышки» с известными media-позициями.

use super::*;

/// Допуск A/V рассинхрона из приёмки владельца (media-время).
const MAX_ALIGNMENT_ERROR_MS: f64 = 20.0;
/// Длина истории: сессия праймит не больше 600 ms direct-PCM.
const HISTORY_SECONDS: f64 = 0.6;
/// Первая вспышка после H и шаг между вспышками.
const FIRST_BURST_OFFSET_SECONDS: f64 = 0.2;
const BURST_PERIOD_SECONDS: f64 = 0.5;
const BURST_LENGTH_SECONDS: f64 = 0.08;
/// Сколько вспышек после H содержит поток.
const BURST_COUNT: usize = 9;
/// Смена скорости — посередине тишины между 4-й и 5-й вспышкой.
const RATE_SWITCH_AFTER_SECONDS: f64 = 2.49;
/// Хвост медиа после последней вспышки, чтобы EOF не резал её.
const STREAM_TAIL_SECONDS: f64 = 0.4;

/// PCM format сценария.
fn pcm_format(sample_rate_hz: u32, channel_count: u32) -> AudioTempoPcmFormat {
    AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(sample_rate_hz).expect("valid rate"),
        AudioTempoChannelCount::new(channel_count).expect("valid channels"),
    )
}

/// Tempo segment с явным id.
fn tempo_segment(segment_id: u64, ratio: f64) -> AudioTempoSegment {
    AudioTempoSegment::new(
        AudioTempoSegmentId::new(segment_id),
        AudioTempoRatio::new(ratio).expect("valid ratio"),
    )
}

/// Детерминированный сигнал: тишина + Hann-вспышки тона с известным центром.
struct BurstSignal {
    sample_rate_hz: f64,
    channel_count: usize,
    history_end_frame: usize,
}

impl BurstSignal {
    fn new(format: AudioTempoPcmFormat) -> Self {
        let sample_rate_hz = f64::from(format.sample_rate_hz().get());
        Self {
            sample_rate_hz,
            channel_count: format.channel_count().get() as usize,
            history_end_frame: (HISTORY_SECONDS * sample_rate_hz) as usize,
        }
    }

    /// Начало вспышки `burst_index` в media frames; индекс -1 — вспышка внутри истории.
    fn burst_start_frame(&self, burst_index: i64) -> f64 {
        self.history_end_frame as f64
            + (FIRST_BURST_OFFSET_SECONDS + BURST_PERIOD_SECONDS * burst_index as f64)
                * self.sample_rate_hz
    }

    /// Центр энергии Hann-вспышки — её середина.
    fn burst_center_frame(&self, burst_index: usize) -> f64 {
        self.burst_start_frame(burst_index as i64)
            + BURST_LENGTH_SECONDS * self.sample_rate_hz / 2.0
    }

    /// Последний media frame потока.
    fn stream_end_frame(&self) -> usize {
        let last_burst_end = self.burst_start_frame(BURST_COUNT as i64 - 1)
            + BURST_LENGTH_SECONDS * self.sample_rate_hz;
        (last_burst_end + STREAM_TAIL_SECONDS * self.sample_rate_hz) as usize
    }

    /// Interleaved samples media-диапазона `[start, start + frames)`.
    fn interleaved_samples(&self, start_frame: usize, frame_count: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frame_count * self.channel_count);
        for frame in start_frame..start_frame + frame_count {
            for channel in 0..self.channel_count {
                samples.push(self.sample_at(frame, channel));
            }
        }
        samples
    }

    fn sample_at(&self, frame: usize, channel: usize) -> f32 {
        let burst_length_frames = BURST_LENGTH_SECONDS * self.sample_rate_hz;
        // Вспышка -1 лежит в истории далеко от H: повтор истории должен быть тихим,
        // но DSP получает настоящий звуковой контекст.
        for burst_index in -1..BURST_COUNT as i64 {
            let offset = frame as f64 - self.burst_start_frame(burst_index);
            if (0.0..burst_length_frames).contains(&offset) {
                let envelope_phase = offset / burst_length_frames;
                let envelope = 0.5 - 0.5 * (std::f64::consts::TAU * envelope_phase).cos();
                let tone_hz = 660.0 + 220.0 * channel as f64;
                let time = frame as f64 / self.sample_rate_hz;
                return (0.5 * envelope * (std::f64::consts::TAU * tone_hz * time).sin()) as f32;
            }
        }
        0.0
    }
}

/// Кусочно-линейная модель сессии: output frame -> media frame.
struct SessionMediaModel {
    /// `(output_start, media_start, rate)` в порядке output-времени.
    segments: Vec<(f64, f64, f64)>,
}

impl SessionMediaModel {
    /// Processor из истории: его output 0 сессия считает media H.
    fn anchored_at_history_end(history_end_frame: f64, ratio: f64) -> Self {
        Self {
            segments: vec![(0.0, history_end_frame, ratio)],
        }
    }

    fn media_at_output(&self, output_frame: f64) -> f64 {
        let (output_start, media_start, rate) = self
            .segments
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= output_frame)
            .copied()
            .expect("model starts at output 0");
        media_start + (output_frame - output_start) * rate
    }

    /// Как `reanchor_for_rate_change_with_planned_spans`: pending по старым rate, затем новый.
    fn reanchor_for_rate_change(
        &mut self,
        emitted_output_frames: f64,
        pending_spans: &[audio_core::AudioTempoOutputSegmentSpan],
        new_ratio: f64,
    ) {
        let mut output_cursor = emitted_output_frames;
        let mut media_cursor = self.media_at_output(output_cursor);
        self.segments.retain(|(start, _, _)| *start < output_cursor);
        for span in pending_spans {
            let span_frames = span.stretched_output().frame_count().get() as f64;
            let span_ratio = span.segment().ratio().as_f64();
            self.segments
                .push((output_cursor, media_cursor, span_ratio));
            output_cursor += span_frames;
            media_cursor += span_frames * span_ratio;
        }
        self.segments.push((output_cursor, media_cursor, new_ratio));
    }
}

/// Как сценарий подаёт пакеты: мелкие первые пакеты проверяют перенос состояния.
#[derive(Clone, Copy)]
struct PacketPlan {
    leading_small_packets: usize,
    small_packet_frames: usize,
    regular_packet_frames: usize,
}

impl PacketPlan {
    const REGULAR: Self = Self {
        leading_small_packets: 0,
        small_packet_frames: 0,
        regular_packet_frames: 1024,
    };

    fn packet_frames(self, packet_index: usize) -> usize {
        if packet_index < self.leading_small_packets {
            self.small_packet_frames
        } else {
            self.regular_packet_frames
        }
    }
}

/// Как создан processor: сессия делает и то, и другое.
#[derive(Clone, Copy, Debug)]
enum StreamStart {
    /// Первый уход с 1x: `prime_decoded_history` уже сыгранным PCM.
    PrimedWithHistory,
    /// Seek/старт на скорости != 1x: история очищена, processor свежий.
    FreshWithoutHistory,
}

const BOTH_STREAM_STARTS: [StreamStart; 2] = [
    StreamStart::PrimedWithHistory,
    StreamStart::FreshWithoutHistory,
];

/// Создаёт processor на `ratio` и при необходимости праймит его историей.
fn start_processor(
    format: AudioTempoPcmFormat,
    ratio: f64,
    stream_start: StreamStart,
    history_samples: &[f32],
) -> SignalsmithTempoProcessor {
    let mut processor = SignalsmithTempoProcessor::new(AudioTempoProcessorConfig::new(
        format,
        tempo_segment(1, ratio),
    ));
    if let StreamStart::PrimedWithHistory = stream_start {
        processor
            .prime_decoded_history(
                AudioTempoDecodedMedia::from_interleaved_samples(history_samples, format)
                    .expect("history"),
            )
            .expect("prime");
    }
    processor
}

/// Результат прогона: ошибка модели для каждой найденной вспышки и счётчики.
struct RoundTripMeasurement {
    alignment_errors_ms: Vec<f64>,
    emitted_output_frames: u64,
    expected_output_frames: f64,
}

/// Processor на `creation_ratio` с первым input-ом в H, пакеты, смена на 1x посередине, EOF.
fn run_round_trip(
    format: AudioTempoPcmFormat,
    creation_ratio: f64,
    stream_start: StreamStart,
    packet_plan: PacketPlan,
) -> RoundTripMeasurement {
    let signal = BurstSignal::new(format);
    let channel_count = signal.channel_count;
    let history = signal.interleaved_samples(0, signal.history_end_frame);
    let mut processor = start_processor(format, creation_ratio, stream_start, &history);

    let mut model =
        SessionMediaModel::anchored_at_history_end(signal.history_end_frame as f64, creation_ratio);
    let switch_media_frame =
        signal.history_end_frame + (RATE_SWITCH_AFTER_SECONDS * signal.sample_rate_hz) as usize;
    let stream_end_frame = signal.stream_end_frame();
    let mut output_samples = Vec::new();
    let mut output_buffer = Vec::new();
    let mut media_cursor = signal.history_end_frame;
    let mut switched = false;
    let mut expected_output_frames = 0.0;
    let mut packet_index = 0;

    while media_cursor < stream_end_frame {
        if !switched && media_cursor >= switch_media_frame {
            let report = processor
                .set_segment(tempo_segment(2, 1.0))
                .expect("rate change");
            let emitted_frames = (output_samples.len() / channel_count) as f64;
            model.reanchor_for_rate_change(
                emitted_frames,
                report.output_progress_mapping().pending_output_segments(),
                1.0,
            );
            switched = true;
        }
        let packet_frames = packet_plan
            .packet_frames(packet_index)
            .min(stream_end_frame - media_cursor);
        let packet = signal.interleaved_samples(media_cursor, packet_frames);
        let decoded =
            AudioTempoDecodedMedia::from_interleaved_samples(&packet, format).expect("packet");
        let output = processor
            .process_decoded_media_into(decoded, &mut output_buffer)
            .expect("process");
        output_samples.extend_from_slice(output.interleaved_samples());
        expected_output_frames +=
            packet_frames as f64 / if switched { 1.0 } else { creation_ratio };
        media_cursor += packet_frames;
        packet_index += 1;
    }
    let tail = processor
        .finish_stream_into(&mut output_buffer)
        .expect("finish");
    output_samples.extend_from_slice(tail.interleaved_samples());

    let alignment_errors_ms = measure_burst_alignment(&signal, &model, &output_samples);
    RoundTripMeasurement {
        alignment_errors_ms,
        emitted_output_frames: (output_samples.len() / channel_count) as u64,
        expected_output_frames,
    }
}

/// Находит вспышки на выходе, переводит их центр энергии моделью в media и сравнивает.
fn measure_burst_alignment(
    signal: &BurstSignal,
    model: &SessionMediaModel,
    output_samples: &[f32],
) -> Vec<f64> {
    let frame_energy: Vec<f64> = output_samples
        .chunks_exact(signal.channel_count)
        .map(|frame| frame.iter().map(|&sample| f64::from(sample).powi(2)).sum())
        .collect();
    let regions = detect_energy_regions(&frame_energy);
    assert_eq!(
        regions.len(),
        BURST_COUNT,
        "every burst must appear exactly once on the output"
    );

    regions
        .iter()
        .enumerate()
        .map(|(burst_index, &(region_start, region_end))| {
            let (weighted_sum, energy_sum) =
                (region_start..region_end).fold((0.0, 0.0), |(weighted, total), frame| {
                    (
                        weighted + frame as f64 * frame_energy[frame],
                        total + frame_energy[frame],
                    )
                });
            let output_center = weighted_sum / energy_sum;
            let model_media = model.media_at_output(output_center);
            let true_media = signal.burst_center_frame(burst_index);
            (model_media - true_media) * 1_000.0 / signal.sample_rate_hz
        })
        .collect()
}

/// Регионы энергии: блоки выше 1% пика, разрывы короче 8 блоков склеиваются.
fn detect_energy_regions(frame_energy: &[f64]) -> Vec<(usize, usize)> {
    const BLOCK_FRAMES: usize = 128;
    const MAX_GAP_BLOCKS: usize = 8;
    let block_energy: Vec<f64> = frame_energy
        .chunks(BLOCK_FRAMES)
        .map(|block| block.iter().sum())
        .collect();
    let threshold = block_energy.iter().copied().fold(0.0, f64::max) * 0.01;
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (block_index, &energy) in block_energy.iter().enumerate() {
        if energy < threshold {
            continue;
        }
        let block_start = block_index * BLOCK_FRAMES;
        let block_end = (block_start + BLOCK_FRAMES).min(frame_energy.len());
        match regions.last_mut() {
            Some((_, last_end)) if block_start <= *last_end + MAX_GAP_BLOCKS * BLOCK_FRAMES => {
                *last_end = block_end;
            }
            _ => regions.push((block_start, block_end)),
        }
    }
    regions
}

/// Проверяет допуск и печатает ошибки (видны с `--nocapture`).
fn assert_aligned(label: &str, measurement: &RoundTripMeasurement) {
    let rounded: Vec<String> = measurement
        .alignment_errors_ms
        .iter()
        .map(|error| format!("{error:+.1}"))
        .collect();
    eprintln!("{label}: burst model-truth ms = [{}]", rounded.join(", "));
    for (burst_index, error_ms) in measurement.alignment_errors_ms.iter().enumerate() {
        assert!(
            error_ms.abs() <= MAX_ALIGNMENT_ERROR_MS,
            "{label}: burst {burst_index} is off by {error_ms:+.1} ms (all: [{}])",
            rounded.join(", ")
        );
    }
}

#[test]
fn output_starts_at_first_new_media_frame_for_rates_layouts_and_sample_rates() {
    for stream_start in BOTH_STREAM_STARTS {
        for (sample_rate_hz, channel_count) in [(48_000, 1), (48_000, 2), (44_100, 1), (44_100, 2)]
        {
            for creation_ratio in [1.25, 1.5, 2.0, 0.5] {
                let measurement = run_round_trip(
                    pcm_format(sample_rate_hz, channel_count),
                    creation_ratio,
                    stream_start,
                    PacketPlan::REGULAR,
                );
                assert_aligned(
                    &format!(
                        "{stream_start:?} {sample_rate_hz} Hz x{channel_count} ch, created at {creation_ratio}x"
                    ),
                    &measurement,
                );
            }
        }
    }
}

#[test]
fn output_alignment_survives_first_packets_shorter_than_latency() {
    // 37 frames — меньше и input-, и output-latency: replay растягивается на много пакетов.
    let plan = PacketPlan {
        leading_small_packets: 300,
        small_packet_frames: 37,
        regular_packet_frames: 1024,
    };
    for stream_start in BOTH_STREAM_STARTS {
        for creation_ratio in [1.5, 0.5] {
            let measurement =
                run_round_trip(pcm_format(48_000, 2), creation_ratio, stream_start, plan);
            assert_aligned(
                &format!("{stream_start:?} small first packets, {creation_ratio}x"),
                &measurement,
            );
        }
    }
}

#[test]
fn stream_emits_only_new_media_without_latency_prefix() {
    for stream_start in BOTH_STREAM_STARTS {
        for creation_ratio in [1.25, 1.5, 2.0, 0.5] {
            let measurement = run_round_trip(
                pcm_format(48_000, 1),
                creation_ratio,
                stream_start,
                PacketPlan::REGULAR,
            );
            let surplus_frames =
                measurement.emitted_output_frames as f64 - measurement.expected_output_frames;
            assert!(
                surplus_frames.abs() <= 2.0,
                "{stream_start:?} {creation_ratio}x: emitted {} output frames for {:.1} frames of new media (surplus {surplus_frames:+.1})",
                measurement.emitted_output_frames,
                measurement.expected_output_frames
            );
        }
    }
}

/// Сумма frames report-а по ordered spans.
fn span_frames(spans: &[audio_core::AudioTempoOutputSegmentSpan]) -> u64 {
    spans
        .iter()
        .map(|span| span.stretched_output().frame_count().get())
        .sum()
}

#[test]
fn report_accounting_matches_new_media_after_every_operation() {
    let format = pcm_format(48_000, 1);
    let history = vec![0.1f32; 28_800];
    for (stream_start, creation_ratio) in BOTH_STREAM_STARTS
        .into_iter()
        .flat_map(|stream_start| [(stream_start, 1.5), (stream_start, 0.5)])
    {
        let mut processor = start_processor(format, creation_ratio, stream_start, &history);
        let mut output_buffer = Vec::new();
        let mut emitted_frames = 0u64;
        let mut new_media_output_frames = 0.0f64;
        let mut active_ratio = creation_ratio;

        // Мелкие пакеты, смена скорости во время latency prefix-а, затем обычные пакеты.
        for packet_index in 0..80 {
            if packet_index == 3 {
                let report = processor.set_segment(tempo_segment(2, 1.0)).expect("set");
                let pending = report.pending_processor_output().frame_count().get();
                assert_eq!(
                    span_frames(report.output_progress_mapping().pending_output_segments()),
                    pending
                );
                assert!(
                    (emitted_frames as f64 + pending as f64 - new_media_output_frames).abs() <= 1.0,
                    "{stream_start:?} {creation_ratio}x: pending at switch {pending} must hold only new media ({new_media_output_frames:.1})"
                );
                active_ratio = 1.0;
            }
            let packet_frames = if packet_index < 3 { 100 } else { 1024 };
            let packet = vec![0.1f32; packet_frames];
            let output = processor
                .process_decoded_media_into(
                    AudioTempoDecodedMedia::from_interleaved_samples(&packet, format)
                        .expect("packet"),
                    &mut output_buffer,
                )
                .expect("process");
            let report = output.report();
            let produced = report.produced_stretched_output().frame_count().get();
            assert_eq!(output.interleaved_samples().len() as u64, produced);
            assert_eq!(
                span_frames(report.output_progress_mapping().produced_output_segments()),
                produced
            );
            emitted_frames += produced;
            new_media_output_frames += packet_frames as f64 / active_ratio;
            let pending = report.pending_processor_output().frame_count().get();
            assert!(
                (emitted_frames as f64 + pending as f64 - new_media_output_frames).abs() <= 2.0,
                "{stream_start:?} {creation_ratio}x packet {packet_index}: emitted {emitted_frames} + pending {pending} != new media {new_media_output_frames:.1}"
            );
        }

        let finish = processor
            .finish_stream_into(&mut output_buffer)
            .expect("finish");
        emitted_frames += finish
            .report()
            .produced_stretched_output()
            .frame_count()
            .get();
        assert_eq!(
            finish.report().pending_processor_output().frame_count(),
            AudioTempoFrameCount::ZERO
        );
        assert!(
            (emitted_frames as f64 - new_media_output_frames).abs() <= 2.0,
            "{stream_start:?} {creation_ratio}x: EOF total {emitted_frames} != new media {new_media_output_frames:.1}"
        );
    }
}

#[test]
fn eof_before_latency_prefix_ends_returns_only_new_media() {
    let format = pcm_format(48_000, 1);
    let history = vec![0.1f32; 28_800];
    for stream_start in BOTH_STREAM_STARTS {
        let mut processor = start_processor(format, 1.5, stream_start, &history);
        let mut output_buffer = Vec::new();
        let packet = vec![0.1f32; 600];
        let first = processor
            .process_decoded_media_into(
                AudioTempoDecodedMedia::from_interleaved_samples(&packet, format).expect("packet"),
                &mut output_buffer,
            )
            .expect("process");
        // Весь выход короткого первого пакета — latency prefix, наружу он не идёт.
        let first_report = first.report();
        assert_eq!(
            first_report.produced_stretched_output().frame_count().get(),
            0,
            "{stream_start:?}"
        );
        assert_eq!(
            first_report.pending_processor_output().frame_count().get(),
            400,
            "{stream_start:?}"
        );

        let finish = processor
            .finish_stream_into(&mut output_buffer)
            .expect("finish");
        let finish_frames = finish
            .report()
            .produced_stretched_output()
            .frame_count()
            .get();
        assert_eq!(finish_frames, 400, "{stream_start:?}");
        assert_eq!(finish.interleaved_samples().len(), 400, "{stream_start:?}");
    }
}
