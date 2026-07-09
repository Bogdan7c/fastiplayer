//! Воспроизводит РАНТАЙМ-последовательность tempo-пути и ищет в выходе
//! silence-пропуски и клики: прайминг историей, ramp из 10 set_segment
//! (колесо 1.0x -> 2.0x по 0.1), переменные Opus-пакеты 48 kHz.
//!
//! Запуск: `cargo run -p audio-timestretch --example runtime_sequence_probe --release`

use std::f32::consts::TAU;

use anyhow::Result;
use audio_core::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoPcmFormat, AudioTempoProcessor,
    AudioTempoRatio, AudioTempoSampleRateHz, AudioTempoSegment, AudioTempoSegmentId,
};
use audio_timestretch::{
    TimestretchQualityMode, TimestretchTempoProcessor, TimestretchTempoSettings,
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
const PACKET_FRAMES: usize = 960; // Opus 20 ms
const SILENCE_ABS_THRESHOLD: f32 = 1.0e-4;
const MIN_GAP_FRAMES: usize = 96; // 2 ms

fn main() -> Result<()> {
    for quality in [
        TimestretchQualityMode::Balanced,
        TimestretchQualityMode::LowLatency,
    ] {
        run_scenario(quality)?;
    }
    Ok(())
}

fn run_scenario(quality: TimestretchQualityMode) -> Result<()> {
    println!("\n=== scenario: {quality:?}, 48kHz stereo, packets {PACKET_FRAMES} frames ===");
    let pcm_format = AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(SAMPLE_RATE)?,
        AudioTempoChannelCount::new(CHANNELS)?,
    );

    // Непрерывный музыкоподобный сигнал: сумма синусов, без пауз в исходнике.
    let mut phase = 0usize;
    let mut next_packet = |frames: usize| -> Vec<f32> {
        let mut packet = Vec::with_capacity(frames * CHANNELS as usize);
        for i in 0..frames {
            let t = (phase + i) as f32 / SAMPLE_RATE as f32;
            let sample = 0.30 * (TAU * 220.0 * t).sin()
                + 0.20 * (TAU * 330.0 * t).sin()
                + 0.10 * (TAU * 587.0 * t).sin();
            packet.push(sample);
            packet.push(sample * 0.9);
        }
        phase += frames;
        packet
    };

    // 1) Прайминг: 200 ms истории passthrough (как player-core перед первым non-1x).
    let mut segment_id = 1u64;
    let first_rate = 1.1f64;
    let mut processor = TimestretchTempoProcessor::with_settings(
        audio_core::AudioTempoProcessorConfig::new(
            pcm_format,
            AudioTempoSegment::new(
                AudioTempoSegmentId::new(segment_id),
                AudioTempoRatio::new(first_rate)?,
            ),
        ),
        TimestretchTempoSettings::with_quality_mode(quality),
    )?;

    let history = next_packet(SAMPLE_RATE as usize / 5); // 200 ms
    let mut tempo_output = Vec::new();
    let warmup = AudioTempoProcessor::process_decoded_media_into(
        &mut processor,
        AudioTempoDecodedMedia::from_interleaved_samples(&history, pcm_format)?,
        &mut tempo_output,
    )?;
    println!(
        "warmup: primed {} frames -> discarded {} samples",
        history.len() / CHANNELS as usize,
        warmup.interleaved_samples().len()
    );

    let mut output: Vec<f32> = Vec::new();

    // 2) Ramp колеса: 1.1 -> 2.0 по 0.1, между ступенями по 2 пакета (как UI по ~40 ms).
    let mut rate = first_rate;
    while rate < 1.999 {
        for _ in 0..2 {
            let packet = next_packet(PACKET_FRAMES);
            let out = AudioTempoProcessor::process_decoded_media_into(
                &mut processor,
                AudioTempoDecodedMedia::from_interleaved_samples(&packet, pcm_format)?,
                &mut tempo_output,
            )?;
            output.extend_from_slice(out.interleaved_samples());
        }
        rate += 0.1;
        segment_id += 1;
        processor.set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(segment_id),
            AudioTempoRatio::new(rate)?,
        ))?;
    }

    // 3) Steady 2x: 15 секунд media переменными пакетами (960/1024/1536).
    let steady_started = output.len();
    let mut media_frames = 0usize;
    let sizes = [960usize, 1024, 1536];
    let mut size_index = 0usize;
    while media_frames < SAMPLE_RATE as usize * 15 {
        let frames = sizes[size_index % sizes.len()];
        size_index += 1;
        let packet = next_packet(frames);
        media_frames += frames;
        let out = AudioTempoProcessor::process_decoded_media_into(
            &mut processor,
            AudioTempoDecodedMedia::from_interleaved_samples(&packet, pcm_format)?,
            &mut tempo_output,
        )?;
        output.extend_from_slice(out.interleaved_samples());
    }

    println!(
        "output: total {} frames (steady 2x part: {} frames, expected ~{})",
        output.len() / CHANNELS as usize,
        (output.len() - steady_started) / CHANNELS as usize,
        SAMPLE_RATE as usize * 15 / 2
    );

    report_gaps_and_clicks("full", &output);
    report_gaps_and_clicks("steady-2x", &output[steady_started..]);
    Ok(())
}

/// Ищет silence-пропуски и клики (резкие дельты) в interleaved выходе.
fn report_gaps_and_clicks(label: &str, samples: &[f32]) {
    let channels = CHANNELS as usize;
    let frames = samples.len() / channels;

    // Silence gaps: подряд идущие кадры, где оба канала почти нулевые.
    let mut gaps = 0usize;
    let mut gap_frames_total = 0usize;
    let mut longest_gap = 0usize;
    let mut current_gap = 0usize;
    for f in 0..frames {
        let silent = (0..channels).all(|c| samples[f * channels + c].abs() < SILENCE_ABS_THRESHOLD);
        if silent {
            current_gap += 1;
        } else {
            if current_gap >= MIN_GAP_FRAMES {
                gaps += 1;
                gap_frames_total += current_gap;
                longest_gap = longest_gap.max(current_gap);
            }
            current_gap = 0;
        }
    }
    if current_gap >= MIN_GAP_FRAMES {
        gaps += 1;
        gap_frames_total += current_gap;
        longest_gap = longest_gap.max(current_gap);
    }

    // Клики: адрес каждой дельты выше порога.
    let mut clicks = 0usize;
    let mut max_delta = 0.0f32;
    for f in 1..frames {
        for c in 0..channels {
            let delta = (samples[f * channels + c] - samples[(f - 1) * channels + c]).abs();
            max_delta = max_delta.max(delta);
            if delta > 0.35 {
                clicks += 1;
            }
        }
    }

    println!(
        "{label:>10}: gaps(>=2ms)={gaps} (total {:.1} ms, longest {:.1} ms) clicks={clicks} maxΔ={max_delta:.3}",
        gap_frames_total as f64 * 1000.0 / f64::from(SAMPLE_RATE),
        longest_gap as f64 * 1000.0 / f64::from(SAMPLE_RATE),
    );
}
