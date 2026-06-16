//! Safe placeholder for future refcounted `AVFrame` ownership.

use super::error::{FfiResult, unsupported};

/// Opaque owner для будущего refcounted `AVFrame`.
///
/// Type не раскрывает raw pointer наружу. Когда decode появится, именно этот
/// wrapper будет удерживать `AVFrame` до `release_frame`.
#[derive(Debug)]
pub struct FfmpegFrame {
    /// Поле закрывает struct literal снаружи crate-а.
    _private: (),
}

impl FfmpegFrame {
    /// Scaffold не создаёт `AVFrame`, чтобы не имитировать несуществующий decode path.
    pub fn allocate_for_decode() -> FfiResult<Self> {
        Err(unsupported("av_frame_alloc"))
    }
}
