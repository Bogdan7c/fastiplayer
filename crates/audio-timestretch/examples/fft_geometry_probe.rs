//! Ищет FFT/hop геометрию с минимумом кликов на переданном громком треке.
//!
//! Запуск: `cargo run -p audio-timestretch --example fft_geometry_probe --release -- <file.wav>`

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use timestretch::{QualityMode, StreamProcessor, StretchParams};

const PACKET_FRAMES: usize = 960;
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
        .context("usage: cargo run -p audio-timestretch --example fft_geometry_probe --release -- <audio-file>")?;
    let sample = decode(&source_path)?;
    let input_secs =
        (sample.samples.len() / sample.channels as usize) as f64 / sample.sample_rate as f64;
    let input_max_delta = max_adjacent_delta(&sample.samples, sample.channels as usize);
    println!(
        "input: {input_secs:.2} s @ {} Hz, {} ch, maxΔ={input_max_delta:.3}",
        sample.sample_rate, sample.channels
    );
    let click_threshold = (input_max_delta * CLICK_DELTA_FACTOR).max(0.05);

    // (имя, fft, hop): None = дефолт либы.
    let geometries: [(&str, Option<(usize, usize)>); 5] = [
        ("balanced-default", None),
        ("balanced-8192-1024", Some((8192, 1024))),
        ("balanced-8192-4096", Some((8192, 4096))),
        ("balanced-16384-4096", Some((16384, 4096))),
        ("balanced-8192-2048", Some((8192, 2048))),
    ];

    for (name, fft_hop) in geometries {
        println!("\n=== {name} ===");
        for playback_rate in [0.5f64, 0.25, 2.0, 3.0, 4.0] {
            let backend_ratio = 1.0 / playback_rate;
            let mut params = StretchParams::new(backend_ratio)
                .with_sample_rate(sample.sample_rate)
                .with_channels(sample.channels)
                .with_quality_mode(QualityMode::Balanced);
            if let Some((fft, hop)) = fft_hop {
                params = params.with_fft_size(fft).with_hop_size(hop);
            }
            let mut processor = StreamProcessor::new(params);

            let chunk_len = PACKET_FRAMES * sample.channels as usize;
            let mut output: Vec<f32> = Vec::new();
            let started = Instant::now();
            for chunk in sample.samples.chunks(chunk_len) {
                let produced = processor.process(chunk)?;
                output.extend_from_slice(&produced);
            }
            let elapsed = started.elapsed().as_secs_f64();

            let clicks = count_deltas_above(&output, sample.channels as usize, click_threshold);
            let peak = output.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let throughput = input_secs / elapsed.max(1e-9);
            println!(
                "  {playback_rate:>4}x: clicks={clicks} peak={peak:.3} throughput={throughput:.1}x rt"
            );
        }
    }
    Ok(())
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
