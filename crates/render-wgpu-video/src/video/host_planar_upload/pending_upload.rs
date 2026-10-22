//! Передача GPU-копии host-planar кадра в submit кадра, который её показывает.
//!
//! Раньше загрузчик отправлял копию плоскостей отдельным `queue.submit` сразу после
//! memcpy в staging. По профилю cpu-parity 07 (AV1 4K60) этот лишний submit стоил
//! главному потоку ~0,12 мс на кадр — дороже submit-а всего кадра. Теперь копия
//! записывается в собственный command buffer и ждёт в слоте загрузки
//! (`PendingHostPlanarUpload`). Рендерер, нарисовав кадр из этого слота, забирает её
//! (`HostPlanarUploadCommands`), а shell отправляет её первой в общем submit кадра.
//!
//! Инвариант: записанная копия попадает в очередь ровно один раз и раньше любого
//! submit-а, который семплит слот.
//! - Кадр нарисован: копия уходит первой в submit этого кадра.
//! - Кадр из слота так и не нарисовали: загрузчик отправляет копию отдельным submit-ом
//!   в начале следующей загрузки (`take_unclaimed`), до записи новой копии. Порядок
//!   очереди сохраняется, а staging-чанки belt-а возвращаются, потому что их re-map
//!   привязан к submit-у именно этого command buffer-а.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::video::{WgpuHostPlanarPlaneViews, WgpuRenderableFrame};

use super::HostPlanarWgpuTextureViews;

/// Записанная, но ещё не отправленная в очередь копия плоскостей в один слот загрузки.
///
/// Живёт в слоте (`WgpuHostPlanarUploadedTextures`) и разделяется с загрузчиком, который
/// помнит последнюю запись, чтобы отправить её сам, если кадр так и не нарисовали.
#[derive(Default)]
pub(crate) struct PendingHostPlanarUpload {
    /// `Some` — копия записана и ждёт submit-а; `None` — отправлена или её не было.
    commands: Mutex<Option<wgpu::CommandBuffer>>,
}

impl PendingHostPlanarUpload {
    /// Кладёт только что записанную копию слота.
    ///
    /// Возвращает прежнюю копию, если её никто не отправил. По протоколу загрузчика
    /// такого не бывает (он отправляет невостребованную копию до новой записи), но если
    /// случилось, вызывающий обязан отправить прежнюю сразу: она старше новой.
    pub(super) fn store_recorded_commands(
        &self,
        commands: wgpu::CommandBuffer,
    ) -> Option<wgpu::CommandBuffer> {
        self.lock_commands().replace(commands)
    }

    /// Забирает копию, которую не забрал ни один нарисованный кадр.
    pub(super) fn take_unclaimed(&self) -> Option<wgpu::CommandBuffer> {
        self.lock_commands().take()
    }

    /// Забирает копию для submit-а кадра, который рисует этот слот.
    ///
    /// `None`, если копия уже ушла в очередь: слот показан повторно или загрузчик
    /// отправил её сам перед следующей загрузкой.
    fn claim_for_frame_submit(&self) -> Option<HostPlanarUploadCommands> {
        self.lock_commands()
            .take()
            .map(|commands| HostPlanarUploadCommands { commands })
    }

    /// Mutex защищает только `Option<CommandBuffer>`: инварианта, который могла бы
    /// сломать паника другого потока, здесь нет, поэтому отравление не мешает работе.
    fn lock_commands(&self) -> MutexGuard<'_, Option<wgpu::CommandBuffer>> {
        self.commands.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// GPU-копия плоскостей кадра, которую нужно отправить вместе с этим кадром.
///
/// Получается из `WgpuVideoRenderOutcome::host_upload`. Shell отправляет её первой в
/// том же `queue.submit`, что и command buffer кадра: копия обязана выполниться до
/// render pass-а, который семплит загруженные текстуры.
#[must_use = "загрузку кадра нужно отправить в queue.submit раньше command buffer-а кадра"]
pub struct HostPlanarUploadCommands {
    commands: wgpu::CommandBuffer,
}

impl HostPlanarUploadCommands {
    /// Command buffer копии для `queue.submit`.
    pub fn into_command_buffer(self) -> wgpu::CommandBuffer {
        self.commands
    }
}

impl HostPlanarWgpuTextureViews {
    /// Views слота вместе с кэшем bind group-ов и ожидающей загрузкой.
    ///
    /// Кадр из views материализатора собирается только так
    /// (`WgpuRenderableFrame::from_decoded_host_planar`): иначе рендерер не увидит
    /// ожидающую копию и нарисует текстуру до её загрузки.
    #[must_use]
    pub fn plane_views(&self) -> WgpuHostPlanarPlaneViews<'_> {
        WgpuHostPlanarPlaneViews::from_uploaded_views(self)
    }

    /// Ожидающая загрузка слота, которому принадлежат эти views.
    pub(in crate::video) fn pending_upload(&self) -> &PendingHostPlanarUpload {
        &self._uploaded_texture_guard.pending_upload
    }
}

impl WgpuRenderableFrame<'_> {
    /// Забирает GPU-копию плоскостей, если кадр рисует слот с ещё не отправленной загрузкой.
    ///
    /// Вызывается рендерером только после успешной записи video pass-а: если запись
    /// упала, копия остаётся в слоте и загрузчик отправит её сам.
    pub(in crate::video) fn claim_host_upload(&self) -> Option<HostPlanarUploadCommands> {
        self.host_upload
            .and_then(PendingHostPlanarUpload::claim_for_frame_submit)
    }
}

#[cfg(test)]
mod tests;
