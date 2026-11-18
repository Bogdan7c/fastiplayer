//! Декодер HE-AAC (AAC + SBR, AAC + SBR + Parametric Stereo) поверх
//! встроенного AAC-декодера закреплённого LGPL FFmpeg.
//!
//! Зачем отдельный crate (решения 347–348 плана моста): нормативный текст SBR
//! и PS платный, свой clean-room-декодер владелец отложил. Звук лент
//! Instagram (`mp4a.40.5`) и TikTok (`mp4a.40.29`), а также многих радио и
//! IPTV — HE-AAC. Обычный AAC-LC по-прежнему декодирует патч Symphonia;
//! какой поток куда идёт, решает маршрут в crate `audio`.
//!
//! Граница: наружу выходят только нейтральные типы `audio-core`
//! ([`DecodedAacPcm`] несёт `AudioOutputSpec`). Типы и `unsafe` FFmpeg живут
//! в приватном модуле `ffi`. Guardrail разрешает FFmpeg только этому crate-у
//! и `video-ffmpeg`.

// Перевод раскладки нужен только FFI-слою; тесты правил идут в любой сборке.
#[cfg(any(feature = "ffmpeg", test))]
mod channel_mapping;
mod decoded_pcm;
mod decoder;
mod error;
#[cfg(feature = "ffmpeg")]
mod ffi;
mod stream_config;

pub use decoded_pcm::DecodedAacPcm;
pub use decoder::FfmpegAacDecoder;
pub use error::AudioFfmpegError;
pub use stream_config::AacStreamConfig;
