//! Сравнивает quality-профили adapter-а на реальной музыке по кликам,
//! silence-пропускам и CPU в рантайм-подобной последовательности
//! (пакеты 960 frames, прайминг, работа через adapter).
//!
//! Запуск: `cargo run -p audio-timestretch --example quality_click_probe --release`

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use audio_core::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoPcmFormat, AudioTempoProcessor,
    AudioTempoProcessorConfig, AudioTempoRatio, AudioTempoSampleRateHz, AudioTempoSegment,
    AudioTempoSegmentId,
};
use audio_timestretch::{
    TimestretchQualityMode, TimestretchTempoProcessor, TimestretchTempoSettings,
};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

const PACKET_FRAMES: usize = 960;
const SILENCE_ABS_THRESHOLD: f32 = 1.0e-4;
const MIN_GAP_FRAMES: usize = 96;
const CLICK_DELTA_FACTOR: f32 = 1.5;

struct Sample {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u32,
}

fn main() -> Result<()> {
    let source_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: cargo run -p audio-timestretch --example quality_click_probe --release -- <audio-file>")?;
    let sample = decode(&source_path)?;
    let input_secs =
        (sample.samples.len() / sample.channels as usize) as f64 / sample.sample_rate as f64;
    let input_max_delta = max_adjacent_delta(&sample.samples, sample.channels as usize);
    println!(
        "input: {input_secs:.2} s @ {} Hz, {} ch, maxΔ={input_max_delta:.3}",
        sample.sample_rate, sample.channels
    );
    let click_threshold = (input_max_delta * CLICK_DELTA_FACTOR).max(0.05);

    for quality in [
        TimestretchQualityMode::Balanced,
        TimestretchQualityMode::MaxQuality,
        TimestretchQualityMode::LowLatency,
    ] {
        println!("\n=== {quality:?} ===");
        for playback_rate in [0.5f64, 1.25, 1.5, 2.0, 4.0] {
            run_rate(&sample, quality, playback_rate, click_threshold, input_secs)?;
        }
    }
    Ok(())
}

fn run_rate(
    sample: &Sample,
    quality: TimestretchQualityMode,
    playback_rate: f64,
    click_threshold: f32,
    input_secs: f64,
) -> Result<()> {
    let pcm_format = AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(sample.sample_rate)?,
        AudioTempoChannelCount::new(sample.channels)?,
    );
    let mut processor = TimestretchTempoProcessor::with_settings(
        AudioTempoProcessorConfig::new(
            pcm_format,
            AudioTempoSegment::new(
                AudioTempoSegmentId::new(1),
                AudioTempoRatio::new(playback_rate)?,
            ),
        ),
        TimestretchTempoSettings::with_quality_mode(quality),
    )?;

    let channels = sample.channels as usize;
    let chunk_len = PACKET_FRAMES * channels;

    // Прайминг как в рантайме: 200 ms истории, warmup output отбрасывается.
    let history_len = (sample.sample_rate as usize / 5) * channels;
    let history = &sample.samples[..history_len.min(sample.samples.len())];
    let mut tempo_output = Vec::new();
    let _warmup = AudioTempoProcessor::process_decoded_media_into(
        &mut processor,
        AudioTempoDecodedMedia::from_interleaved_samples(history, pcm_format)?,
        &mut tempo_output,
    )?;

    let mut output: Vec<f32> = Vec::new();
    let started = Instant::now();
    for chunk in sample.samples[history.len()..].chunks(chunk_len) {
        let out = AudioTempoProcessor::process_decoded_media_into(
            &mut processor,
            AudioTempoDecodedMedia::from_interleaved_samples(chunk, pcm_format)?,
            &mut tempo_output,
        )?;
        output.extend_from_slice(out.interleaved_samples());
    }
    let elapsed = started.elapsed().as_secs_f64();

    let frames = output.len() / channels;
    let (gaps, gap_ms, longest_ms) = silence_gaps(&output, channels, sample.sample_rate);
    let clicks = count_deltas_above(&output, channels, click_threshold);
    let peak = output.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let throughput = input_secs / elapsed.max(1e-9);
    println!(
        "  {playback_rate:>4}x: frames={frames} clicks={clicks} gaps={gaps} ({gap_ms:.1} ms, max {longest_ms:.1} ms) peak={peak:.3} throughput={throughput:.1}x rt"
    );
    Ok(())
}

fn silence_gaps(samples: &[f32], channels: usize, sample_rate: u32) -> (usize, f64, f64) {
    let frames = samples.len() / channels;
    let mut gaps = 0usize;
    let mut total = 0usize;
    let mut longest = 0usize;
    let mut current = 0usize;
    for f in 0..frames {
        let silent = (0..channels).all(|c| samples[f * channels + c].abs() < SILENCE_ABS_THRESHOLD);
        if silent {
            current += 1;
        } else {
            if current >= MIN_GAP_FRAMES {
                gaps += 1;
                total += current;
                longest = longest.max(current);
            }
            current = 0;
        }
    }
    if current >= MIN_GAP_FRAMES {
        gaps += 1;
        total += current;
        longest = longest.max(current);
    }
    let to_ms = |frames: usize| frames as f64 * 1000.0 / f64::from(sample_rate);
    (gaps, to_ms(total), to_ms(longest))
}

fn max_adjacent_delta(samples: &[f32], channels: usize) -> f32 {
    per_channel_deltas(samples, channels).fold(0.0f32, f32::max)
}

fn count_deltas_above(samples: &[f32], channels: usize, threshold: f32) -> usize {
    per_channel_deltas(samples, channels)
        .filter(|d| *d > threshold)
        .count()
}

fn per_channel_deltas<'a>(samples: &'a [f32], channels: usize) -> impl Iterator<Item = f32> + 'a {
    samples
        .windows(channels * 2)
        .step_by(channels)
        .flat_map(move |w| (0..channels).map(move |c| (w[channels + c] - w[c]).abs()))
}

fn decode(path: &Path) -> Result<Sample> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            Hint::new().with_extension("wav"),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("probe")?;

    let track = format
        .default_track(TrackType::Audio)
        .context("audio track")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(
            track
                .codec_params
                .as_ref()
                .and_then(|p| p.audio())
                .context("audio params")?,
            &AudioDecoderOptions::default(),
        )
        .context("decoder")?;

    let mut samples = Vec::new();
    let mut sample_rate = 0u32;
    let mut channels = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => bail!("reset required"),
            Err(error) => return Err(error.into()),
        };
        if packet.track_id != track.id {
            continue;
        }
        let decoded = decoder.decode(&packet).context("decode")?;
        let spec = decoded.spec();
        sample_rate = spec.rate();
        channels = spec.channels().count() as u32;
        let mut interleaved = vec![0.0f32; decoded.samples_interleaved()];
        decoded.copy_to_slice_interleaved(&mut interleaved);
        samples.extend_from_slice(&interleaved);
    }

    if samples.is_empty() {
        bail!("нет декодированных samples");
    }
    Ok(Sample {
        samples,
        sample_rate,
        channels,
    })
}
