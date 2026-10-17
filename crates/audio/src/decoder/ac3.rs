//! Crate-private adapter AC-3 / E-AC-3 поверх собственного декодера `ac3-decoder`.
//!
//! Архитектура (по образцу Opus fallback-а):
//! - Symphonia 0.6 распознаёт `A_AC3`/`A_EAC3`, но декодера не имеет, поэтому
//!   `create_audio_decoder` направляет exact identity сюда до Symphonia lookup;
//! - [`ac3_decoder::Ac3Decoder`] (clean-room по A/52 / ETSI TS 102 366) отдаёт
//!   планарный f32 без ограничения амплитуды и typed позицию каждого канала.
//!   Один экземпляр декодирует и AC-3, и E-AC-3, выбирая синтаксис по `bsid`
//!   каждого access unit-а (TS-поток может переключиться без смены дорожки);
//! - [`canonical_layout`] переводит позиции в нейтральный canonical layout и
//!   перемежает каналы — codec-specific порядок не пересекает audio boundary;
//! - каждый кадр отдаётся в своём настоящем формате ([`frame_format`]): смену
//!   раскладки или частоты посреди потока (2.0 ↔ 5.1) player-core обрабатывает
//!   пересборкой audio output-а без потери A/V sync.
//!
//! Политика ошибок (три разных случая, не сливаются в один):
//! - битый packet (ошибка битового потока, CRC, обрезка) — случайность: `warn`
//!   и тишина той же длительности в формате последнего удачного кадра (звук не
//!   убегает от видео); до первого удачного кадра — пустой результат;
//! - постоянная причина (неподдерживаемая возможность потока, раскладка без
//!   нейтрального представления) — ОДНА ошибка `decode`, дальше та же тишина
//!   без новых ошибок, пока причина не изменится ([`persistent_failure`]);
//! - нарушение контракта декодера (число каналов не сходится) — ошибка `decode`.

mod canonical_layout;
mod frame_format;
mod persistent_failure;
#[cfg(test)]
mod tests;

use ac3_decoder::{
    Ac3Decoder, ChannelPosition, CrcPolicy, DecodedAudio, DecoderConfig, DitherMode, DitherSeed,
    DynamicRangeControl, TransientPreNoiseProcessing, UnsupportedFeature,
};
use anyhow::Result;
use audio_core::{AudioChannelLayout, AudioDecoder, AudioDecoderConfig, EncodedAudioPacket};
use thiserror::Error;
use tracing::{info, warn};

use canonical_layout::LayoutConverter;
use frame_format::{LastFrameFormat, PublishedFrameFormat};
use persistent_failure::{FailureReport, PersistentFailure, PersistentFailureNotice};

/// Точный codec identity Matroska / нейтрального demux для AC-3.
pub(super) const AC3_CODEC_ID: &str = "A_AC3";
/// Точный codec identity Matroska / нейтрального demux для E-AC-3.
pub(super) const EAC3_CODEC_ID: &str = "A_EAC3";

/// Какой из двух точных identity обслуживает decoder (для diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ac3CodecIdentity {
    /// `A_AC3`.
    Ac3,
    /// `A_EAC3`.
    Eac3,
}

impl Ac3CodecIdentity {
    /// Распознаёт только exact строки: похожие identity не подменяются.
    pub(super) fn from_codec_id(codec_id: &str) -> Option<Self> {
        match codec_id {
            AC3_CODEC_ID => Some(Self::Ac3),
            EAC3_CODEC_ID => Some(Self::Eac3),
            _ => None,
        }
    }

    /// Строка identity для сообщений.
    const fn codec_id(self) -> &'static str {
        match self {
            Self::Ac3 => AC3_CODEC_ID,
            Self::Eac3 => EAC3_CODEC_ID,
        }
    }
}

/// Настройки декодера в production (решение проекта, задано явно).
///
/// - CRC проверяется: кадр с несовпавшей CRC — битый packet (тишина), а не
///   громкий шум из испорченных мантисс;
/// - dither псевдослучайный (A/52 §7.3.4 требует заполнять мантиссы без бит
///   шумом; так же делает FFmpeg по умолчанию);
/// - DRC «line out»: полный `dynrng` (A/52 §7.7.1.1 «shall, by default»;
///   FFmpeg по умолчанию так же). Dialnorm и downmix декодер не применяет —
///   громкость и сведение каналов принадлежат приложению;
/// - подавление пред-шума транзиентов E-AC-3 (TPNP, Annex E §3.7) применяется:
///   спецификация требует его от эталонного декодера.
const fn production_decoder_config() -> DecoderConfig {
    DecoderConfig {
        crc: CrcPolicy::Verify,
        dither: DitherMode::Pseudorandom(DitherSeed::DEFAULT),
        dynamic_range: DynamicRangeControl::Standard,
        transient_pre_noise: TransientPreNoiseProcessing::Apply,
    }
}

/// Typed ошибки AC-3 / E-AC-3 decode path-а.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum Ac3DecodeError {
    /// Packet не разбирается как AC-3 / E-AC-3 (битые данные, CRC, обрезка, мусор).
    #[error("corrupted {codec_id} packet: {source}")]
    CorruptPacket {
        /// Identity дорожки.
        codec_id: &'static str,
        /// Typed причина из декодера.
        source: ac3_decoder::Ac3DecodeError,
    },
    /// Поток корректен, но использует возможность, которую декодер не реализует.
    #[error("{codec_id} stream uses an unsupported decoder feature: {feature:?}")]
    UnsupportedStream {
        /// Identity дорожки.
        codec_id: &'static str,
        /// Что именно не поддержано.
        feature: UnsupportedFeature,
    },
    /// Раскладка программы не переводится в нейтральные позиции громкоговорителей.
    #[error("AC-3 program channel layout {decoder_channels:?} has no neutral speaker mapping")]
    UnconvertibleChannelLayout {
        /// Позиции каналов декодера в порядке декодирования.
        decoder_channels: Vec<ChannelPosition>,
    },
    /// Описание каналов кадра не согласуется с samples (нарушение контракта декодера).
    #[error("invalid AC-3 channel description: {reason}")]
    InvalidChannelDescription {
        /// Что именно не сошлось.
        reason: String,
    },
}

/// Результат декодирования одного packet-а после применения политики ошибок.
#[derive(Debug, PartialEq)]
enum PacketOutcome {
    /// Кадр в своём настоящем формате (он же теперь опубликован).
    Decoded(Vec<f32>),
    /// Кадр не отдан (битый packet или уже сообщённая постоянная причина):
    /// вместо него тишина длиной последнего удачного кадра.
    Concealed(Vec<f32>),
    /// Кадр не отдан до первого удачного кадра: длительность неизвестна, PCM нет.
    DroppedBeforeFirstFrame,
    /// Кадр отвергнут ошибкой `decode`: новая постоянная причина (сообщается
    /// ровно один раз) или нарушение контракта декодера.
    Rejected(Ac3DecodeError),
}

/// Production decoder AC-3 / E-AC-3 за neutral `AudioDecoder` boundary.
pub(super) struct Ac3FamilyDecoder {
    /// Track ID, для которого создан decoder.
    track_id: u32,
    /// Exact identity дорожки.
    identity: Ac3CodecIdentity,
    /// Собственный декодер access unit-ов.
    program_decoder: Ac3Decoder,
    /// Перестановка каналов в canonical порядок (кэш по раскладке кадра).
    layout_converter: LayoutConverter,
    /// Формат последнего удачного кадра (для spec-а и маскировки).
    frame_format: LastFrameFormat,
    /// О какой постоянной причине уже сообщили ошибкой.
    persistent_failure: PersistentFailureNotice,
    /// Sample rate из container metadata до первого кадра (0 = неизвестно).
    container_sample_rate: u32,
    /// Число каналов из container metadata до первого кадра (0 = неизвестно).
    container_channels: u32,
}

impl Ac3FamilyDecoder {
    /// Создаёт decoder; container metadata нужна только до первого кадра.
    pub(super) fn new(config: &AudioDecoderConfig, identity: Ac3CodecIdentity) -> Result<Self> {
        super::conversion::validate_audio_decoder_config(config)?;
        info!(
            track_id = config.track_id(),
            codec_id = identity.codec_id(),
            sample_rate = ?config.sample_rate(),
            channels = ?config.channels(),
            "AC-3 family audio decoder created"
        );
        Ok(Self {
            track_id: config.track_id(),
            identity,
            program_decoder: Ac3Decoder::new(production_decoder_config()),
            layout_converter: LayoutConverter::default(),
            frame_format: LastFrameFormat::default(),
            persistent_failure: PersistentFailureNotice::default(),
            container_sample_rate: config.sample_rate().unwrap_or(0),
            container_channels: config.channels().unwrap_or(0),
        })
    }

    /// Декодирует access unit и применяет политику ошибок модуля.
    fn decode_access_unit(&mut self, packet_bytes: &[u8]) -> PacketOutcome {
        match self.program_decoder.decode_access_unit(packet_bytes) {
            Ok(decoded) => self.publish_decoded_frame(&decoded),
            Err(ac3_decoder::Ac3DecodeError::Unsupported(feature)) => {
                self.handle_persistent_failure(PersistentFailure::UnsupportedFeature(feature))
            }
            Err(source) => self.conceal_corrupt_packet(source),
        }
    }

    /// Переводит удачный кадр в canonical PCM и публикует его формат.
    fn publish_decoded_frame(&mut self, decoded: &DecodedAudio) -> PacketOutcome {
        let neutral_layout = match self.layout_converter.conversion_for(decoded.channels()) {
            Ok(neutral_layout) => neutral_layout,
            Err(unconvertible) => {
                let failure =
                    PersistentFailure::UnconvertibleLayout(unconvertible.decoder_channels.clone());
                return self.handle_persistent_failure(failure);
            }
        };
        let layout = neutral_layout.layout();
        let interleaved = match neutral_layout.interleave(decoded) {
            Ok(interleaved) => interleaved,
            Err(error) => return PacketOutcome::Rejected(error),
        };
        self.frame_format.publish(
            PublishedFrameFormat {
                sample_rate: decoded.sample_rate_hz(),
                layout,
                frames_per_packet: decoded.samples_per_channel(),
            },
            self.track_id,
        );
        self.persistent_failure.clear_after_decoded_frame();
        PacketOutcome::Decoded(interleaved)
    }

    /// Битый packet: предупреждение и тишина вместо кадра.
    fn conceal_corrupt_packet(&self, source: ac3_decoder::Ac3DecodeError) -> PacketOutcome {
        let error = Ac3DecodeError::CorruptPacket {
            codec_id: self.identity.codec_id(),
            source,
        };
        warn!(
            track_id = self.track_id,
            error = %error,
            "Corrupted AC-3 family packet replaced by silence"
        );
        self.concealment()
    }

    /// Постоянная причина: ошибка один раз, дальше тишина без новых ошибок.
    fn handle_persistent_failure(&mut self, failure: PersistentFailure) -> PacketOutcome {
        let error = self.persistent_failure_error(&failure);
        match self.persistent_failure.register(failure) {
            FailureReport::ReportNow => {
                warn!(
                    track_id = self.track_id,
                    error = %error,
                    "AC-3 family program cannot be played; following packets become silence"
                );
                PacketOutcome::Rejected(error)
            }
            FailureReport::AlreadyReported => self.concealment(),
        }
    }

    /// Typed ошибка для постоянной причины.
    fn persistent_failure_error(&self, failure: &PersistentFailure) -> Ac3DecodeError {
        match failure {
            PersistentFailure::UnsupportedFeature(feature) => Ac3DecodeError::UnsupportedStream {
                codec_id: self.identity.codec_id(),
                feature: *feature,
            },
            PersistentFailure::UnconvertibleLayout(decoder_channels) => {
                Ac3DecodeError::UnconvertibleChannelLayout {
                    decoder_channels: decoder_channels.clone(),
                }
            }
        }
    }

    /// Тишина в формате последнего удачного кадра либо ничего до первого кадра.
    fn concealment(&self) -> PacketOutcome {
        self.frame_format.concealment_silence().map_or(
            PacketOutcome::DroppedBeforeFirstFrame,
            PacketOutcome::Concealed,
        )
    }

    /// Проверяет, что packet относится к track-у decoder-а.
    fn ensure_packet_track_matches(&self, packet: &EncodedAudioPacket<'_>) -> Result<()> {
        if packet.track_id() == self.track_id {
            return Ok(());
        }
        anyhow::bail!(
            "Audio packet track mismatch: decoder track {}, packet track {}",
            self.track_id,
            packet.track_id()
        );
    }
}

impl AudioDecoder for Ac3FamilyDecoder {
    /// Декодирует packet в interleaved f32 canonical layout-а.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        self.ensure_packet_track_matches(packet)?;
        match self.decode_access_unit(packet.data()) {
            PacketOutcome::Decoded(samples) | PacketOutcome::Concealed(samples) => Ok(samples),
            PacketOutcome::DroppedBeforeFirstFrame => Ok(Vec::new()),
            PacketOutcome::Rejected(error) => Err(error.into()),
        }
    }

    /// Сбрасывает межкадровое состояние декодера (overlap-add, dither) после seek.
    ///
    /// Формат последнего кадра сохраняется: до первого кадра после seek spec не
    /// откатывается к container metadata (иначе player-core пересоздал бы output зря).
    /// Сообщённая постоянная причина тоже сохраняется: seek её не устраняет.
    fn reset(&mut self) -> Result<()> {
        self.program_decoder.reset();
        Ok(())
    }

    /// Частота последнего кадра либо container metadata до первого кадра.
    fn sample_rate(&self) -> u32 {
        self.frame_format
            .published()
            .map_or(self.container_sample_rate, |published| {
                published.sample_rate
            })
    }

    /// Число каналов последнего кадра либо container metadata.
    fn channels(&self) -> u32 {
        self.frame_format
            .published()
            .map_or(self.container_channels, |published| {
                published.layout.channel_count()
            })
    }

    /// Layout последнего кадра; до первого кадра — только однозначный mono/stereo.
    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        match self.frame_format.published() {
            Some(published) => Some(published.layout),
            None => AudioChannelLayout::from_channel_count(self.container_channels).ok(),
        }
    }
}
