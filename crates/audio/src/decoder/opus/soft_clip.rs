//! Мягкое ограничение (soft clip) float PCM, которое libopus делает сам только в i16 API.
//!
//! Почему эта стадия существует. libopus в обычной сборке декодирует во float.
//! Его i16 API (`opus_decode`, `opus_multistream_decode`) после декодирования
//! вызывает `opus_pcm_soft_clip` и лишь затем квантует сигнал в 16 бит. Float API
//! (`opus_decode_float`, `opus_multistream_decode_float`) отдаёт сигнал как есть,
//! без soft clip: пики выше 0 dBFS остаются больше 1.0. Мы декодируем через float
//! API (без лишней 16-битной квантизации и двух конверсий), а поведение
//! ограничения пиков сохраняем здесь, вызывая тот же libopus-алгоритм через
//! безопасную обёртку `opus::SoftClip`.
//!
//! Эквивалентность i16-пути libopus:
//! - single-stream: libopus применяет soft clip ко всему interleaved buffer-у
//!   декодера с памятью на каждый канал — делаем то же одним `SoftClip`;
//! - multistream: libopus применяет soft clip отдельно к выходу каждого
//!   элементарного stream-а (mono или coupled stereo). Алгоритм
//!   `opus_pcm_soft_clip` обрабатывает каналы независимо (у каждого своя ячейка
//!   памяти, общая только предварительная обрезка до ±2 — тоже посэмплово),
//!   поэтому ограничение каждого выходного канала отдельно даёт тот же результат.
//!   `opus::SoftClip` умеет только 1–2 канала, поэтому для multistream держим
//!   по одному mono clipper-у на выходной канал и прогоняем канал через
//!   переиспользуемый scratch buffer;
//! - память soft clip (продолжение нелинейности через границу packet-ов)
//!   живёт между packet-ами и сбрасывается вместе с decoder state, как
//!   `OPUS_RESET_STATE` сбрасывает `softclip_mem` внутри libopus.

use opus::{Channels, SoftClip};

use super::header::OpusBackendConfig;

/// Стадия soft clip для decoded Opus PCM с памятью между packet-ами.
pub(super) enum OpusSoftClipStage {
    /// Single-stream (mono/stereo): один libopus clipper на interleaved buffer.
    Interleaved {
        /// Состояние libopus soft clip (по ячейке памяти на канал).
        clipper: SoftClip,

        /// Раскладка, с которой clipper пересоздаётся при reset.
        channels: Channels,
    },

    /// Multistream: независимый mono clipper на каждый выходной канал.
    PerChannel {
        /// Clipper-ы в порядке interleaved выходных каналов.
        channel_clippers: Vec<SoftClip>,

        /// Переиспользуемый buffer одного канала: растёт до самого длинного
        /// packet-а один раз, дальше на горячем пути не аллоцирует.
        channel_scratch: Vec<f32>,
    },
}

impl OpusSoftClipStage {
    /// Создаёт стадию под уже выбранный libopus backend.
    pub(super) fn for_backend(backend: &OpusBackendConfig) -> Self {
        match backend {
            OpusBackendConfig::SingleStream { channels } => Self::Interleaved {
                clipper: SoftClip::new(*channels),
                channels: *channels,
            },
            OpusBackendConfig::Multistream {
                canonical_mapping, ..
            } => Self::PerChannel {
                // Длина mapping table равна числу выходных каналов multistream decoder-а.
                channel_clippers: canonical_mapping
                    .iter()
                    .map(|_| SoftClip::new(Channels::Mono))
                    .collect(),
                channel_scratch: Vec::new(),
            },
        }
    }

    /// Ограничивает interleaved PCM на месте, продолжая память прошлого packet-а.
    ///
    /// Длина `interleaved_pcm` должна быть кратна числу каналов: decoder
    /// передаёт ровно `frame_count * channels` сэмплов.
    pub(super) fn apply(&mut self, interleaved_pcm: &mut [f32]) {
        match self {
            Self::Interleaved { clipper, .. } => clipper.apply(interleaved_pcm),
            Self::PerChannel {
                channel_clippers,
                channel_scratch,
            } => apply_per_channel(channel_clippers, channel_scratch, interleaved_pcm),
        }
    }

    /// Забывает память soft clip после seek/discontinuity, как это делает
    /// `OPUS_RESET_STATE` в libopus для i16-пути.
    pub(super) fn reset(&mut self) {
        match self {
            Self::Interleaved { clipper, channels } => *clipper = SoftClip::new(*channels),
            Self::PerChannel {
                channel_clippers, ..
            } => {
                for channel_clipper in channel_clippers.iter_mut() {
                    *channel_clipper = SoftClip::new(Channels::Mono);
                }
            }
        }
    }
}

/// Прогоняет каждый выходной канал через собственный mono clipper.
fn apply_per_channel(
    channel_clippers: &mut [SoftClip],
    channel_scratch: &mut Vec<f32>,
    interleaved_pcm: &mut [f32],
) {
    let channel_count = channel_clippers.len();
    if channel_count == 0 {
        return;
    }

    for (channel_index, channel_clipper) in channel_clippers.iter_mut().enumerate() {
        // Собираем один канал из interleaved buffer-а в непрерывный кусок,
        // потому что libopus soft clip работает только с interleaved 1–2 каналами.
        channel_scratch.clear();
        channel_scratch.extend(
            interleaved_pcm
                .iter()
                .skip(channel_index)
                .step_by(channel_count)
                .copied(),
        );

        channel_clipper.apply(channel_scratch);

        // Возвращаем ограниченный канал на его места в interleaved buffer-е.
        for (interleaved_sample, clipped_sample) in interleaved_pcm
            .iter_mut()
            .skip(channel_index)
            .step_by(channel_count)
            .zip(channel_scratch.iter())
        {
            *interleaved_sample = *clipped_sample;
        }
    }
}

#[cfg(test)]
mod tests {
    use opus::{Channels, SoftClip};

    use super::OpusSoftClipStage;
    use crate::decoder::opus::header::OpusBackendConfig;

    /// Количество stereo frames в синтетическом «packet-е».
    const FRAMES_PER_PACKET: usize = 480;

    /// Multistream-раскладка с двумя выходными каналами (один coupled stream).
    fn two_channel_multistream_backend() -> OpusBackendConfig {
        OpusBackendConfig::Multistream {
            streams: 1,
            coupled_streams: 1,
            canonical_mapping: vec![0, 1],
        }
    }

    /// Громкий stereo-сигнал выше 0 dBFS: каналы разные, чтобы перепутанная
    /// память или каналы дали другой результат.
    fn loud_stereo_packet(packet_index: usize) -> Vec<f32> {
        let mut interleaved = Vec::with_capacity(FRAMES_PER_PACKET * 2);
        for frame_index in 0..FRAMES_PER_PACKET {
            let time = (packet_index * FRAMES_PER_PACKET + frame_index) as f32 / 48_000.0;
            interleaved.push(1.7 * (std::f32::consts::TAU * 110.0 * time).sin());
            interleaved.push(1.3 * (std::f32::consts::TAU * 70.0 * time + 0.5).cos());
        }
        interleaved
    }

    /// Mono-кусок, который заканчивается внутри пика выше 1.0: после него у
    /// libopus soft clip остаётся ненулевая память.
    fn mono_ramp_ending_above_full_scale() -> Vec<f32> {
        (0..FRAMES_PER_PACKET)
            .map(|frame_index| 1.5 * frame_index as f32 / (FRAMES_PER_PACKET - 1) as f32)
            .collect()
    }

    /// Per-channel путь multistream обязан совпадать с interleaved libopus clip-ом
    /// для тех же каналов, включая память через границы packet-ов.
    #[test]
    fn per_channel_stage_matches_interleaved_libopus_soft_clip() {
        let mut per_channel_stage =
            OpusSoftClipStage::for_backend(&two_channel_multistream_backend());
        let mut libopus_reference = SoftClip::new(Channels::Stereo);

        for packet_index in 0..6 {
            let source_packet = loud_stereo_packet(packet_index);
            assert!(
                source_packet.iter().any(|sample| sample.abs() > 1.0),
                "test signal must exceed full scale"
            );

            let mut stage_output = source_packet.clone();
            per_channel_stage.apply(&mut stage_output);
            let mut reference_output = source_packet;
            libopus_reference.apply(&mut reference_output);

            assert_eq!(stage_output, reference_output, "packet {packet_index}");
            assert!(
                stage_output
                    .iter()
                    .all(|sample| (-1.0..=1.0).contains(sample))
            );
        }
    }

    /// Reset обязан забыть память soft clip у обоих вариантов стадии.
    #[test]
    fn reset_forgets_soft_clip_memory() {
        let backends = [
            OpusBackendConfig::SingleStream {
                channels: Channels::Mono,
            },
            OpusBackendConfig::Multistream {
                streams: 1,
                coupled_streams: 0,
                canonical_mapping: vec![0],
            },
        ];
        // Следующий packet начинается с того же знака, что и пик прошлого:
        // libopus продолжает нелинейность, пока знак не сменится.
        let quiet_continuation = vec![0.9_f32; FRAMES_PER_PACKET];

        for backend in &backends {
            let mut remembering_stage = OpusSoftClipStage::for_backend(backend);
            let mut clipped_ramp = mono_ramp_ending_above_full_scale();
            remembering_stage.apply(&mut clipped_ramp);
            let mut continued = quiet_continuation.clone();
            remembering_stage.apply(&mut continued);
            assert!(
                continued[0] < 0.9,
                "{backend:?}: memory must bend the next packet without reset"
            );

            let mut reset_stage = OpusSoftClipStage::for_backend(backend);
            let mut clipped_ramp = mono_ramp_ending_above_full_scale();
            reset_stage.apply(&mut clipped_ramp);
            reset_stage.reset();
            let mut after_reset = quiet_continuation.clone();
            reset_stage.apply(&mut after_reset);
            assert_eq!(after_reset, quiet_continuation, "{backend:?}");
        }
    }

    /// Сигнал в пределах full scale проходит без изменений (обычный случай).
    #[test]
    fn signal_within_full_scale_passes_unchanged() {
        let quiet_packet: Vec<f32> = loud_stereo_packet(0)
            .into_iter()
            .map(|sample| sample * 0.5)
            .collect();
        for backend in [
            OpusBackendConfig::SingleStream {
                channels: Channels::Stereo,
            },
            two_channel_multistream_backend(),
        ] {
            let mut stage = OpusSoftClipStage::for_backend(&backend);
            let mut processed = quiet_packet.clone();
            stage.apply(&mut processed);
            assert_eq!(processed, quiet_packet, "{backend:?}");
        }
    }
}
