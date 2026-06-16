//! Thin FFmpeg FFI boundary.
//!
//! Все будущие raw pointer types, ownership rules и unsafe calls должны
//! оставаться в этом module tree. Соседние modules получают только safe
//! wrappers и typed errors.

pub mod codec_context;
pub mod error;
pub mod frame;
pub mod packet;
