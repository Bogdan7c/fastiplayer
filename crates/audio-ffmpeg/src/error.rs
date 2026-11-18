//! Типизированные ошибки декодера HE-AAC поверх FFmpeg.
//!
//! Вызывающему коду (`audio`) важно различать четыре разные ситуации, поэтому
//! они не сливаются в одну строку:
//! - FFmpeg не собран в эту сборку (`FeatureDisabled`) — постоянная причина;
//! - описание потока негодно (`InvalidStreamConfig`) — ошибка конфигурации;
//! - один пакет битый (`CorruptPacket`) — можно пропустить и играть дальше;
//! - FFmpeg вернул формат или раскладку, которую мы не умеем выразить
//!   нейтрально (`UnsupportedSampleFormat`, `UnsupportedChannelLayout`).

use thiserror::Error;

/// Ошибка декодера HE-AAC поверх FFmpeg.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AudioFfmpegError {
    /// Сборка без FFmpeg (`--no-default-features`): HE-AAC честно недоступен.
    #[error("HE-AAC decoding needs FFmpeg, which is not part of this build")]
    FeatureDisabled,

    /// FFmpeg не нашёл встроенный AAC-декодер (FFmpeg собран без него).
    #[error("FFmpeg has no built-in AAC decoder")]
    DecoderNotFound,

    /// Описание потока нельзя передать FFmpeg (пустое, слишком большое, нулевая частота).
    #[error("invalid AAC stream config: {details}")]
    InvalidStreamConfig {
        /// Что именно не так с описанием потока.
        details: String,
    },

    /// FFmpeg не смог выделить память под контекст, пакет или кадр.
    #[error("FFmpeg allocation failed: {operation}")]
    AllocationFailed {
        /// Какую структуру не удалось выделить.
        operation: &'static str,
    },

    /// FFmpeg отверг данные пакета как битые (`AVERROR_INVALIDDATA`).
    ///
    /// Это ошибка одного пакета: декодер остаётся годным для следующих.
    #[error("corrupt AAC packet: {message}")]
    CorruptPacket {
        /// Текст ошибки FFmpeg (`av_strerror`).
        message: String,
    },

    /// Любая другая ошибка FFmpeg с исходным кодом и операцией.
    #[error("FFmpeg {operation} failed ({code}): {message}")]
    Ffmpeg {
        /// Вызов FFmpeg, который вернул ошибку.
        operation: &'static str,
        /// Отрицательный код `AVERROR`.
        code: i32,
        /// Текст ошибки FFmpeg (`av_strerror`).
        message: String,
    },

    /// Декодер выдал формат отсчётов, отличный от ожидаемого float planar.
    #[error("FFmpeg AAC decoder produced unsupported sample format {format}")]
    UnsupportedSampleFormat {
        /// Числовое значение `AVSampleFormat`.
        format: i32,
    },

    /// Раскладку каналов кадра нельзя выразить нейтральными позициями плеера.
    #[error("FFmpeg AAC decoder produced unsupported channel layout: {details}")]
    UnsupportedChannelLayout {
        /// Диагностическое описание раскладки (порядок, маска, число каналов).
        details: String,
    },

    /// Один пакет дал кадры разного формата — склеить их в один буфер нельзя.
    #[error("one AAC packet produced frames with different formats")]
    MixedFramesInPacket,
}
