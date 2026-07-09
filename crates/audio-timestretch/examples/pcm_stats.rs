//! Диагностика записанного PCM (WAV): клики, silence-пропуски, стерео-статистика.
//!
//! Запуск: `cargo run -p audio-timestretch --example pcm_stats --release -- <file.wav> [click_threshold]`

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

const BALANCE_WINDOW_FRAMES: usize = 2048;
const SILENCE_ABS_THRESHOLD: f32 = 1.0e-4;
const MIN_GAP_FRAMES: usize = 128;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .context("usage: pcm_stats <file.wav> [click_threshold]")?;
    let click_threshold: f32 = args
        .get(2)
        .map(|raw| raw.parse())
        .transpose()?
        .unwrap_or(0.30);

    let sample = decode(Path::new(path))?;
    let channels = sample.channels as usize;
    let frames = sample.samples.len() / channels;
    let secs = frames as f64 / sample.sample_rate as f64;
    println!(
        "file: {path} — {frames} frames @ {} Hz, {} ch, {secs:.2} s",
        sample.sample_rate, sample.channels
    );

    let peak = sample.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let max_delta = max_adjacent_delta(&sample.samples, channels);
    let p999 = delta_percentile(&sample.samples, channels, 0.999);
    println!("peak={peak:.3} maxΔ={max_delta:.3} p999Δ={p999:.4}");

    // Клики: адрес каждого перепада выше порога (сек).
    let clicks = click_positions(
        &sample.samples,
        channels,
        click_threshold,
        sample.sample_rate,
    );
    println!("clicks (Δ>{click_threshold}): {}", clicks.len());
    for (ts, delta) in clicks.iter().take(40) {
        println!("  click @ {ts:.3}s Δ={delta:.3}");
    }

    // Пропуски: окна тишины внутри громкого сигнала.
    let gaps = silence_gaps(&sample.samples, channels, sample.sample_rate);
    println!("silence gaps (>={MIN_GAP_FRAMES} frames): {}", gaps.len());
    for (start, len) in gaps.iter().take(40) {
        println!("  gap @ {start:.3}s len={len:.1}ms");
    }

    // Стерео-статистика по окнам.
    let (imb_max, imb_avg) = stereo_imbalance(&sample.samples, channels, BALANCE_WINDOW_FRAMES);
    let (corr_min, corr_avg) = stereo_correlation(&sample.samples, channels, BALANCE_WINDOW_FRAMES);
    println!("imb(max/avg)={imb_max:.3}/{imb_avg:.3} corr(min/avg)={corr_min:.3}/{corr_avg:.3}");

    // Динамика баланса по секундам: видно "гуляние" между L и R.
    if channels == 2 {
        println!("per-second L/R RMS:");
        let frames_per_sec = sample.sample_rate as usize;
        for (second, chunk) in sample.samples.chunks(frames_per_sec * channels).enumerate() {
            let (mut l, mut r) = (0.0f64, 0.0f64);
            let mut count = 0usize;
            for f in chunk.chunks_exact(2) {
                l += f64::from(f[0]) * f64::from(f[0]);
                r += f64::from(f[1]) * f64::from(f[1]);
                count += 1;
            }
            if count == 0 {
                continue;
            }
            let l_rms = (l / count as f64).sqrt();
            let r_rms = (r / count as f64).sqrt();
            println!(
                "  {second:>3}s L={l_rms:.4} R={r_rms:.4} L/R={:.2}",
                l_rms / r_rms.max(1e-9)
            );
        }
    }

    Ok(())
}

fn click_positions(
    samples: &[f32],
    channels: usize,
    threshold: f32,
    sample_rate: u32,
) -> Vec<(f64, f32)> {
    let mut clicks = Vec::new();
    for (frame_index, w) in samples.windows(channels * 2).step_by(channels).enumerate() {
        for c in 0..channels {
            let delta = (w[channels + c] - w[c]).abs();
            if delta > threshold {
                clicks.push((frame_index as f64 / sample_rate as f64, delta));
            }
        }
    }
    clicks
}

fn silence_gaps(samples: &[f32], channels: usize, sample_rate: u32) -> Vec<(f64, f64)> {
    let mut gaps = Vec::new();
    let mut gap_start: Option<usize> = None;
    let frame_count = samples.len() / channels;
    for frame_index in 0..frame_count {
        let frame = &samples[frame_index * channels..(frame_index + 1) * channels];
        let silent = frame.iter().all(|s| s.abs() < SILENCE_ABS_THRESHOLD);
        match (silent, gap_start) {
            (true, None) => gap_start = Some(frame_index),
            (false, Some(start)) => {
                let len = frame_index - start;
                if len >= MIN_GAP_FRAMES && start > 0 {
                    gaps.push((
                        start as f64 / sample_rate as f64,
                        len as f64 * 1000.0 / sample_rate as f64,
                    ));
                }
                gap_start = None;
            }
            _ => {}
        }
    }
    gaps
}

fn max_adjacent_delta(samples: &[f32], channels: usize) -> f32 {
    per_channel_deltas(samples, channels).fold(0.0f32, f32::max)
}

fn delta_percentile(samples: &[f32], channels: usize, q: f64) -> f32 {
    let mut deltas: Vec<f32> = per_channel_deltas(samples, channels).collect();
    if deltas.is_empty() {
        return 0.0;
    }
    deltas.sort_by(|a, b| a.total_cmp(b));
    let idx = ((deltas.len() - 1) as f64 * q).round() as usize;
    deltas[idx]
}

fn per_channel_deltas<'a>(samples: &'a [f32], channels: usize) -> impl Iterator<Item = f32> + 'a {
    samples
        .windows(channels * 2)
        .step_by(channels)
        .flat_map(move |w| (0..channels).map(move |c| (w[channels + c] - w[c]).abs()))
}

fn stereo_imbalance(samples: &[f32], channels: usize, window: usize) -> (f32, f32) {
    if channels != 2 {
        return (0.0, 0.0);
    }
    let mut max = 0.0f32;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for w in samples.chunks_exact(channels * window) {
        let (mut l, mut r) = (0.0f64, 0.0f64);
        for f in w.chunks_exact(2) {
            l += f64::from(f[0]) * f64::from(f[0]);
            r += f64::from(f[1]) * f64::from(f[1]);
        }
        let total = l + r;
        let imb = if total <= f64::EPSILON {
            0.0
        } else {
            ((l - r).abs() / total) as f32
        };
        max = max.max(imb);
        sum += f64::from(imb);
        count += 1;
    }
    (
        max,
        if count == 0 {
            0.0
        } else {
            (sum / count as f64) as f32
        },
    )
}

fn stereo_correlation(samples: &[f32], channels: usize, window: usize) -> (f32, f32) {
    if channels != 2 {
        return (1.0, 1.0);
    }
    let mut min = 1.0f32;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for w in samples.chunks_exact(channels * window) {
        let (mut ll, mut rr, mut lr) = (0.0f64, 0.0f64, 0.0f64);
        for f in w.chunks_exact(2) {
            let (l, r) = (f64::from(f[0]), f64::from(f[1]));
            ll += l * l;
            rr += r * r;
            lr += l * r;
        }
        if ll <= f64::EPSILON || rr <= f64::EPSILON {
            continue;
        }
        let corr = (lr / (ll.sqrt() * rr.sqrt())) as f32;
        min = min.min(corr);
        sum += f64::from(corr);
        count += 1;
    }
    (
        min,
        if count == 0 {
            1.0
        } else {
            (sum / count as f64) as f32
        },
    )
}

struct Sample {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u32,
}

fn decode(path: &Path) -> Result<Sample> {
    let source = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let track = format
        .default_track(TrackType::Audio)
        .context("no audio track")?;
    let codec_params = track
        .codec_params
        .as_ref()
        .context("no codec params")?
        .audio()
        .context("no audio codec params")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(codec_params, &AudioDecoderOptions::default())?;
    let track_id = track.id;

    let mut samples = Vec::new();
    let mut sample_rate = None;
    let mut channels = None;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => bail!("unexpected reset"),
            Err(e) => return Err(e).context("read packet"),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(e).context("decode"),
        };
        let spec = decoded.spec();
        sample_rate.get_or_insert(spec.rate());
        channels.get_or_insert(spec.channels().count() as u32);
        let mut packet_samples = Vec::with_capacity(decoded.samples_interleaved());
        decoded.copy_to_vec_interleaved(&mut packet_samples);
        samples.extend_from_slice(&packet_samples);
    }

    Ok(Sample {
        samples,
        sample_rate: sample_rate.context("no rate")?,
        channels: channels.context("no channels")?,
    })
}
