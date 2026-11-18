//! Единственное место crate-а с `unsafe` и типами FFmpeg (`AV*`).
//!
//! Наружу выходят только нейтральные типы: [`crate::DecodedAacPcm`] и
//! [`crate::AudioFfmpegError`]. Это закрепляет тест `tests/raw_ffi_boundary.rs`.

mod averror;
mod codec_context;
mod frame_reader;
mod owned;

pub(crate) use codec_context::AacCodecContext;
