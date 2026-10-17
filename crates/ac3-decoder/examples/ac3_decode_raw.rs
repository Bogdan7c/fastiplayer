//! Dev-инструмент: декодирует сырой поток `.ac3`/`.eac3` в f32 для сверки с
//! ffmpeg (`tools/ffmpeg_sweep.py`, `tools/eac3_sweep.py`). Не часть продукта
//! и не запускается в CI.
//!
//! Запуск:
//! `cargo run --release -p ac3-decoder --example ac3_decode_raw -- <in> <out.f32> [опции]`
//!
//! Поток режется на кадры по заголовкам и склеивается в access unit-ы: кадр
//! AC-3 или независимый substream 0 E-AC-3 начинает новый, всё остальное
//! (зависимые, другие программы) присоединяется к текущему (Annex E §3.8).
//! Выход — interleaved f32le в порядке каналов декодера; рядом
//! `<out.f32>.frames` — по строке на access unit: `ok <частота> <отсчётов>
//! <позиции через ,>` или `error <описание>`. Вместо отвергнутого access
//! unit-а пишется тишина длины последнего удачного в его раскладке (как
//! политика адаптера), чтобы номера сэмплов совпадали с ffmpeg.
//!
//! Опции: `--dither off`, `--crc skip`, `--drc off|heavy|scaled:<cut%>:<boost%>`,
//! `--tpnp off`, `--bench` (только время декодирования, без записи выхода),
//! `--inspect` (по строке на substream: тип, `compr`, параметры TPNP —
//! вместо декодирования).

use std::time::Instant;

use ac3_decoder::{
    Ac3Decoder, CompressionScale, CrcPolicy, DecodedAudio, DecoderConfig, DitherMode,
    DynamicRangeControl, StreamType, SubstreamHeader, TransientPreNoiseProcessing,
    inspect_access_unit, read_enhanced_syncframe_info, read_syncframe_info,
};

/// Синхрослово AC-3 (A/52 §5.4.1.1).
const SYNC_WORD: [u8; 2] = [0x0B, 0x77];
/// Байт с `bsid` (старшие 5 бит) и последний `bsid` синтаксиса AC-3.
const BSID_BYTE: usize = 5;
const LAST_AC3_BSID: u8 = 10;

struct Options {
    input: String,
    output: String,
    config: DecoderConfig,
    bench: bool,
    inspect: bool,
}

fn parse_scaled(value: &str) -> Result<DynamicRangeControl, String> {
    let mut parts = value.split(':').skip(1);
    let mut next_scale = |name: &str| {
        parts
            .next()
            .and_then(|text| text.parse::<u8>().ok())
            .and_then(CompressionScale::from_percent)
            .ok_or_else(|| format!("bad {name} percent in {value}"))
    };
    let cut = next_scale("cut")?;
    let boost = next_scale("boost")?;
    Ok(DynamicRangeControl::Scaled { cut, boost })
}

fn parse_options() -> Result<Options, String> {
    let mut arguments = std::env::args().skip(1);
    let input = arguments.next().ok_or("missing input path")?;
    let output = arguments.next().ok_or("missing output path")?;
    let mut config = DecoderConfig::default();
    let mut bench = false;
    let mut inspect = false;
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--bench" => bench = true,
            "--inspect" => inspect = true,
            "--dither" | "--crc" | "--drc" | "--tpnp" => {
                let value = arguments.next().ok_or(format!("{flag} needs a value"))?;
                match (flag.as_str(), value.as_str()) {
                    ("--dither", "off") => config.dither = DitherMode::Disabled,
                    ("--crc", "skip") => config.crc = CrcPolicy::Skip,
                    ("--tpnp", "off") => {
                        config.transient_pre_noise = TransientPreNoiseProcessing::Skip;
                    }
                    ("--drc", "off") => config.dynamic_range = DynamicRangeControl::Disabled,
                    ("--drc", "heavy") => config.dynamic_range = DynamicRangeControl::Heavy,
                    ("--drc", scaled) if scaled.starts_with("scaled:") => {
                        config.dynamic_range = parse_scaled(scaled)?;
                    }
                    _ => return Err(format!("unknown value {value} for {flag}")),
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Options {
        input,
        output,
        config,
        bench,
        inspect,
    })
}

/// Кадр, найденный в потоке: байты и начинает ли он access unit.
struct Frame<'a> {
    bytes: &'a [u8],
    starts_access_unit: bool,
}

/// Длина кадра и признак начала access unit-а по его заголовку.
fn probe_frame(rest: &[u8]) -> Option<(usize, bool)> {
    let bitstream_id = rest.get(BSID_BYTE)? >> 3;
    if bitstream_id <= LAST_AC3_BSID {
        let info = read_syncframe_info(rest).ok()?;
        Some((info.frame_length_bytes, true))
    } else {
        let info = read_enhanced_syncframe_info(rest).ok()?;
        let starts = info.stream_type != StreamType::Dependent && info.substream_id == 0;
        Some((info.frame_length_bytes, starts))
    }
}

/// Нарезка потока на кадры по заголовкам; мусор между кадрами пропускается
/// поиском синхрослова.
fn split_frames(stream: &[u8]) -> Vec<Frame<'_>> {
    let mut frames = Vec::new();
    let mut position = 0;
    while position + 2 <= stream.len() {
        let rest = &stream[position..];
        if rest[..2] != SYNC_WORD {
            position += 1;
            continue;
        }
        match probe_frame(rest) {
            Some((length, starts_access_unit)) if length > 0 && length <= rest.len() => {
                frames.push(Frame {
                    bytes: &rest[..length],
                    starts_access_unit,
                });
                position += length;
            }
            _ => position += 1,
        }
    }
    frames
}

/// Склеивает кадры в access unit-ы.
fn split_access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = Vec::new();
    for frame in split_frames(stream) {
        match units.last_mut() {
            Some(unit) if !frame.starts_access_unit => unit.extend_from_slice(frame.bytes),
            _ => units.push(frame.bytes.to_vec()),
        }
    }
    units
}

fn append_interleaved(audio: &DecodedAudio, pcm: &mut Vec<u8>) {
    let channels: Vec<&[f32]> = (0..audio.channels().len())
        .filter_map(|channel| audio.channel_samples(channel))
        .collect();
    for index in 0..audio.samples_per_channel() {
        for channel in &channels {
            pcm.extend_from_slice(&channel[index].to_le_bytes());
        }
    }
}

/// Описание substream-ов каждого access unit-а (dev-диагностика).
fn inspect_units(units: &[Vec<u8>], config: DecoderConfig) -> String {
    let mut report = String::new();
    for (index, unit) in units.iter().enumerate() {
        match inspect_access_unit(unit, config) {
            Ok(inspection) => {
                for substream in &inspection.substreams {
                    let line = match &substream.header {
                        SubstreamHeader::Ac3(info) => {
                            format!("ac3 compr {:?}", info.bsi.program.compression_gain_word)
                        }
                        SubstreamHeader::Enhanced(info) => format!(
                            "{:?} id {} blocks {} compr {:?} tpnp {:?}",
                            info.stream_type,
                            info.substream_id,
                            info.audio_blocks,
                            info.bsi.program.compression_gain_word,
                            info.audio_frame.transient_pre_noise
                        ),
                    };
                    report.push_str(&format!("{index} {line}\n"));
                }
            }
            Err(error) => report.push_str(&format!("{index} error {error}\n")),
        }
    }
    report
}

fn run() -> Result<(), String> {
    let options = parse_options()?;
    let stream = std::fs::read(&options.input).map_err(|error| error.to_string())?;
    let frames = split_access_units(&stream);
    let mut decoder = Ac3Decoder::new(options.config);
    if options.inspect {
        print!("{}", inspect_units(&frames, options.config));
        return Ok(());
    }
    if options.bench {
        let started = Instant::now();
        let mut decoded_samples = 0usize;
        let mut sample_rate = 0;
        for frame in &frames {
            if let Ok(audio) = decoder.decode_access_unit(frame) {
                decoded_samples += audio.samples_per_channel();
                sample_rate = audio.sample_rate_hz();
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        let audio_seconds = decoded_samples as f64 / f64::from(sample_rate.max(1));
        println!(
            "frames {} audio {audio_seconds:.2} s decode {elapsed:.4} s realtime x{:.1}",
            frames.len(),
            audio_seconds / elapsed
        );
        return Ok(());
    }
    let mut pcm = Vec::new();
    let mut report = String::new();
    let mut last_channel_count = 0;
    let mut last_samples_per_channel = 0;
    for frame in &frames {
        match decoder.decode_access_unit(frame) {
            Ok(audio) => {
                let positions: Vec<String> = audio
                    .channels()
                    .iter()
                    .map(|position| format!("{position:?}"))
                    .collect();
                report.push_str(&format!(
                    "ok {} {} {}\n",
                    audio.sample_rate_hz(),
                    audio.samples_per_channel(),
                    positions.join(",")
                ));
                last_channel_count = audio.channels().len();
                last_samples_per_channel = audio.samples_per_channel();
                append_interleaved(&audio, &mut pcm);
            }
            Err(error) => {
                report.push_str(&format!("error {error}\n"));
                let bytes = last_samples_per_channel * last_channel_count * size_of::<f32>();
                pcm.resize(pcm.len() + bytes, 0);
            }
        }
    }
    std::fs::write(&options.output, &pcm).map_err(|error| error.to_string())?;
    std::fs::write(format!("{}.frames", options.output), report)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("ac3_decode_raw: {message}");
        std::process::exit(2);
    }
}
