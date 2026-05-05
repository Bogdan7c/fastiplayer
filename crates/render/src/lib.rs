use anyhow::Result;
use video_core::DecodedFrame;

pub mod nv12_renderer;

pub trait VideoRenderer {
    fn render_frame(
        &mut self,
        frame: &DecodedFrame,
        target: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()>;
}
