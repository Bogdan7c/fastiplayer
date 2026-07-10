//! Диагностический пробник качества timestretch-профилей на реальном audio.
//!
//! Запуск: `cargo run -p audio-timestretch --example profile_probe --release`
//! Пишет raw f32 interleaved dumps в /tmp/ts-probe/ для прослушивания через ffmpeg.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use timestretch::{QualityMode, StereoMode, StreamProcessor, StretchParams};

const CHUNK_FRAMES: usize = 512;
const BALANCE_WINDOW_FRAMES: usize = 2048;

struct Sample {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u32,
}

struct ProfileSpec {
    name: &'static str,
    quality: QualityMode,
    fft: Option<usize>,
    hop: Option<usize>,
    stereo: Option<StereoMode>,
}

fn main() -> Result<()> {
    let source_path = std::env::args().nth(1).map(PathBuf::from).context(
        "usage: cargo run -p audio-timestretch --example profile_probe --release -- <audio-file>",
    )?;
    let sample = decode(&source_path)?;
    println!(
        "input: {} frames @ {} Hz, {} ch",
        sample.samples.len() / sample.channels as usize,
        sample.sample_rate,
        sample.channels
    );
    print_stats("input", &sample.samples, sample.channels as usize, None);

    let input_max_delta = max_adjacent_delta(&sample.samples, sample.channels as usize);

    let profiles = [
        ProfileSpec {
            name: "current-2048-256-lowlat",
            quality: QualityMode::LowLatency,
            fft: Some(2048),
            hop: Some(256),
            stereo: Some(StereoMode::MidSide),
        },
        ProfileSpec {
            name: "old-1024-256-lowlat",
            quality: QualityMode::LowLatency,
            fft: Some(1024),
            hop: Some(256),
            stereo: None,
        },
        ProfileSpec {
            name: "cur-independent-stereo",
            quality: QualityMode::LowLatency,
            fft: Some(2048),
            hop: Some(256),
            stereo: Some(StereoMode::Independent),
        },
        ProfileSpec {
            name: "lowlat-lib-default-4096-512",
            quality: QualityMode::LowLatency,
            fft: None,
            hop: None,
            stereo: None,
        },
        ProfileSpec {
            name: "balanced-lib-default",
            quality: QualityMode::Balanced,
            fft: None,
            hop: None,
            stereo: None,
        },
    ];

    let out_dir = PathBuf::from("/tmp/ts-probe");
    std::fs::create_dir_all(&out_dir)?;

    for playback_rate in [0.5f64, 0.75, 1.25, 1.5, 2.0, 4.0] {
        let backend_ratio = 1.0 / playback_rate;
        println!("\n=== playback {playback_rate}x (backend ratio {backend_ratio:.4}) ===");
        for profile in &profiles {
            let mut params = StretchParams::new(backend_ratio)
                .with_sample_rate(sample.sample_rate)
                .with_channels(sample.channels)
                .with_quality_mode(profile.quality);
            if let Some(fft) = profile.fft {
                params = params.with_fft_size(fft);
            }
            if let Some(hop) = profile.hop {
                params = params.with_hop_size(hop);
            }
            if let Some(stereo) = profile.stereo {
                params = params.with_stereo_mode(stereo);
            }

            let mut processor = StreamProcessor::new(params);
            let mut output: Vec<f32> = Vec::new();
            let chunk_len = CHUNK_FRAMES * sample.channels as usize;
            for chunk in sample.samples.chunks(chunk_len) {
                let produced = processor.process(chunk)?;
                output.extend_from_slice(&produced);
            }
            let mut tail = Vec::with_capacity(1 << 20);
            processor.flush_into(&mut tail)?;
            output.extend_from_slice(&tail);

            print_stats(
                profile.name,
                &output,
                sample.channels as usize,
                Some(input_max_delta),
            );

            let dump = out_dir.join(format!("{}_{playback_rate}x.f32", profile.name));
            let mut file = File::create(&dump)?;
            let bytes: Vec<u8> = output.iter().flat_map(|s| s.to_le_bytes()).collect();
            file.write_all(&bytes)?;
        }
    }

    println!("\ndumps: /tmp/ts-probe/*.f32 (ffmpeg -f f32le -ar 44100 -ac 2 -i <file> out.wav)");
    Ok(())
}

fn print_stats(name: &str, samples: &[f32], channels: usize, input_max_delta: Option<f32>) {
    let max_delta = max_adjacent_delta(samples, channels);
    let p999 = delta_percentile(samples, channels, 0.999);
    let clicks = input_max_delta
        .map(|input_max| count_deltas_above(samples, channels, (input_max * 1.5).max(0.05)))
        .unwrap_or(0);
    let (imb_max, imb_avg) = stereo_imbalance(samples, channels, BALANCE_WINDOW_FRAMES);
    let (corr_min, corr_avg) = stereo_correlation(samples, channels, BALANCE_WINDOW_FRAMES);
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    println!(
        "{name:>28}: peak={peak:.3} maxΔ={max_delta:.3} p999Δ={p999:.4} clicks={clicks} \
         imb(max/avg)={imb_max:.3}/{imb_avg:.3} corr(min/avg)={corr_min:.3}/{corr_avg:.3}"
    );
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

fn decode(path: &Path) -> Result<Sample> {
    let source = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("wav");

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
