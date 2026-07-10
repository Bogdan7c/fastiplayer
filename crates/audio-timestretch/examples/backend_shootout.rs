//! Сравнение tempo-бэкендов на реальном треке: timestretch 0.5 (текущий),
//! Signalsmith Stretch, SoundTouch. Метрики + raw f32 дампы для прослушивания.
//!
//! Запуск: `cargo run -p audio-timestretch --example backend_shootout --release -- <file.wav>`
//! Дампы: /tmp/ts-shootout/*.f32 (ffmpeg -f f32le -ar 48000 -ac 2 -i <file> out.wav)

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

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
        .context("usage: cargo run -p audio-timestretch --example backend_shootout --release -- <audio-file>")?;
    let sample = decode(&source_path)?;
    let input_secs =
        (sample.samples.len() / sample.channels as usize) as f64 / sample.sample_rate as f64;
    let input_max_delta = max_adjacent_delta(&sample.samples, sample.channels as usize);
    println!(
        "input: {input_secs:.2} s @ {} Hz, {} ch, maxΔ={input_max_delta:.3}",
        sample.sample_rate, sample.channels
    );
    let click_threshold = (input_max_delta * CLICK_DELTA_FACTOR).max(0.05);
    let out_dir = PathBuf::from("/tmp/ts-shootout");
    std::fs::create_dir_all(&out_dir)?;

    for playback_rate in [1.5f64, 2.0, 3.0, 0.5] {
        println!("\n=== playback {playback_rate}x ===");
        run_timestretch(
            &sample,
            playback_rate,
            click_threshold,
            input_secs,
            &out_dir,
        )?;
        run_signalsmith(
            &sample,
            playback_rate,
            click_threshold,
            input_secs,
            &out_dir,
        )?;
        run_soundtouch(
            &sample,
            playback_rate,
            click_threshold,
            input_secs,
            &out_dir,
        )?;
    }

    println!("\nдампы: /tmp/ts-shootout/*.f32");
    println!(
        "слушать: ffplay -f f32le -ar {} -ch_layout stereo <file>",
        sample.sample_rate
    );
    Ok(())
}

fn run_timestretch(
    sample: &Sample,
    playback_rate: f64,
    click_threshold: f32,
    input_secs: f64,
    out_dir: &Path,
) -> Result<()> {
    use timestretch::{QualityMode, StreamProcessor, StretchParams};

    let params = StretchParams::new(1.0 / playback_rate)
        .with_sample_rate(sample.sample_rate)
        .with_channels(sample.channels)
        .with_quality_mode(QualityMode::Balanced)
        .with_fft_size(8192)
        .with_hop_size(4096);
    let mut processor = StreamProcessor::new(params);

    let chunk_len = PACKET_FRAMES * sample.channels as usize;
    let mut output: Vec<f32> = Vec::new();
    let started = Instant::now();
    for chunk in sample.samples.chunks(chunk_len) {
        let produced = processor.process(chunk)?;
        output.extend_from_slice(&produced);
    }
    let elapsed = started.elapsed().as_secs_f64();
    report(
        "timestretch-0.5",
        sample,
        playback_rate,
        &output,
        click_threshold,
        input_secs,
        elapsed,
        out_dir,
    )
}

fn run_signalsmith(
    sample: &Sample,
    playback_rate: f64,
    click_threshold: f32,
    input_secs: f64,
    out_dir: &Path,
) -> Result<()> {
    use signalsmith_stretch::Stretch;

    let channels = sample.channels as usize;
    let mut stretch = Stretch::preset_default(sample.channels, sample.sample_rate);

    let chunk_len = PACKET_FRAMES * channels;
    let mut output: Vec<f32> = Vec::new();
    let mut consumed_frames = 0usize;
    let mut produced_frames = 0usize;
    let started = Instant::now();
    for chunk in sample.samples.chunks(chunk_len) {
        consumed_frames += chunk.len() / channels;
        // Дробный остаток переносится: следующий пакет доберёт кадр.
        let target_total = (consumed_frames as f64 / playback_rate) as usize;
        let out_frames = target_total.saturating_sub(produced_frames);
        produced_frames += out_frames;
        let mut out_chunk = vec![0.0f32; out_frames * channels];
        stretch.process(chunk, &mut out_chunk);
        output.extend_from_slice(&out_chunk);
    }
    let elapsed = started.elapsed().as_secs_f64();
    report(
        "signalsmith",
        sample,
        playback_rate,
        &output,
        click_threshold,
        input_secs,
        elapsed,
        out_dir,
    )
}

fn run_soundtouch(
    sample: &Sample,
    playback_rate: f64,
    click_threshold: f32,
    input_secs: f64,
    out_dir: &Path,
) -> Result<()> {
    use soundtouch::SoundTouch;

    let channels = sample.channels as usize;
    let mut soundtouch = SoundTouch::new();
    soundtouch
        .set_channels(sample.channels)
        .set_sample_rate(sample.sample_rate)
        .set_tempo(playback_rate);

    let chunk_len = PACKET_FRAMES * channels;
    let mut output: Vec<f32> = Vec::new();
    let mut receive_buffer = vec![0.0f32; chunk_len * 8];
    let started = Instant::now();
    for chunk in sample.samples.chunks(chunk_len) {
        soundtouch.put_samples(chunk, chunk.len() / channels);
        let receive_capacity_frames = receive_buffer.len() / channels;
        loop {
            let received = soundtouch.receive_samples(&mut receive_buffer, receive_capacity_frames);
            if received == 0 {
                break;
            }
            output.extend_from_slice(&receive_buffer[..received * channels]);
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    report(
        "soundtouch",
        sample,
        playback_rate,
        &output,
        click_threshold,
        input_secs,
        elapsed,
        out_dir,
    )
}

#[allow(clippy::too_many_arguments)]
fn report(
    name: &str,
    sample: &Sample,
    playback_rate: f64,
    output: &[f32],
    click_threshold: f32,
    input_secs: f64,
    elapsed: f64,
    out_dir: &Path,
) -> Result<()> {
    let channels = sample.channels as usize;
    let frames = output.len() / channels;
    let expected_frames = (sample.samples.len() / channels) as f64 / playback_rate;
    let clicks = count_deltas_above(output, channels, click_threshold);
    let peak = output.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let throughput = input_secs / elapsed.max(1e-9);
    println!(
        "  {name:>16}: frames={frames} (ожид. ~{expected_frames:.0}) clicks={clicks} peak={peak:.3} throughput={throughput:.1}x rt"
    );

    let dump = out_dir.join(format!("{name}_{playback_rate}x.f32"));
    let mut file = File::create(&dump)?;
    let bytes: Vec<u8> = output.iter().flat_map(|s| s.to_le_bytes()).collect();
    file.write_all(&bytes)?;
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
