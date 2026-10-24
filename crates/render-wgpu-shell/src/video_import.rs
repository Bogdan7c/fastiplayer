//! Склад кадров wgpu-рендера: реализация `render_shell_api::VideoFrameImport`.
//!
//! До сессии 02 плана `user/gles2-renderer/` эти объекты создавало и хранило приложение
//! (`AppState`): обёртку декодера `wrap_video_backend_for_wgpu_submission`, материализаторы
//! `DmaBufWgpuFrameMaterializer` / `HostPlanarWgpuFrameMaterializer` и привязку отложенных
//! освобождений `WgpuSubmissionQueueBinding`. Теперь ими владеет рендер (решение владельца 3):
//! приложение передаёт аренду кадра и получает непрозрачный `RenderableFrameKey`.
//!
//! # Что здесь НЕ меняется относительно прежнего пути
//!
//! - кэш импорта DMA-BUF по идентичности буфера, его ёмкость по пулу декодера, барьеры
//!   FOREIGN (cpu-parity 03) и путь загрузки без CPU-копии (08b) живут в `render-wgpu-video`;
//! - освобождение кадра, отправленного в GPU, по-прежнему ждёт `on_submitted_work_done`
//!   внутри обёртки provider-а, ровно один раз;
//! - очередь отложенных освобождений перепривязывается к новому рендеру только при установке
//!   импорта, подготовленного пересозданием (после ожидания старого GPU — порядок держит
//!   `RendererLifecycleCoordinator` приложения).

mod renderable_frame;

use std::sync::Arc;

use render_core::RenderCapabilities;
use render_shell_api::{
    FrameImportCacheSnapshot, FrameImportError, FrameImportStats, FrameImportUnsupportedReason,
    FrameMaterialization, FrameMaterializationOutcome, PendingVideoImport, PreparedVideoImport,
    ReleasedFrameCount, RenderableFrameKey, RendererFamily, RendererInstanceId, VideoFrameImport,
    VideoImportHandoff, VideoImportInstallError, VideoImportPath, VideoImportPlan,
    VideoImportPrepareError, VideoImportPreparer, VideoImportReleaseError,
};
use render_wgpu_video::{
    DmaBufImportCacheCapacity, DmaBufImportStats, DmaBufWgpuFrameMaterializer,
    HostPlanarWgpuFrameMaterializer, WgpuFrameMaterializationUnsupportedReason,
    WgpuFrameTextureViewLookup, WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
    WgpuSubmissionQueueBinding, wrap_video_backend_for_wgpu_submission,
};
use video_backend_api::{PresentFrameResourceProviderHandle, StartedVideoBackend};
use video_present_core::VideoFrameLease;

pub(crate) use renderable_frame::renderable_frame_from_views;

/// Семейство, которому принадлежат коробки этого склада (импорт, перенос при пересоздании).
const WGPU_FAMILY: RendererFamily = RendererFamily::WgpuVulkan;

/// GPU-объекты рендера, нужные материализаторам (клоны handle-ов wgpu, это `Arc` внутри).
#[derive(Clone)]
pub(crate) struct WgpuImportDevice {
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

/// Склад кадров одного экземпляра wgpu-рендера.
pub(crate) struct WgpuVideoFrameImport {
    /// Паспорт рендера: им помечены подготовленные импорты и ключи кадров.
    owner: RendererInstanceId,

    /// GPU-объекты рендера для создания материализаторов.
    gpu: WgpuImportDevice,

    /// Возможности рендера: первый фильтр `materialize` («объявлен ли контракт кадра»).
    capabilities: RenderCapabilities,

    /// Активный импорт; `None` — декодер не подключён.
    active: Option<WgpuImportState>,
}

/// Состояние одного импорта: материализатор и привязка отложенных освобождений.
///
/// Привязка разделена с обёрткой provider-а, которую получил плеер: через неё освобождения
/// кадров, отправленных в GPU, ждут завершения работы очереди этого рендера.
#[derive(Clone)]
struct WgpuImportState {
    /// План, под который создан материализатор (путь + размер пула декодера).
    plan: VideoImportPlan,

    /// Материализатор DMA-BUF или HostPlanar на ресурсах этого рендера.
    materializer: Arc<dyn WgpuFrameTextureViewMaterializer>,

    /// Очередь отложенных освобождений (общая с обёрткой provider-а декодера).
    submission_binding: WgpuSubmissionQueueBinding,
}

/// Откуда взялся подготовленный импорт: это решает, нужна ли перепривязка очереди.
enum PendingImportOrigin {
    /// Новый декодер: привязка уже создана на очереди этого рендера.
    FreshDecoder,

    /// Пересоздание рендера: привязку старого рендера надо перевести на эту очередь.
    RecreatedRenderer,
}

/// Содержимое `PendingVideoImport` этого рендера.
struct PendingWgpuVideoImport {
    state: WgpuImportState,
    origin: PendingImportOrigin,
}

/// Содержимое `VideoImportHandoff`: активный импорт старого рендера.
struct WgpuVideoImportHandoff {
    state: WgpuImportState,
}

/// Подготовленные ресурсы кадра внутри `RenderableFrameKey` (открывает только этот рендер).
pub(crate) struct WgpuPreparedFrame {
    /// Plane views кадра; держат импорт/слот загрузки, пока жив ключ.
    pub(crate) views: WgpuFrameTextureViews,
}

impl WgpuVideoFrameImport {
    /// Пустой склад рендера: декодер ещё не подключён.
    pub(crate) const fn new(
        owner: RendererInstanceId,
        gpu: WgpuImportDevice,
        capabilities: RenderCapabilities,
    ) -> Self {
        Self {
            owner,
            gpu,
            capabilities,
            active: None,
        }
    }

    /// Подготовщик импорта для этого рендера (`RenderShell::video_import_preparer`).
    pub(crate) fn video_import_preparer(&self) -> Box<dyn VideoImportPreparer> {
        Box::new(WgpuVideoImportPreparer {
            owner: self.owner,
            gpu: self.gpu.clone(),
        })
    }

    /// Превращает готовые views в ключ после проверки, что рендер сможет их нарисовать.
    fn key_for_ready_views(
        &self,
        lease: &VideoFrameLease,
        views: WgpuFrameTextureViews,
    ) -> FrameMaterializationOutcome {
        // Та же сборка, что выполнит `render_frame`: несовпадение формата кадра и вида views
        // ловится здесь, до показа (раньше — в app перед `render_frame`, с тем же исходом).
        if let Err(error) = renderable_frame_from_views(lease.decoded_frame(), &views) {
            tracing::error!(
                error = %error,
                format = %lease.decoded_frame().format(),
                memory_path = %lease.decoded_frame().memory_path(),
                "Failed to build WGPU renderable frame"
            );
            return FrameMaterializationOutcome::Unsupported(
                FrameImportUnsupportedReason::LayoutNotHandledByActiveImport,
            );
        }
        FrameMaterializationOutcome::Ready(RenderableFrameKey::new(
            self.owner,
            lease.clone(),
            WgpuPreparedFrame { views },
        ))
    }
}

impl VideoFrameImport for WgpuVideoFrameImport {
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        Ok(prepare_wgpu_import(self.owner, &self.gpu, backend, plan))
    }

    fn install_video_import(
        &mut self,
        pending: PendingVideoImport,
    ) -> Result<(), VideoImportInstallError> {
        let pending = pending.into_state::<PendingWgpuVideoImport>(self.owner)?;
        if matches!(pending.origin, PendingImportOrigin::RecreatedRenderer) {
            // Отравленный mutex — типизированный отказ: доверять состоянию освобождений нельзя.
            pending
                .state
                .submission_binding
                .rebind(&self.gpu.queue)
                .map_err(|error| VideoImportInstallError::SubmissionBindingFailed {
                    message: error.to_string(),
                })?;
        }
        self.active = Some(pending.state);
        Ok(())
    }

    fn active_video_import_path(&self) -> Option<VideoImportPath> {
        self.active.as_ref().map(|active| active.plan.path)
    }

    fn materialize(&mut self, lease: &VideoFrameLease) -> FrameMaterialization {
        let frame_contract = lease.decoded_frame().frame_contract;
        if !self.capabilities.supports_frame_contract(frame_contract) {
            return FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::Unsupported(
                    FrameImportUnsupportedReason::ContractNotDeclared { frame_contract },
                ),
            );
        }
        let Some(active) = &self.active else {
            return FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::NoVideoBackendBound,
            );
        };

        let lookup = active
            .materializer
            .try_texture_view_lookup(lease.decoded_frame());
        let resource_pool_lock_wait = lookup.texture_pool_lock_wait();
        let outcome = match lookup {
            WgpuFrameTextureViewLookup::Ready { views, .. } => {
                self.key_for_ready_views(lease, views)
            }
            WgpuFrameTextureViewLookup::Busy { .. } => FrameMaterializationOutcome::Busy,
            WgpuFrameTextureViewLookup::Missing { .. } => FrameMaterializationOutcome::Missing,
            WgpuFrameTextureViewLookup::Unsupported { reason, .. } => {
                FrameMaterializationOutcome::Unsupported(neutral_unsupported_reason(reason))
            }
            WgpuFrameTextureViewLookup::Error { .. } => {
                FrameMaterializationOutcome::Error(FrameImportError::ImportFailed {
                    message: "WGPU frame materializer reported a lookup error".to_owned(),
                })
            }
        };
        FrameMaterialization {
            outcome,
            resource_pool_lock_wait,
        }
    }

    fn import_stats(&self) -> Option<FrameImportStats> {
        self.active
            .as_ref()
            .and_then(|active| active.materializer.dma_buf_import_stats())
            .map(neutral_import_stats)
    }

    fn export_video_import_for_recreation(&self) -> Option<VideoImportHandoff> {
        self.active.as_ref().map(|active| {
            VideoImportHandoff::new(
                WGPU_FAMILY,
                active.plan.path,
                WgpuVideoImportHandoff {
                    state: active.clone(),
                },
            )
        })
    }

    fn prepare_recreated_video_import(
        &mut self,
        handoff: VideoImportHandoff,
    ) -> Result<PendingVideoImport, VideoImportPrepareError> {
        let path = handoff.path();
        let handoff = handoff.into_state::<WgpuVideoImportHandoff>(WGPU_FAMILY)?;
        // Новый материализатор на ресурсах этого рендера с тем же provider-ом декодера;
        // старый продолжает работать до установки (активный импорт старого рендера не тронут).
        let materializer = handoff.state.materializer.recreate_for_renderer(
            &self.gpu.instance,
            &self.gpu.adapter,
            &self.gpu.device,
            &self.gpu.queue,
        );
        Ok(PendingVideoImport::new(
            self.owner,
            path,
            PendingWgpuVideoImport {
                state: WgpuImportState {
                    plan: handoff.state.plan,
                    materializer,
                    submission_binding: handoff.state.submission_binding,
                },
                origin: PendingImportOrigin::RecreatedRenderer,
            },
        ))
    }

    fn release_after_device_lost(&mut self) -> Result<ReleasedFrameCount, VideoImportReleaseError> {
        let Some(active) = &self.active else {
            return Ok(ReleasedFrameCount::new(0));
        };
        active
            .submission_binding
            .release_after_device_lost()
            .map(ReleasedFrameCount::new)
            .map_err(|_poisoned| VideoImportReleaseError::PendingReleaseStatePoisoned)
    }
}

/// Подготовщик импорта для потока плеера (кандидаты нового файла, решение владельца в 02).
///
/// Держит клоны wgpu-handle-ов рендера (как прежний `WgpuCandidateVideoPipelineResourceDriver`
/// приложения) и создаёт обёртку декодера и материализатор там же, где их создавал драйвер.
struct WgpuVideoImportPreparer {
    owner: RendererInstanceId,
    gpu: WgpuImportDevice,
}

impl VideoImportPreparer for WgpuVideoImportPreparer {
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        Ok(prepare_wgpu_import(self.owner, &self.gpu, backend, plan))
    }
}

/// Оборачивает декодер и готовит импорт на ресурсах рендера `owner`.
///
/// Плеер получает обёртку: освобождение кадра, отправленного в GPU, дойдёт до декодера
/// только после `on_submitted_work_done` очереди этого рендера.
fn prepare_wgpu_import(
    owner: RendererInstanceId,
    gpu: &WgpuImportDevice,
    backend: StartedVideoBackend,
    plan: VideoImportPlan,
) -> PreparedVideoImport {
    let (player_backend, resource_provider, submission_binding) =
        wrap_video_backend_for_wgpu_submission(backend, &gpu.queue);
    let materializer = create_materializer(gpu, plan, resource_provider);
    let pending = PendingVideoImport::new(
        owner,
        plan.path,
        PendingWgpuVideoImport {
            state: WgpuImportState {
                plan,
                materializer,
                submission_binding,
            },
            origin: PendingImportOrigin::FreshDecoder,
        },
    );
    PreparedVideoImport {
        backend: player_backend,
        pending,
    }
}

/// Создаёт материализатор под путь плана на ресурсах рендера.
fn create_materializer(
    gpu: &WgpuImportDevice,
    plan: VideoImportPlan,
    resource_provider: PresentFrameResourceProviderHandle,
) -> Arc<dyn WgpuFrameTextureViewMaterializer> {
    match plan.path {
        VideoImportPath::HardwareZeroCopy => {
            // Кэш импорта не больше пула поверхностей декодера (cpu-parity 03).
            let import_cache_capacity =
                DmaBufImportCacheCapacity::matching_surface_pool(plan.decoder_surface_pool.get());
            Arc::new(DmaBufWgpuFrameMaterializer::new(
                &gpu.instance,
                &gpu.adapter,
                &gpu.device,
                &gpu.queue,
                resource_provider,
                import_cache_capacity,
            ))
        }
        VideoImportPath::SoftwareHostUpload => Arc::new(HostPlanarWgpuFrameMaterializer::new(
            &gpu.device,
            &gpu.queue,
            resource_provider,
        )),
    }
}

/// Переводит причину отказа wgpu-материализатора в нейтральный словарь контракта.
fn neutral_unsupported_reason(
    reason: WgpuFrameMaterializationUnsupportedReason,
) -> FrameImportUnsupportedReason {
    match reason {
        // Материализатор не того пути передачи (или кадр не того контракта загрузки).
        WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresUploadMaterializer
        | WgpuFrameMaterializationUnsupportedReason::DmaBufRequiresDmaBufMaterializer
        | WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresSoftwareUploadContract => {
            FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
        }
        WgpuFrameMaterializationUnsupportedReason::HostPlanarLayoutNotSupportedByUploadMaterializer => {
            FrameImportUnsupportedReason::LayoutNotHandledByActiveImport
        }
        // Причина сохраняется целиком: по ней приложение переходит на программный декод.
        WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(rejection) => {
            FrameImportUnsupportedReason::DmaBufDescriptorRejected(rejection)
        }
    }
}

/// Переводит счётчики кэша импорта DMA-BUF в нейтральную форму (имена полей те же).
const fn neutral_import_stats(stats: DmaBufImportStats) -> FrameImportStats {
    FrameImportStats {
        imports_performed: stats.imports_performed,
        import_failures: stats.import_failures,
        image_reuse_enabled: stats.image_reuse_enabled,
        cache: FrameImportCacheSnapshot {
            entries: stats.cache.entries,
            capacity: stats.cache.capacity,
            hits: stats.cache.hits,
            replaced_same_resource: stats.cache.replaced_same_resource,
            evicted_format_change: stats.cache.evicted_format_change,
            evicted_capacity: stats.cache.evicted_capacity,
        },
    }
}

#[cfg(test)]
mod tests;
