//! Thin FFmpeg FFI boundary.
//!
//! Все будущие raw pointer types, ownership rules и unsafe calls должны
//! оставаться в этом module tree. Соседние modules получают только safe
//! wrappers и typed errors.

pub mod codec_context;
#[cfg(feature = "ffmpeg")]
mod direct_frame_buffers;
pub mod error;
pub mod frame;
#[cfg(any(test, feature = "ffmpeg"))]
pub(crate) mod host_frame_memory;
pub mod packet;
pub mod pixel_format;
pub mod runtime_version;
