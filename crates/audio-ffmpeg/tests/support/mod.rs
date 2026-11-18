//! Общие помощники тестов: чтение фикстур демуксером плеера, декод, уровень тона.

use std::path::{Path, PathBuf};

use audio_core::AudioOutputSpec;
use audio_ffmpeg::{AacStreamConfig, FfmpegAacDecoder};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Начало окна анализа: пропускаем разгон кодера и задержку SBR.
const ANALYSIS_START_SECONDS: f64 = 0.25;
/// Длина окна анализа.
const ANALYSIS_WINDOW_SECONDS: f64 = 1.0;
/// Длина заголовка ADTS без CRC.
const ADTS_HEADER_BYTES: usize = 7;

pub fn fixture(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/he-aac")
        .join(file_name)
}

/// Весь звук потока и формат каждого непустого пакета.
pub struct DecodedStream {
    pub samples: Vec<f32>,
    pub output_specs: Vec<AudioOutputSpec>,
}

impl DecodedStream {
    /// Первый же PCM — на выходной частоте и с выходными каналами,
    /// без смены формата дальше (иначе плеер пересобрал бы вывод звука).
    pub fn assert_first_output(&self, sample_rate: u32, channels: u32) {
        let first = self.output_specs.first().expect("stream produced PCM");
        assert_eq!(
            (first.sample_rate, first.channels()),
            (sample_rate, channels)
        );
        self.assert_every_output(sample_rate, channels);
    }

    pub fn assert_every_output(&self, sample_rate: u32, channels: u32) {
        for spec in &self.output_specs {
            assert_eq!((spec.sample_rate, spec.channels()), (sample_rate, channels));
        }
    }

    fn spec(&self) -> AudioOutputSpec {
        *self.output_specs.first().expect("stream produced PCM")
    }
}

/// Пакеты аудиодорожки и её паспорт — тем же демуксером, что у плеера.
pub fn demux_audio_packets(file_name: &str) -> (AacStreamConfig, Vec<Vec<u8>>) {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture(file_name)).expect("fixture opens");
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("audio track")
        .clone();
    let audio_specific_config = track
        .codec_private
        .expect("MP4 carries an AudioSpecificConfig");
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("read event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => {
                packets.push(packet.data.to_vec());
            }
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file is always ready"),
            _ => {}
        }
    }
    (
        AacStreamConfig::AudioSpecificConfig(audio_specific_config.to_vec()),
        packets,
    )
}

/// Кадры ADTS без заголовков — то, что демуксер MPEG-TS отдаёт декодеру.
pub fn adts_raw_frames(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).expect("fixture reads");
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset + ADTS_HEADER_BYTES <= bytes.len() {
        assert_eq!(bytes[offset], 0xFF, "ADTS sync at {offset}");
        let protection_absent = bytes[offset + 1] & 0x01 == 1;
        let frame_length = (usize::from(bytes[offset + 3] & 0x03) << 11)
            | (usize::from(bytes[offset + 4]) << 3)
            | (usize::from(bytes[offset + 5]) >> 5);
        let header_length = if protection_absent {
            ADTS_HEADER_BYTES
        } else {
            ADTS_HEADER_BYTES + 2
        };
        frames.push(bytes[offset + header_length..offset + frame_length].to_vec());
        offset += frame_length;
    }
    frames
}

/// Декодирует пакеты по одному, как это делает плеер.
pub fn decode_all(decoder: &mut FfmpegAacDecoder, packets: &[Vec<u8>]) -> DecodedStream {
    let mut stream = DecodedStream {
        samples: Vec::new(),
        output_specs: Vec::new(),
    };
    for packet in packets {
        if let Some(decoded) = decoder.decode_packet(packet).expect("packet decodes") {
            stream.output_specs.push(decoded.output_spec);
            stream.samples.extend(decoded.interleaved_samples);
        }
    }
    stream
}

/// Уровень тона частоты `frequency_hz` в канале `lane` (дБ от полной шкалы,
/// фильтр Гёрцеля с окном Ханна на секунде после начала).
pub fn tone_level_db(stream: &DecodedStream, lane: usize, frequency_hz: f64) -> f64 {
    let spec = stream.spec();
    let channels = spec.channels() as usize;
    let sample_rate = f64::from(spec.sample_rate);
    let start = (ANALYSIS_START_SECONDS * sample_rate) as usize;
    let length = (ANALYSIS_WINDOW_SECONDS * sample_rate) as usize;
    let lane_samples: Vec<f64> = stream
        .samples
        .iter()
        .skip(lane)
        .step_by(channels)
        .skip(start)
        .take(length)
        .map(|sample| f64::from(*sample))
        .collect();
    assert_eq!(
        lane_samples.len(),
        length,
        "stream shorter than the analysis window"
    );

    let coefficient = 2.0 * (2.0 * std::f64::consts::PI * frequency_hz / sample_rate).cos();
    let (mut previous, mut before_previous) = (0.0_f64, 0.0_f64);
    for (index, sample) in lane_samples.iter().enumerate() {
        let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / length as f64).cos();
        let current = sample * hann + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
    }
    let power = previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous;
    // Амплитуда синуса: |X| = A·N/4 для окна Ханна.
    let amplitude = 4.0 * power.max(0.0).sqrt() / length as f64;
    20.0 * amplitude.max(1e-12).log10()
}

/// Уровень (RMS, дБ от полной шкалы) всего, что выше `cutoff_hz`, в канале `lane`.
///
/// SBR восстанавливает верх полосами, а не точными частотами, поэтому верх
/// меряется энергией: два каскада фильтра верхних частот 2-го порядка (RBJ,
/// Q = 1/√2) дают −80 дБ/декаду — тон 440 Гц ослабляется больше чем на 100 дБ.
pub fn band_level_above_db(stream: &DecodedStream, lane: usize, cutoff_hz: f64) -> f64 {
    let spec = stream.spec();
    let channels = spec.channels() as usize;
    let sample_rate = f64::from(spec.sample_rate);
    let start = (ANALYSIS_START_SECONDS * sample_rate) as usize;
    let length = (ANALYSIS_WINDOW_SECONDS * sample_rate) as usize;
    let lane_samples: Vec<f64> = stream
        .samples
        .iter()
        .skip(lane)
        .step_by(channels)
        .map(|sample| f64::from(*sample))
        .collect();
    assert!(
        lane_samples.len() >= start + length,
        "stream shorter than the analysis window"
    );

    // Фильтр прогоняется с начала, чтобы к окну анализа его состояние установилось.
    let filtered = high_pass(
        &high_pass(&lane_samples, cutoff_hz, sample_rate),
        cutoff_hz,
        sample_rate,
    );
    let energy: f64 = filtered[start..start + length]
        .iter()
        .map(|sample| sample * sample)
        .sum();
    let rms = (energy / length as f64).sqrt();
    20.0 * rms.max(1e-12).log10()
}

/// Фильтр верхних частот 2-го порядка (RBJ Audio EQ Cookbook).
fn high_pass(samples: &[f64], cutoff_hz: f64, sample_rate: f64) -> Vec<f64> {
    let omega = 2.0 * std::f64::consts::PI * cutoff_hz / sample_rate;
    let alpha = omega.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let cos_omega = omega.cos();
    let a0 = 1.0 + alpha;
    let (b0, b1, b2) = (
        (1.0 + cos_omega) / 2.0 / a0,
        -(1.0 + cos_omega) / a0,
        (1.0 + cos_omega) / 2.0 / a0,
    );
    let (a1, a2) = (-2.0 * cos_omega / a0, (1.0 - alpha) / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    samples
        .iter()
        .map(|&x0| {
            let y0 = b0 * x0 + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, x0, y1, y0);
            y0
        })
        .collect()
}
