//! Замер пропускной способности timestretch-профилей: во сколько раз быстрее
//! realtime обрабатывается PCM. Для playback rate `R` throughput должен быть
//! заметно больше `R`, иначе session thread не успевает кормить output.
//!
//! Запуск: `cargo run -p audio-timestretch --example tempo_throughput --release`

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

const CHUNK_FRAMES: usize = 1024;

struct Sample {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u32,
}

fn main() -> Result<()> {
    let source_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: cargo run -p audio-timestretch --example tempo_throughput --release -- <audio-file>")?;
    let sample = decode(&source_path)?;
    let input_secs =
        (sample.samples.len() / sample.channels as usize) as f64 / sample.sample_rate as f64;
    println!(
        "input: {input_secs:.2} s @ {} Hz, {} ch",
        sample.sample_rate, sample.channels
    );

    let profiles = [
        (
            "lowlat-1024-256",
            QualityMode::LowLatency,
            Some((1024, 256)),
        ),
        ("lowlat-lib-default", QualityMode::LowLatency, None),
        ("balanced-lib-default", QualityMode::Balanced, None),
    ];

    for playback_rate in [1.5f64, 2.0, 4.0] {
        let backend_ratio = 1.0 / playback_rate;
        println!("\n=== playback {playback_rate}x ===");
        for (name, quality, fft_hop) in &profiles {
            let mut params = StretchParams::new(backend_ratio)
                .with_sample_rate(sample.sample_rate)
                .with_channels(sample.channels)
                .with_quality_mode(*quality);
            if let Some((fft, hop)) = fft_hop {
                params = params.with_fft_size(*fft).with_hop_size(*hop);
            }

            let mut processor = StreamProcessor::new(params);
            let chunk_len = CHUNK_FRAMES * sample.channels as usize;
            let started = Instant::now();
            let mut output_samples = 0usize;
            for chunk in sample.samples.chunks(chunk_len) {
                let produced = processor.process(chunk)?;
                output_samples += produced.len();
            }
            let elapsed = started.elapsed().as_secs_f64();
            let throughput = input_secs / elapsed;
            let feed_ok = throughput > playback_rate * 2.0;
            println!(
                "{name:>22}: {elapsed:.3} s wall, throughput {throughput:.1}x realtime, \
                 need >{playback_rate}x -> {}",
                if feed_ok {
                    "OK"
                } else {
                    "НЕ УСПЕВАЕТ"
                }
            );
            let _ = output_samples;
        }
    }

    Ok(())
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
