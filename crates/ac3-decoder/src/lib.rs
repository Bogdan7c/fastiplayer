//! Собственный clean-room декодер AC-3 (Dolby Digital) и E-AC-3
//! (Dolby Digital Plus): один [`Ac3Decoder`] декодирует оба синтаксиса,
//! выбирая его по `bsid` каждого access unit-а.
//!
//! Единственные источники истины — спецификации ATSC A/52:2012 (AC-3 +
//! Annex E E-AC-3) и ETSI TS 102 366 v1.4.1. Чужие реализации декодеров не
//! используются; ffmpeg применяется только как внешний кодер тестовых
//! потоков и эталон для сравнения.
//!
//! # Карта модулей
//!
//! | модуль        | ответственность                                          | разделы A/52       |
//! |---------------|----------------------------------------------------------|--------------------|
//! | `bitstream`   | ограниченный читатель бит, CRC-16                        | §5.2, §7.10.1      |
//! | `tables`      | таблицы спецификации с тестами-сверками (`tables::eac3` — Annex E) | Т. 5.6, 5.18, 7.6–7.23, 7.34, E2.10–E4.7 |
//! | `syntax`      | синтаксис AC-3: syncinfo, BSI (+Annex D), audblk, хвост; `syntax::eac3` — Annex E и access unit с зависимыми substream-ами | §5.3–§5.5, Annex D, Annex E §2, §3.8 |
//! | `spectral`    | экспоненты, битовое распределение, мантиссы, dither, coupling — общие с E-AC-3; `aht`, `spx`, `ecpl` — инструменты Annex E | §7.1–§7.4, Annex E §3.4–§3.6 |
//! | `decoder`     | публичный [`Ac3Decoder`]: CRC-политика, состояние между кадрами | §6.1, §7.10 |
//! | `output`      | [`DecodedAudio`], позиции каналов                        | Т. 5.8, Т. E2.5    |
//! | `synthesis`   | rematrixing, DRC, IMDCT + окно KBD, overlap-add, TPNP, сведение substream-ов программы | §7.5, §7.7, §7.9, Annex E §3.7–3.8 |
//! | `error`       | типизированные ошибки                                    | §7.10.2            |
//!
//! Access unit E-AC-3 — независимый substream 0 (или ядро AC-3, как на
//! Blu-ray) и его зависимые substream-ы: каналы зависимых заменяют
//! одноимённые и добавляют новые позиции (Annex E §3.8.2, вплоть до 16
//! каналов), `dynrng`/`compr` берутся у последнего зависимого (§3.8.5),
//! другие программы (substreamid 1–7) пропускаются. Формат (частота, число
//! блоков 1/2/3/6, позиции каналов) сообщается для каждого access unit-а.
//! Без PCM разбор доступен через [`inspect_access_unit`] и
//! [`read_enhanced_syncframe_info`].
//!
//! # Что не проверено эталоном
//!
//! - `bsid` 9/10 (AC-3 с частотой ÷2/÷4): обе спецификации эти значения не
//!   определяют («shall mute», A/52 §5.4.2.1, Annex E §2.3.1.6); декодер по
//!   требованию demux-стороны разбирает их синтаксисом AC-3 с пониженной
//!   частотой. ffmpeg n9.0.2 такие кадры не декодирует, реальных потоков в
//!   samples.ffmpeg.org нет — путь проверен только самосогласованностью.
//! - Пониженные частоты E-AC-3 (`fscod2`, 24/22,05/16 кГц): ffmpeg их не
//!   поддерживает; проверено эквивалентностью (тот же кадр на исходной
//!   частоте даёт тот же PCM).
//! - ECPL (улучшенный coupling): реальных потоков нет (ни у кодера ffmpeg).
//!
//! # Инварианты
//!
//! - Никакой вход не вызывает панику: все чтения ограничены, индексы —
//!   из проверенных диапазонов; нарушения — [`Ac3DecodeError`].
//! - Состояние между блоками живёт внутри одного кадра (§7.1.2); между
//!   кадрами — генераторы dither-а и шума SPX (детерминированные), хвосты
//!   overlap-add каналов (§7.9.4 шаг 6) и история TPNP (Annex E §3.7); всё
//!   это сбрасывает [`Ac3Decoder::reset`].
//! - Ошибка access unit-а — разрыв потока: хвосты overlap-add и TPNP забываются,
//!   следующий кадр начинается как первый (нарастающая половина окна, без
//!   щелчка после тишины, которую подставит приложение). Генератор dither-а
//!   продолжает свою последовательность.
//! - PCM не ограничивается по амплитуде; первый кадр не обрезается: отсчёт 0
//!   выхода — отсчёт 0 первого блока (совпадает с ffmpeg, задержка 0);
//!   компенсация задержки кодера (256 отсчётов у ffmpeg) — дело контейнера
//!   и приложения.
//! - Аудиоблоки не читают дальше начала хвоста кадра; после шести блоков
//!   `nauxbits ≥ 0` и `auxdatal ≤ nauxbits` (§5.4.4).

#![forbid(unsafe_code)]

mod bitstream;
mod config;
mod decoder;
mod error;
mod output;
mod spectral;
mod syntax;
mod synthesis;
mod tables;

#[cfg(test)]
mod tests;

pub use config::{
    CompressionScale, CrcPolicy, DecoderConfig, DitherMode, DitherSeed, DynamicRangeControl,
    TransientPreNoiseProcessing,
};
pub use decoder::Ac3Decoder;
pub use error::{
    Ac3DecodeError, AccessUnitViolation, AudioBlockIndex, BitstreamViolation, CrcRegion,
    FrameSection, ReusedElement, UnsupportedFeature,
};
pub use output::{ChannelPosition, DecodedAudio, ac3_channel_positions};
pub use syntax::annex_d::{
    AlternateCenterMixLevel, AlternateSurroundMixLevel, AnalogDigitalConverterType, BsiExtension,
    DolbyHeadphoneMode, DolbySurroundExMode, ExtendedDownmixInfo, ExtendedProductionInfo,
    PreferredStereoDownmix,
};
pub use syntax::bsi::BitStreamInfo;
pub use syntax::bsi_values::{
    AudioCodingMode, AudioProductionInfo, CenterMixLevel, DialogueNormalization, DolbySurroundMode,
    ProgramMetadata, RoomType, ServiceType, SurroundMixLevel,
};
pub use syntax::eac3::access_unit::SubstreamHeader;
pub use syntax::eac3::audfrm::{AudioFrameSummary, TransientPreNoise};
pub use syntax::eac3::bsi::{
    ConvertedFrameInfo, EnhancedBitStreamInfo, EnhancedProgramMetadata, StreamType,
};
pub use syntax::eac3::bsi_metadata::{
    EnhancedProductionInfo, InformationalMetadata, MixingMetadata, MixingOption,
};
pub use syntax::eac3::channel_map::CustomChannelMap;
pub use syntax::eac3::inspection::{AccessUnitInspection, SubstreamInspection};
pub use syntax::eac3::syncframe::EnhancedSyncframeInfo;
pub use syntax::eac3::tool_usage::EnhancedToolUsage;
pub use syntax::syncframe::SyncframeInfo;
pub use tables::frame_size::SampleRateCode;

/// Разбирает заголовок syncframe-а AC-3 (`syncinfo` + `bsi`, A/52 §5.3.1–5.3.2)
/// без проверки CRC и без декодирования аудио. Буфер должен содержать кадр
/// целиком (иначе [`Ac3DecodeError::Truncated`] с его длиной).
pub fn read_syncframe_info(frame: &[u8]) -> Result<SyncframeInfo, Ac3DecodeError> {
    syntax::syncframe::read_syncframe_info(frame)
}

/// Разбирает заголовок syncframe-а E-AC-3 (`bsi` + `audfrm`, A/52 Annex E
/// §2.2.1–2.2.3) без проверки CRC и без аудиоблоков. Буфер должен содержать
/// кадр целиком (иначе [`Ac3DecodeError::Truncated`] с его длиной).
pub fn read_enhanced_syncframe_info(frame: &[u8]) -> Result<EnhancedSyncframeInfo, Ac3DecodeError> {
    syntax::eac3::syncframe::read_enhanced_syncframe_info(frame)
}

/// Полностью разбирает access unit (AC-3 или E-AC-3 с зависимыми
/// substream-ами) до частотных коэффициентов и описывает программу 0:
/// заголовки substream-ов, позиции их кодированных каналов и использованные
/// инструменты Annex E. PCM не синтезируется; CRC и dither — по `config`.
pub fn inspect_access_unit(
    access_unit: &[u8],
    config: DecoderConfig,
) -> Result<AccessUnitInspection, Ac3DecodeError> {
    syntax::eac3::inspection::inspect_access_unit(access_unit, config)
}
