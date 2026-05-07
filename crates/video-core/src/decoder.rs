use std::any::Any;
use std::time::Duration;

use media_core::Packet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTextureHandle(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Bt709Limited,
    Bt709Full,
    Bt601,
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub color_space: ColorSpace,
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
