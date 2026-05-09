use std::any::Any;
use std::time::Duration;

use codec_core::VideoColorMetadata;
use media_core::Packet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTextureHandle(pub u64);

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub color: VideoColorMetadata,
    pub texture_handle: FrameTextureHandle,
}

pub trait VideoDecoder {
    fn decode(&mut self, packet: &Packet) -> anyhow::Result<Option<DecodedFrame>>;
    fn flush(&mut self) -> anyhow::Result<()>;
    fn backend_name(&self) -> &'static str;

    /// Downcast to concrete type for backend-specific operations.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{ColorMetadataOrigin, ColorRange, MatrixCoefficients};

    /// Создаёт test frame без реальных GPU resources и без legacy `ColorSpace`.
    fn decoded_test_frame() -> DecodedFrame {
        DecodedFrame {
            pts: Duration::ZERO,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: FrameTextureHandle(1),
        }
    }

    #[test]
    fn decoded_frame_test_helper_uses_explicit_color_metadata() {
        let frame = decoded_test_frame();

        assert_eq!(frame.color.range, ColorRange::Limited);
        assert_eq!(frame.color.matrix, MatrixCoefficients::Bt709);
        assert_eq!(frame.color.origin, ColorMetadataOrigin::FallbackDefault);
    }
}
