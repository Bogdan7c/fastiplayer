//! Граница «HostPlanar Y/U/V views слота загрузки + его кэш bind group-ов».
//!
//! Аналог `WgpuDmaBufPlaneViews` для программного пути: app получает набор
//! через `WgpuFrameTextureViews::host_planar_plane_views()` и передаёт в
//! `WgpuRenderableFrame::from_decoded_host_planar`, не зная про кэш.

use super::{FrameBindGroupCache, HostPlanarWgpuTextureViews};

/// Y/U/V views одного слота host-planar загрузки с кэшем его bind group-ов.
#[derive(Clone, Copy)]
pub struct WgpuHostPlanarPlaneViews<'views> {
    /// Texture view с luma/Y plane.
    pub y_view: &'views wgpu::TextureView,

    /// Texture view с chroma U/Cb plane.
    pub u_view: &'views wgpu::TextureView,

    /// Texture view с chroma V/Cr plane.
    pub v_view: &'views wgpu::TextureView,

    /// Кэш bind group-ов слота; `None` — bind group создаётся на кадр.
    pub(crate) bind_group_cache: Option<&'views FrameBindGroupCache>,
}

impl<'views> WgpuHostPlanarPlaneViews<'views> {
    /// Собирает набор из views, которыми владеет слот загрузки.
    pub(super) fn from_uploaded_views(views: &'views HostPlanarWgpuTextureViews) -> Self {
        Self {
            y_view: &views.y_view,
            u_view: &views.u_view,
            v_view: &views.v_view,
            bind_group_cache: Some(views.bind_group_cache()),
        }
    }
}
