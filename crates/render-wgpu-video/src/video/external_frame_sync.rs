//! Связка «DMA-BUF кадр в render pass-е» с явной синхронизацией внешнего писателя.
//!
//! Рендерер семплит импортированный image, который между кадрами заново пишет
//! аппаратный декодер. Чтобы чтение видело новое содержимое и декодер не
//! получил поверхность обратно раньше, чем шейдер её дочитал, вокруг каждого
//! video pass-а ставятся два барьера (см. `dma_buf_import::external_sync`).
//! wgpu запрещает писать их в основной encoder кадра, поэтому они живут в
//! отдельных command buffer-ах, а shell отправляет их в том же `queue.submit`
//! до и после основного command buffer-а.
//!
//! Этот модуль — типы границы между materializer-ом (откуда берётся
//! синхронизация image), renderer-ом (кто записывает барьеры) и shell-ом (кто
//! отправляет их в очередь).

use anyhow::Context;

use crate::dma_buf_import::ImportedImageExternalSync;

use super::FrameBindGroupCache;

/// DMA-BUF plane views одного кадра вместе с его синхронизацией.
///
/// Получается из `WgpuFrameTextureViews::dma_buf_views()` и передаётся в
/// конструкторы `WgpuRenderableFrame`, чтобы app не собирал синхронизацию сам.
#[derive(Clone, Copy)]
pub struct WgpuDmaBufPlaneViews<'views> {
    /// Texture view с luma/Y plane.
    pub y_view: &'views wgpu::TextureView,

    /// Texture view с interleaved chroma/UV plane.
    pub uv_view: &'views wgpu::TextureView,

    /// Синхронизация image с внешним писателем; `None` при покадровом импорте.
    pub(crate) external_sync: Option<&'views ImportedImageExternalSync>,

    /// Кэш bind group-ов импортированной текстуры; `None` — bind group на кадр.
    pub(crate) bind_group_cache: Option<&'views FrameBindGroupCache>,
}

/// Command buffer-ы барьеров одного video pass-а.
///
/// Shell обязан отправить `acquire` ДО основного command buffer-а кадра и
/// `release` ПОСЛЕ него в одном `queue.submit`.
pub struct ExternalFrameSyncCommandBuffers {
    /// Acquire-барьер: внешняя очередь -> graphics family, до чтения в шейдере.
    pub acquire: wgpu::CommandBuffer,

    /// Release-барьер: graphics family -> внешняя очередь, после чтения.
    pub release: wgpu::CommandBuffer,
}

/// Итог одного video pass-а для shell-а.
///
/// `must_use`: потерять `external_sync` значит показать image без acquire-барьера.
#[must_use = "external_sync command buffers must be submitted around the frame encoder"]
pub struct WgpuVideoRenderOutcome {
    /// `true`, если video pass реально нарисовал кадр (а не только очистил target).
    pub frame_drawn: bool,

    /// Барьеры, которые shell обязан отправить вокруг основного command buffer-а.
    pub external_sync: Option<ExternalFrameSyncCommandBuffers>,
}

impl WgpuVideoRenderOutcome {
    /// Кадра не было: target очищен или pass пропущен, барьеры не нужны.
    pub const fn without_frame() -> Self {
        Self {
            frame_drawn: false,
            external_sync: None,
        }
    }
}

/// Записывает acquire/release для кадра, если его image требует синхронизации.
///
/// Делается до записи render pass-а в основной encoder: порядок submit-а задаёт
/// shell, а здесь важно лишь, что оба command buffer-а готовы к моменту submit-а.
pub(super) fn record_external_frame_sync(
    device: &wgpu::Device,
    external_sync: Option<&ImportedImageExternalSync>,
) -> anyhow::Result<Option<ExternalFrameSyncCommandBuffers>> {
    let Some(external_sync) = external_sync else {
        return Ok(None);
    };
    let acquire = external_sync
        .record_acquire(device)
        .context("record DMA-BUF frame acquire barrier")?;
    let release = external_sync
        .record_release(device)
        .context("record DMA-BUF frame release barrier")?;
    Ok(Some(ExternalFrameSyncCommandBuffers { acquire, release }))
}
