use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use audio_core::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoPcmFormat,
    AudioTempoProcessorConfig, AudioTempoRatio, AudioTempoSampleRateHz, AudioTempoSegment,
    AudioTempoSegmentId,
};
use audio_timestretch::{
    TimestretchQualityMode, TimestretchTempoError, TimestretchTempoProcessor,
    TimestretchTempoSettings,
};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use timestretch::{StreamProcessor, StretchError, StretchParams};

#[global_allocator]
static COUNTING_ALLOCATOR: CountingAllocator = CountingAllocator;

const SAMPLE_RATE_HZ: u32 = 48_000;
const CHANNEL_COUNT: u32 = 2;
const CHUNK_FRAMES: usize = 512;
const SYNTHETIC_SIGNAL_FRAMES: usize = SAMPLE_RATE_HZ as usize;
const REAL_AUDIO_SANITY_FRAMES: usize = 44_100;
const OUTPUT_LENGTH_TOLERANCE: f64 = 0.08;
const COMMON_RATE_PEAK_LIMIT: f32 = 4.0;
const BOUNDARY_RATE_PEAK_LIMIT: f32 = 8.0;
const MIN_USEFUL_RMS: f32 = 1.0e-4;
const MAX_REALTIME_GATE_LATENCY_MS: f64 = 40.0;

struct CountingAllocator;

thread_local! {
    static ALLOCATION_PROBE: AllocationProbe = const { AllocationProbe::new() };
}

struct AllocationProbe {
    enabled: Cell<bool>,
    alloc_calls: Cell<usize>,
    realloc_calls: Cell<usize>,
}

impl AllocationProbe {
    const fn new() -> Self {
        Self {
            enabled: Cell::new(false),
            alloc_calls: Cell::new(0),
            realloc_calls: Cell::new(0),
        }
    }

    fn reset_and_enable(&self) {
        self.alloc_calls.set(0);
        self.realloc_calls.set(0);
        self.enabled.set(true);
    }

    fn disable(&self) {
        self.enabled.set(false);
    }

    fn record_alloc(&self) {
        if self.enabled.get() {
            self.alloc_calls
                .set(self.alloc_calls.get().saturating_add(1));
        }
    }

    fn record_realloc(&self) {
        if self.enabled.get() {
            self.realloc_calls
                .set(self.realloc_calls.get().saturating_add(1));
        }
    }

    fn counts(&self) -> AllocationCounts {
        AllocationCounts {
            alloc_calls: self.alloc_calls.get(),
            realloc_calls: self.realloc_calls.get(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AllocationCounts {
    alloc_calls: usize,
    realloc_calls: usize,
}

struct AllocationProbeGuard;

impl AllocationProbeGuard {
    fn start() -> Self {
        ALLOCATION_PROBE.with(AllocationProbe::reset_and_enable);
        Self
    }
}

impl Drop for AllocationProbeGuard {
    fn drop(&mut self) {
        ALLOCATION_PROBE.with(AllocationProbe::disable);
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        ALLOCATION_PROBE.with(AllocationProbe::record_alloc);
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        ALLOCATION_PROBE.with(AllocationProbe::record_realloc);
        new_pointer
    }
}

#[test]
fn published_stream_api_surface_is_usable_but_fixed_buffer_readme_methods_are_absent() {
    let params = StretchParams::new(1.25)
        .with_sample_rate(SAMPLE_RATE_HZ)
        .with_channels(CHANNEL_COUNT);
    let mut processor = StreamProcessor::new(params);
    let (_, _, input_capacity_samples, pending_capacity_samples) = processor.capacities();

    assert!(input_capacity_samples > 0);
    assert!(pending_capacity_samples > 0);
    assert!(processor.latency_secs().is_finite());
    assert!(processor.latency_secs() > 0.0);
    assert!((processor.target_stretch_ratio() - 1.25).abs() < f64::EPSILON);

    processor.set_stretch_ratio(0.75).unwrap();
    assert!((processor.target_stretch_ratio() - 0.75).abs() < f64::EPSILON);

    // S36 checkpoint: Context7/README описывают fixed-buffer методы, но в
    // crates.io `0.4.0` публичный compile-checked path здесь именно
    // `process_into` с caller-owned `Vec` capacity.
}

#[test]
fn playback_rate_direction_matches_project_contract() -> Result<()> {
    let input = synthetic_stereo_signal(SYNTHETIC_SIGNAL_FRAMES);

    let normal_output = process_entire_signal(1.0, &input)?;
    let double_speed_output = process_entire_signal(2.0, &input)?;
    let half_speed_output = process_entire_signal(0.5, &input)?;

    assert_relative_output_len(1.0, input.len(), normal_output.samples.len());
    assert_relative_output_len(2.0, input.len(), double_speed_output.samples.len());
    assert_relative_output_len(0.5, input.len(), half_speed_output.samples.len());
    assert!(double_speed_output.samples.len() < normal_output.samples.len());
    assert!(half_speed_output.samples.len() > normal_output.samples.len());
    assert_signal_is_usable(&double_speed_output.samples, COMMON_RATE_PEAK_LIMIT);
    assert_signal_is_usable(&half_speed_output.samples, COMMON_RATE_PEAK_LIMIT);

    Ok(())
}

#[test]
fn dynamic_ratio_updates_target_immediately_and_current_ratio_converges() -> Result<()> {
    let pcm_format = stereo_pcm_format()?;
    let mut processor = processor_for_playback_rate(1.0)?;
    let first_segment =
        AudioTempoSegment::new(AudioTempoSegmentId::new(2), AudioTempoRatio::new(0.5)?);
    let input = synthetic_stereo_signal(SYNTHETIC_SIGNAL_FRAMES);
    let mut output_chunk = Vec::new();

    process_chunks(
        &mut processor,
        pcm_format,
        &input[..input.len() / 2],
        &mut output_chunk,
    )?;
    processor.set_segment(first_segment)?;

    let after_set = processor.ratio_snapshot()?;
    assert_eq!(after_set.target_project_ratio, first_segment.ratio());
    assert!(after_set.current_project_ratio.as_f64() > first_segment.ratio().as_f64());

    process_chunks(
        &mut processor,
        pcm_format,
        &input[input.len() / 2..],
        &mut output_chunk,
    )?;
    let after_processing = processor.ratio_snapshot()?;
    assert_eq!(after_processing.target_project_ratio, first_segment.ratio());
    assert!(
        after_processing.current_project_ratio.as_f64() < after_set.current_project_ratio.as_f64(),
        "current ratio should move toward the slower target"
    );

    Ok(())
}

#[test]
fn boundary_rates_are_stable_synced_and_bounded() -> Result<()> {
    let input = synthetic_stereo_signal(SYNTHETIC_SIGNAL_FRAMES);

    for playback_rate in [0.25, 4.0] {
        let processed = process_entire_signal(playback_rate, &input)
            .with_context(|| format!("processing boundary rate {playback_rate}x"))?;

        assert_relative_output_len(playback_rate, input.len(), processed.samples.len());
        assert_signal_is_usable(&processed.samples, BOUNDARY_RATE_PEAK_LIMIT);
        assert!(processed.max_process_budget_samples < input.len().saturating_mul(16));
        assert_eq!(processed.final_pending_output_frames, 0);
    }

    Ok(())
}

#[test]
fn low_latency_profile_stays_inside_v1_latency_budget() -> Result<()> {
    let processor = processor_for_playback_rate(1.0)?;
    let latency_frames = processor.processor_latency_frames()?.get();
    let latency_ms = latency_frames as f64 * 1000.0 / SAMPLE_RATE_HZ as f64;

    assert!(
        latency_ms <= MAX_REALTIME_GATE_LATENCY_MS,
        "latency {latency_ms:.2} ms exceeds S36 realtime gate budget"
    );

    Ok(())
}

#[test]
fn process_into_hot_path_has_no_heap_allocations_after_warmup() -> Result<()> {
    let pcm_format = stereo_pcm_format()?;
    let mut processor = processor_for_playback_rate(0.5)?;
    let input_chunk = synthetic_stereo_signal(CHUNK_FRAMES);
    let initial_budget = processor.output_capacity_budget(input_chunk.len());
    let mut output_chunk = Vec::with_capacity(initial_budget.additional_output_capacity_samples());

    for _ in 0..32 {
        output_chunk.clear();
        process_one_chunk(&mut processor, pcm_format, &input_chunk, &mut output_chunk)?;
    }

    let allocation_guard = AllocationProbeGuard::start();
    for _ in 0..16 {
        output_chunk.clear();
        process_one_chunk(&mut processor, pcm_format, &input_chunk, &mut output_chunk)?;
    }
    let counts = ALLOCATION_PROBE.with(AllocationProbe::counts);
    drop(allocation_guard);

    assert_eq!(counts.alloc_calls, 0, "hot path allocated after warmup");
    assert_eq!(counts.realloc_calls, 0, "hot path reallocated after warmup");

    Ok(())
}

#[test]
fn undersized_output_capacity_returns_typed_buffer_overflow() -> Result<()> {
    let pcm_format = stereo_pcm_format()?;
    let mut processor = processor_for_playback_rate(1.0)?;
    let input_chunk = synthetic_stereo_signal(CHUNK_FRAMES);
    let mut too_small_output = Vec::with_capacity(input_chunk.len() - 1);
    let decoded_media = AudioTempoDecodedMedia::from_interleaved_samples(&input_chunk, pcm_format)?;

    let error = processor
        .process_decoded_media_into(decoded_media, &mut too_small_output)
        .unwrap_err();

    assert!(matches!(
        error,
        TimestretchTempoError::Backend {
            source: StretchError::BufferOverflow { .. }
        }
    ));

    Ok(())
}

#[test]
fn real_audio_wav_sanity_is_finite_non_silent_and_rate_directed() -> Result<()> {
    let sample = decode_real_audio_sample(
        &repository_root().join("test-assets/audio/music_sample.wav"),
        REAL_AUDIO_SANITY_FRAMES,
    )?;

    assert_eq!(sample.sample_rate_hz, 44_100);
    assert_eq!(sample.channel_count, 2);

    let double_speed = process_signal_with_format(2.0, &sample.samples, sample.pcm_format()?)?;
    let half_speed = process_signal_with_format(0.5, &sample.samples, sample.pcm_format()?)?;

    assert_relative_output_len(2.0, sample.samples.len(), double_speed.samples.len());
    assert_relative_output_len(0.5, sample.samples.len(), half_speed.samples.len());
    assert_signal_is_usable(&double_speed.samples, COMMON_RATE_PEAK_LIMIT);
    assert_signal_is_usable(&half_speed.samples, COMMON_RATE_PEAK_LIMIT);

    Ok(())
}

fn processor_for_playback_rate(playback_rate: f64) -> Result<TimestretchTempoProcessor> {
    processor_for_playback_rate_with_format(playback_rate, stereo_pcm_format()?)
}

fn processor_for_playback_rate_with_format(
    playback_rate: f64,
    pcm_format: AudioTempoPcmFormat,
) -> Result<TimestretchTempoProcessor> {
    let segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(1),
        AudioTempoRatio::new(playback_rate)?,
    );
    let config = AudioTempoProcessorConfig::new(pcm_format, segment);
    Ok(TimestretchTempoProcessor::with_settings(
        config,
        TimestretchTempoSettings::with_quality_mode(TimestretchQualityMode::LowLatency),
    )?)
}

fn stereo_pcm_format() -> Result<AudioTempoPcmFormat> {
    Ok(AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(SAMPLE_RATE_HZ)?,
        AudioTempoChannelCount::new(CHANNEL_COUNT)?,
    ))
}

fn process_entire_signal(playback_rate: f64, input: &[f32]) -> Result<ProcessedSignal> {
    process_signal_with_format(playback_rate, input, stereo_pcm_format()?)
}

fn process_signal_with_format(
    playback_rate: f64,
    input: &[f32],
    pcm_format: AudioTempoPcmFormat,
) -> Result<ProcessedSignal> {
    let mut processor = processor_for_playback_rate_with_format(playback_rate, pcm_format)?;
    let mut all_output = Vec::new();
    let mut output_chunk = Vec::new();
    let mut max_process_budget_samples = 0usize;

    for input_chunk in input.chunks(CHUNK_FRAMES * pcm_format.channel_count().get() as usize) {
        let budget = processor.output_capacity_budget(input_chunk.len());
        max_process_budget_samples =
            max_process_budget_samples.max(budget.additional_output_capacity_samples());
        ensure_additional_capacity(
            &mut output_chunk,
            budget.additional_output_capacity_samples(),
        );

        output_chunk.clear();
        process_one_chunk(&mut processor, pcm_format, input_chunk, &mut output_chunk)?;
        all_output.extend_from_slice(&output_chunk);
    }

    output_chunk.clear();
    processor.flush_into(&mut output_chunk)?;
    all_output.extend_from_slice(&output_chunk);

    let final_pending_output_frames = processor
        .backend_capacities()
        .1
        .checked_div(pcm_format.channel_count().get() as usize)
        .context("channel count should be non-zero")?;

    Ok(ProcessedSignal {
        samples: all_output,
        max_process_budget_samples,
        final_pending_output_frames,
    })
}

fn process_chunks(
    processor: &mut TimestretchTempoProcessor,
    pcm_format: AudioTempoPcmFormat,
    input: &[f32],
    output_chunk: &mut Vec<f32>,
) -> Result<()> {
    for input_chunk in input.chunks(CHUNK_FRAMES * pcm_format.channel_count().get() as usize) {
        let budget = processor.output_capacity_budget(input_chunk.len());
        ensure_additional_capacity(output_chunk, budget.additional_output_capacity_samples());
        output_chunk.clear();
        process_one_chunk(processor, pcm_format, input_chunk, output_chunk)?;
    }
    Ok(())
}

fn process_one_chunk(
    processor: &mut TimestretchTempoProcessor,
    pcm_format: AudioTempoPcmFormat,
    input_chunk: &[f32],
    output_chunk: &mut Vec<f32>,
) -> Result<()> {
    let decoded_media = AudioTempoDecodedMedia::from_interleaved_samples(input_chunk, pcm_format)?;
    let report = processor.process_decoded_media_into(decoded_media, output_chunk)?;
    let produced_frames = frames_from_samples(output_chunk.len(), pcm_format)?;

    assert_eq!(
        report.consumed_decoded_media().frame_count(),
        decoded_media.frame_count()
    );
    assert_eq!(
        report.produced_stretched_output().frame_count(),
        produced_frames
    );
    assert!(report.processor_latency().frame_count().get() > 0);
    assert!(report.effective_ratio().as_f64().is_finite());

    Ok(())
}

fn ensure_additional_capacity(output: &mut Vec<f32>, required_additional_capacity: usize) {
    let available_capacity = output.capacity().saturating_sub(output.len());
    if available_capacity < required_additional_capacity {
        output.reserve_exact(required_additional_capacity - available_capacity);
    }
}

fn frames_from_samples(
    sample_count: usize,
    pcm_format: AudioTempoPcmFormat,
) -> Result<AudioTempoFrameCount> {
    let channel_count = pcm_format.channel_count().get() as usize;
    if sample_count % channel_count != 0 {
        bail!("sample count {sample_count} is not divisible by channel count {channel_count}");
    }
    Ok(AudioTempoFrameCount::new(
        u64::try_from(sample_count / channel_count).context("frame count should fit into u64")?,
    ))
}

fn assert_relative_output_len(playback_rate: f64, input_samples: usize, output_samples: usize) {
    let expected_samples = input_samples as f64 / playback_rate;
    let relative_error = ((output_samples as f64) - expected_samples).abs() / expected_samples;
    assert!(
        relative_error <= OUTPUT_LENGTH_TOLERANCE,
        "playback_rate={playback_rate}, input_samples={input_samples}, \
         output_samples={output_samples}, expected_samples={expected_samples:.0}, \
         relative_error={relative_error:.3}"
    );
}

fn assert_signal_is_usable(samples: &[f32], peak_limit: f32) {
    assert!(!samples.is_empty());
    assert!(samples.iter().all(|sample| sample.is_finite()));

    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0f32, f32::max);
    assert!(peak <= peak_limit, "peak {peak} exceeds limit {peak_limit}");

    let rms = signal_rms(samples);
    assert!(rms > MIN_USEFUL_RMS, "rms {rms} is too close to silence");
}

fn signal_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_square = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    (sum_square / samples.len() as f64).sqrt() as f32
}

fn synthetic_stereo_signal(frames: usize) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * CHANNEL_COUNT as usize);

    for frame_index in 0..frames {
        let time_secs = frame_index as f32 / SAMPLE_RATE_HZ as f32;
        let left = sine_sample(time_secs, 220.0, 0.32) + sine_sample(time_secs, 880.0, 0.08);
        let right = sine_sample(time_secs, 330.0, 0.28) + sine_sample(time_secs, 660.0, 0.06);
        samples.extend([left, right]);
    }

    samples
}

fn sine_sample(time_secs: f32, frequency_hz: f32, amplitude: f32) -> f32 {
    (std::f32::consts::TAU * frequency_hz * time_secs).sin() * amplitude
}

#[derive(Debug)]
struct ProcessedSignal {
    samples: Vec<f32>,
    max_process_budget_samples: usize,
    final_pending_output_frames: usize,
}

struct RealAudioSample {
    samples: Vec<f32>,
    sample_rate_hz: u32,
    channel_count: u32,
}

impl RealAudioSample {
    fn pcm_format(&self) -> Result<AudioTempoPcmFormat> {
        Ok(AudioTempoPcmFormat::new(
            AudioTempoSampleRateHz::new(self.sample_rate_hz)?,
            AudioTempoChannelCount::new(self.channel_count)?,
        ))
    }
}

fn decode_real_audio_sample(path: &Path, max_frames: usize) -> Result<RealAudioSample> {
    let source = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let media_source_stream = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("wav");

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        media_source_stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let track = format
        .default_track(TrackType::Audio)
        .context("real-audio sample has no default audio track")?;
    let codec_params = track
        .codec_params
        .as_ref()
        .context("real-audio sample is missing codec parameters")?
        .audio()
        .context("real-audio sample track has no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(codec_params, &AudioDecoderOptions::default())?;
    let track_id = track.id;

    let mut decoded_samples = Vec::new();
    let mut sample_rate_hz = None;
    let mut channel_count = None;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => {
                bail!("unexpected Symphonia reset while decoding")
            }
            Err(error) => return Err(error).context("failed to read real-audio packet"),
        };

        if packet.track_id != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(error) => return Err(error).context("failed to decode real-audio packet"),
        };

        let spec = decoded.spec();
        let decoded_channel_count = u32::try_from(spec.channels().count())
            .context("decoded channel count should fit into u32")?;
        sample_rate_hz.get_or_insert(spec.rate());
        channel_count.get_or_insert(decoded_channel_count);

        let mut packet_samples = Vec::with_capacity(decoded.samples_interleaved());
        decoded.copy_to_vec_interleaved(&mut packet_samples);
        decoded_samples.extend_from_slice(&packet_samples);

        if decoded_samples.len() >= max_frames.saturating_mul(decoded_channel_count as usize) {
            break;
        }
    }

    let sample_rate_hz = sample_rate_hz.context("real-audio sample produced no decoded audio")?;
    let channel_count = channel_count.context("real-audio sample produced no channel count")?;
    let requested_samples = max_frames.saturating_mul(channel_count as usize);
    decoded_samples.truncate(requested_samples);

    Ok(RealAudioSample {
        samples: decoded_samples,
        sample_rate_hz,
        channel_count,
    })
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("audio-timestretch crate should live under crates/")
        .to_path_buf()
}
