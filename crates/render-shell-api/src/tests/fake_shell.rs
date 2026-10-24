//! Fake-рендер: минимальная честная реализация контракта без GPU.
//!
//! «GPU» — очередь отложенных освобождений: кадр, отданный рендеру, возвращается декодеру
//! только в `wait_for_gpu_idle` (аналог `on_submitted_work_done`/EGL fence). Привязка
//! освобождений к «очереди» переживает пересоздание рендера, как `WgpuSubmissionQueueBinding`.
//!
//! [`FakeDefect`] намеренно ломает одно правило: тесты доказывают, что conformance-проверка
//! это ловит (проверка не «вакуумная»).

use std::mem;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use render_core::{
    RenderBackendKind, RenderCapabilities, RenderDiagnostics, RenderLiveApplyPhase,
    RenderLiveApplyReport, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup, StartedVideoBackend,
    VideoBackendDecoderThreadHandle,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceHandle, VideoDecoderDiagnosticEvent, VideoDecoderThreadHandle,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract, VideoFrameTransferPath};
use video_present_core::VideoFrameLease;

use super::fake_presentation::FakePresentation;
use super::fake_ui_textures::{
    FakeAppliedUi, FakeMissingTexturePolicy, FakeUiFrame, FakeUiTextureSync, FakeUiTextures,
};
use crate::AcquiredSurfaceFrame;
use crate::{
    EguiFrameContent, FrameImportError, FrameImportStats, FrameImportUnsupportedReason,
    FrameMaterialization, FrameMaterializationOutcome, PendingVideoImport, PreparedVideoImport,
    ReleasedFrameCount, RenderFrameDropReason, RenderFrameFailure, RenderFrameInput,
    RenderFrameOutcome, RenderFrameStageTimings, RenderFrameTiming, RenderHotPathStats,
    RenderScreenDescriptor, RenderShell, RenderSurfaceSize, RenderableFrameKey, RendererFamily,
    RendererGpuDrainError, RendererInstanceId, SurfaceFrameAcquire, VideoFrameImport,
    VideoImportHandoff, VideoImportInstallError, VideoImportPath, VideoImportPlan,
    VideoImportPrepareError, VideoImportPreparer, VideoImportReleaseError,
};
use crate::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured, SurfacePresentSettings,
};

/// Какое правило контракта fake нарушает (для доказательства, что проверки его ловят).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeDefect {
    /// Честная реализация.
    None,
    /// Рисует по чужому билету.
    AcceptsForeignSurface,
    /// Повторяет интерфейс, которого нет.
    ReusesWithoutRetainedUi,
    /// Неподдерживаемый контракт отдаёт как поломку.
    ReportsUnsupportedAsError,
    /// «Декодер не подключён» выдаёт за «ресурса нет».
    ReportsMissingBackendAsMissing,
    /// Держит ключи кадров у себя после показа (кадр не возвращается декодеру).
    RetainsKeys,
    /// При потере устройства освобождает кадры, но оставляет их в очереди «GPU».
    KeepsPendingAfterDeviceLost,
    /// Подготовщик импорта помечает импорт паспортом другого рендера.
    PreparerStampsForeignOwner,
    /// Выдаёт буферы и после того, как отпустил окно (вторая surface на окне).
    AcquiresWhileDetached,
    /// Перенастройка показа всегда «применена», даже когда ничего не меняется.
    ReconfigureNeverReportsNoOp,
    /// Частичную правку неизвестной текстуры интерфейса молча принимает (как будто она есть).
    AcceptsPartialEditOfUnknownUiTexture,
    /// Применяет текстуры интерфейса только в кадре, который рисуется (теряет их при отказе).
    AppliesUiTexturesOnlyWhenDrawing,
}

/// Fake-«GPU»: кадры, ждущие завершения работы.
#[derive(Default)]
struct FakeGpu {
    pending: Mutex<Vec<(PresentFrameResourceProviderHandle, FrameResourceHandle)>>,
}

impl FakeGpu {
    fn schedule_release(
        &self,
        inner: PresentFrameResourceProviderHandle,
        handle: FrameResourceHandle,
    ) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((inner, handle));
    }

    /// Работа GPU завершена: каждый ждущий кадр освобождается один раз.
    fn complete_submitted_work(&self) -> usize {
        let completed =
            mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner));
        let count = completed.len();
        for (inner, handle) in completed {
            inner.release_frame(handle);
        }
        count
    }

    /// Ошибочная реализация потери устройства: освобождает, но очередь не очищает.
    fn release_without_clearing(&self) -> usize {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        for (inner, handle) in pending.iter() {
            inner.release_frame(*handle);
        }
        pending.len()
    }

    fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// К какой «очереди» привязаны отложенные освобождения провайдера.
enum FakeBindingState {
    Active(Arc<FakeGpu>),
    /// Устройство потеряно: новые освобождения проходят сразу.
    Lost,
}

/// Разделяемая привязка (обёртка provider-а и склад рендера держат одну и ту же).
#[derive(Clone)]
struct FakeSubmissionBinding(Arc<Mutex<FakeBindingState>>);

impl FakeSubmissionBinding {
    fn new(gpu: Arc<FakeGpu>) -> Self {
        Self(Arc::new(Mutex::new(FakeBindingState::Active(gpu))))
    }

    fn rebind(&self, gpu: Arc<FakeGpu>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = FakeBindingState::Active(gpu);
    }

    fn mark_lost(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = FakeBindingState::Lost;
    }

    fn schedule_release(
        &self,
        inner: &PresentFrameResourceProviderHandle,
        handle: FrameResourceHandle,
    ) {
        match &*self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            FakeBindingState::Active(gpu) => gpu.schedule_release(inner.clone(), handle),
            FakeBindingState::Lost => inner.release_frame(handle),
        }
    }
}

/// Обёртка provider-а декодера: lookup-ы насквозь, освобождение — после «GPU».
struct FakeSubmittedProvider {
    inner: PresentFrameResourceProviderHandle,
    binding: FakeSubmissionBinding,
}

impl PresentFrameResourceProvider for FakeSubmittedProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        self.inner.resource_lookup(handle)
    }

    fn try_resource_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.inner.try_resource_lookup(handle)
    }

    fn resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.inner.resource_descriptor_lookup(handle)
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.binding.schedule_release(&self.inner, handle);
    }
}

/// Поток декодера, который плеер получает от рендера: provider — обёртка рендера.
struct FakeWrappedDecoderThread {
    inner: Box<VideoBackendDecoderThreadHandle>,
    renderer_provider: PresentFrameResourceProviderHandle,
}

impl VideoDecoderThreadHandle for FakeWrappedDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeSendError> {
        self.inner.send_packet(packet)
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.inner.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.inner.try_recv_frame()
    }

    fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        self.inner.try_recv_diagnostic_event()
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        self.inner.try_recv_error()
    }

    fn flush(&self) -> anyhow::Result<()> {
        self.inner.flush()
    }

    fn resource_provider(&self) -> Self::ResourceProvider {
        self.renderer_provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.inner.decoder_resource_snapshot()
    }

    fn packet_queue_depth(&self) -> usize {
        self.inner.packet_queue_depth()
    }

    fn drain_completed_packet_count(&self) -> usize {
        self.inner.drain_completed_packet_count()
    }
}

/// Состояние импорта (в коробках `PendingVideoImport`/`VideoImportHandoff` и активное).
struct FakeImportState {
    path: VideoImportPath,
    renderer_provider: PresentFrameResourceProviderHandle,
    binding: FakeSubmissionBinding,
}

/// «Подготовленные ресурсы» кадра внутри ключа.
struct FakePreparedFrame {
    handle: FrameResourceHandle,
}

/// «Буфер swapchain» внутри билета.
struct FakeSurfaceBuffer;

/// Живые настройки: просто хранит снимок.
struct FakeLiveSettings {
    settings: RenderLiveSettings,
}

impl RenderLiveSettingsAdapter for FakeLiveSettings {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.settings = update.settings.clone();
        Ok(RenderLiveApplyReport::applied(
            RenderLiveApplyPhase::Preview,
            update.changed_fields.clone(),
        ))
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.settings = settings.clone();
        Ok(RenderLiveApplyReport::no_op(RenderLiveApplyPhase::Commit))
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.settings = baseline.clone();
        Ok(RenderLiveApplyReport::no_op(RenderLiveApplyPhase::Rollback))
    }
}

/// Fake-рендер.
pub(super) struct FakeRenderShell {
    id: RendererInstanceId,
    family: RendererFamily,
    defect: FakeDefect,
    capabilities: RenderCapabilities,
    gpu: Arc<FakeGpu>,
    retained_ui: Option<RenderScreenDescriptor>,
    active_import: Option<FakeImportState>,
    live_settings: FakeLiveSettings,
    presented_video: Vec<FrameResourceHandle>,
    retained_keys: Vec<RenderableFrameKey>,
    presentation: FakePresentation,
    ui_textures: FakeUiTextures,
}

impl FakeRenderShell {
    pub(super) fn new(family: RendererFamily, defect: FakeDefect) -> Self {
        Self {
            id: RendererInstanceId::allocate(),
            family,
            defect,
            capabilities: fake_capabilities(family.backend_kind()),
            gpu: Arc::new(FakeGpu::default()),
            retained_ui: None,
            active_import: None,
            live_settings: FakeLiveSettings {
                settings: RenderLiveSettings::default(),
            },
            presented_video: Vec::new(),
            retained_keys: Vec::new(),
            presentation: FakePresentation::new(SurfacePresentSettings::default()),
            ui_textures: FakeUiTextures::default(),
        }
    }

    /// Какие кадры декодера рендер нарисовал (по порядку).
    pub(super) fn presented_video(&self) -> &[FrameResourceHandle] {
        &self.presented_video
    }

    /// Сколько кадров ждут завершения «GPU».
    pub(super) fn pending_gpu_releases(&self) -> usize {
        self.gpu.pending_count()
    }

    fn import_handles(&self, path: VideoImportPath) -> bool {
        // Fake умеет оба пути; реальный рендер решает по возможностям драйвера.
        matches!(
            path,
            VideoImportPath::HardwareZeroCopy | VideoImportPath::SoftwareHostUpload
        )
    }

    fn materialize_with_active_import(
        &self,
        lease: &VideoFrameLease,
        active: &FakeImportState,
    ) -> FrameMaterialization {
        let frame_contract = lease.decoded_frame().frame_contract;
        if import_path_of(frame_contract) != active.path {
            return FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::Unsupported(
                    FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport,
                ),
            );
        }
        let handle = lease.resource_handle();
        let lookup = active.renderer_provider.try_resource_lookup(handle);
        let resource_pool_lock_wait = lookup.resource_pool_lock_wait();
        let outcome = match lookup {
            PresentFrameResourceProviderLookup::Ready { .. } => FrameMaterializationOutcome::Ready(
                RenderableFrameKey::new(self.id, lease.clone(), FakePreparedFrame { handle }),
            ),
            PresentFrameResourceProviderLookup::Busy { .. } => FrameMaterializationOutcome::Busy,
            PresentFrameResourceProviderLookup::Missing { .. } => {
                FrameMaterializationOutcome::Missing
            }
            PresentFrameResourceProviderLookup::Fatal { .. } => {
                FrameMaterializationOutcome::Error(FrameImportError::ProviderFatal)
            }
        };
        FrameMaterialization {
            outcome,
            resource_pool_lock_wait,
        }
    }

    fn reject_frame(&self, failure: RenderFrameFailure) -> RenderFrameOutcome {
        RenderFrameOutcome::Failed(failure)
    }

    fn missing_texture_policy(&self) -> FakeMissingTexturePolicy {
        if self.defect == FakeDefect::AcceptsPartialEditOfUnknownUiTexture {
            FakeMissingTexturePolicy::SilentlyAccept
        } else {
            FakeMissingTexturePolicy::Report
        }
    }

    /// Первый шаг кадра: текстуры интерфейса — состояние рендера, применяются до отказов.
    fn begin_ui_frame(&mut self, egui: EguiFrameContent) -> FakeUiFrame {
        if self.defect == FakeDefect::AppliesUiTexturesOnlyWhenDrawing {
            return FakeUiFrame::Deferred(egui);
        }
        let policy = self.missing_texture_policy();
        FakeUiFrame::Applied(self.ui_textures.apply_frame(egui, policy))
    }
}

/// Путь передачи кадра по его контракту.
fn import_path_of(frame_contract: VideoFrameContract) -> VideoImportPath {
    match frame_contract.transfer_path {
        VideoFrameTransferPath::HardwareZeroCopy { .. } => VideoImportPath::HardwareZeroCopy,
        VideoFrameTransferPath::SoftwareHostUpload => VideoImportPath::SoftwareHostUpload,
    }
}

/// Возможности fake: NV12 DMA-BUF и программный 4:2:0 8 бит; P010 не объявлен.
pub(super) fn fake_capabilities(backend: RenderBackendKind) -> RenderCapabilities {
    let mut capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    capabilities.backend = backend;
    capabilities.display_name = "fake renderer".to_owned();
    capabilities.supported_frame_contracts = vec![
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        VideoFrameContract::host_yuv420_planar8(),
    ];
    capabilities
}

impl RenderShell for FakeRenderShell {
    fn family(&self) -> RendererFamily {
        self.family
    }

    fn instance_id(&self) -> RendererInstanceId {
        self.id
    }

    fn capabilities(&self) -> &RenderCapabilities {
        &self.capabilities
    }

    fn diagnostics(&self) -> RenderDiagnostics {
        RenderDiagnostics::default()
    }

    fn resize(&mut self, _size: RenderSurfaceSize) {
        // У fake нет настоящего surface: буфер «подходит» к любому размеру.
    }

    fn reconfigure_presentation(
        &mut self,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        self.presentation.reconfigure(
            settings,
            self.defect == FakeDefect::ReconfigureNeverReportsNoOp,
        )
    }

    fn detach_presentation(&mut self) -> PresentationDetachOutcome {
        self.presentation.detach()
    }

    fn reattach_presentation(
        &mut self,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        self.presentation.reattach()
    }

    fn acquire_surface_frame(&mut self, _screen: RenderScreenDescriptor) -> SurfaceFrameAcquire {
        if let Err(reason) = self
            .presentation
            .check_acquire(self.defect == FakeDefect::AcquiresWhileDetached)
        {
            return SurfaceFrameAcquire::Dropped(reason);
        }
        SurfaceFrameAcquire::Acquired(AcquiredSurfaceFrame::new(
            self.id,
            FakeSurfaceBuffer,
            Duration::from_micros(250),
        ))
    }

    fn can_reuse_retained_ui(&self, screen: RenderScreenDescriptor) -> bool {
        self.retained_ui == Some(screen)
    }

    fn render_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome {
        let RenderFrameInput {
            surface,
            video_frame,
            egui,
            screen,
            ..
        } = input;
        let ui_frame = self.begin_ui_frame(egui);
        let acquire_elapsed = match surface.into_buffer::<FakeSurfaceBuffer>(self.id) {
            Ok(opened) => opened.acquire_elapsed,
            Err(_foreign) if self.defect == FakeDefect::AcceptsForeignSurface => Duration::ZERO,
            Err(_foreign) => {
                ui_frame.discard_on_rejection();
                return self.reject_frame(RenderFrameFailure::foreign_surface_frame());
            }
        };
        let video_handle = match video_frame {
            Some(key) => match key.prepared_for::<FakePreparedFrame>(self.id) {
                Some(prepared) => Some(prepared.handle),
                None => {
                    ui_frame.discard_on_rejection();
                    return self.reject_frame(RenderFrameFailure::foreign_video_frame());
                }
            },
            None => None,
        };
        let policy = self.missing_texture_policy();
        match self.ui_textures.finish_frame(ui_frame, policy) {
            FakeAppliedUi::Fresh(FakeUiTextureSync::OutOfSync) => {
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::UiTexturesOutOfSync);
            }
            FakeAppliedUi::ReusePrevious
                if self.retained_ui != Some(screen)
                    && self.defect != FakeDefect::ReusesWithoutRetainedUi =>
            {
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::RetainedUiUnavailable);
            }
            FakeAppliedUi::ReusePrevious => {}
            FakeAppliedUi::Fresh(FakeUiTextureSync::InSync) => {
                self.retained_ui = Some(screen);
            }
        }
        if let Some(handle) = video_handle {
            self.presented_video.push(handle);
        }
        if let (FakeDefect::RetainsKeys, Some(key)) = (self.defect, video_frame) {
            self.retained_keys.push(key.clone());
        }
        let stages = RenderFrameStageTimings {
            surface_acquire: acquire_elapsed,
            ..RenderFrameStageTimings::default()
        };
        RenderFrameOutcome::Presented(RenderFrameTiming::new(stages, acquire_elapsed))
    }

    fn live_settings(&self) -> RenderLiveSettings {
        self.live_settings.settings.clone()
    }

    fn live_settings_adapter(&mut self) -> &mut dyn RenderLiveSettingsAdapter {
        &mut self.live_settings
    }

    fn wait_for_gpu_idle(&mut self, _timeout: Duration) -> Result<(), RendererGpuDrainError> {
        self.gpu.complete_submitted_work();
        Ok(())
    }

    fn hot_path_stats(&self) -> RenderHotPathStats {
        RenderHotPathStats::default()
    }

    fn video_frames(&mut self) -> &mut dyn VideoFrameImport {
        self
    }

    fn video_import_preparer(&self) -> Box<dyn VideoImportPreparer> {
        // Дефектный подготовщик ставит на импорт паспорт несуществующего рендера.
        let owner = if self.defect == FakeDefect::PreparerStampsForeignOwner {
            RendererInstanceId::allocate()
        } else {
            self.id
        };
        Box::new(FakeVideoImportPreparer {
            owner,
            gpu: self.gpu.clone(),
        })
    }
}

/// Подготовщик fake-рендера: та же подготовка, что у склада, но в любом потоке.
struct FakeVideoImportPreparer {
    owner: RendererInstanceId,
    gpu: Arc<FakeGpu>,
}

impl VideoImportPreparer for FakeVideoImportPreparer {
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        Ok(prepare_fake_import(self.owner, &self.gpu, backend, plan))
    }
}

/// Оборачивает декодер так, что освобождения отправленных кадров ждут fake-«GPU».
fn prepare_fake_import(
    owner: RendererInstanceId,
    gpu: &Arc<FakeGpu>,
    backend: StartedVideoBackend,
    plan: VideoImportPlan,
) -> PreparedVideoImport {
    let backend_id = backend.backend_id().to_owned();
    let decoder_thread = backend.into_decoder_thread();
    let binding = FakeSubmissionBinding::new(gpu.clone());
    let renderer_provider = PresentFrameResourceProviderHandle::new(FakeSubmittedProvider {
        inner: decoder_thread.resource_provider(),
        binding: binding.clone(),
    });
    let wrapped = StartedVideoBackend::from_decoder_thread(
        backend_id,
        FakeWrappedDecoderThread {
            inner: decoder_thread,
            renderer_provider: renderer_provider.clone(),
        },
    );
    PreparedVideoImport {
        backend: wrapped,
        pending: PendingVideoImport::new(
            owner,
            plan.path,
            FakeImportState {
                path: plan.path,
                renderer_provider,
                binding,
            },
        ),
    }
}

impl VideoFrameImport for FakeRenderShell {
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        if !self.import_handles(plan.path) {
            return Err(VideoImportPrepareError::PathUnsupported { path: plan.path });
        }
        Ok(prepare_fake_import(self.id, &self.gpu, backend, plan))
    }

    fn install_video_import(
        &mut self,
        pending: PendingVideoImport,
    ) -> Result<(), VideoImportInstallError> {
        let state = pending.into_state::<FakeImportState>(self.id)?;
        state.binding.rebind(self.gpu.clone());
        self.active_import = Some(state);
        Ok(())
    }

    fn active_video_import_path(&self) -> Option<VideoImportPath> {
        self.active_import.as_ref().map(|active| active.path)
    }

    fn materialize(&mut self, lease: &VideoFrameLease) -> FrameMaterialization {
        let frame_contract = lease.decoded_frame().frame_contract;
        if !self.capabilities.supports_frame_contract(frame_contract) {
            let outcome = if self.defect == FakeDefect::ReportsUnsupportedAsError {
                FrameMaterializationOutcome::Error(FrameImportError::ImportFailed {
                    message: "unsupported".to_owned(),
                })
            } else {
                FrameMaterializationOutcome::Unsupported(
                    FrameImportUnsupportedReason::ContractNotDeclared { frame_contract },
                )
            };
            return FrameMaterialization::without_pool_wait(outcome);
        }
        match &self.active_import {
            Some(active) => self.materialize_with_active_import(lease, active),
            None if self.defect == FakeDefect::ReportsMissingBackendAsMissing => {
                FrameMaterialization::without_pool_wait(FrameMaterializationOutcome::Missing)
            }
            None => FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::NoVideoBackendBound,
            ),
        }
    }

    fn import_stats(&self) -> Option<FrameImportStats> {
        None
    }

    fn export_video_import_for_recreation(&self) -> Option<VideoImportHandoff> {
        self.active_import.as_ref().map(|active| {
            VideoImportHandoff::new(
                self.family,
                active.path,
                FakeImportState {
                    path: active.path,
                    renderer_provider: active.renderer_provider.clone(),
                    binding: active.binding.clone(),
                },
            )
        })
    }

    fn prepare_recreated_video_import(
        &mut self,
        handoff: VideoImportHandoff,
    ) -> Result<PendingVideoImport, VideoImportPrepareError> {
        let state = handoff.into_state::<FakeImportState>(self.family)?;
        Ok(PendingVideoImport::new(self.id, state.path, state))
    }

    fn release_after_device_lost(&mut self) -> Result<ReleasedFrameCount, VideoImportReleaseError> {
        if let Some(active) = &self.active_import {
            active.binding.mark_lost();
        }
        let released = if self.defect == FakeDefect::KeepsPendingAfterDeviceLost {
            self.gpu.release_without_clearing()
        } else {
            self.gpu.complete_submitted_work()
        };
        Ok(ReleasedFrameCount::new(released))
    }
}
