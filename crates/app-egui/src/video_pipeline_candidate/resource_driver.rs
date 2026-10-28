//! Concrete/fake-able preparation driver renderer-bound candidate pair-а.
//!
//! С сессии 02 плана `user/gles2-renderer/` production-драйвер не знает GPU-API: он
//! запускает декодер и отдаёт его подготовщику импорта рендера
//! (`render_shell_api::VideoImportPreparer`), полученному в потоке окна.

use std::fmt;
use std::num::NonZeroUsize;

use render_shell_api::{
    PendingVideoImport, RenderShell, RendererFamily, VideoImportPrepareError, VideoImportPreparer,
};
use video_backend_api::{DetachedVideoBackend, DetachedVideoBackendResourceError};
use video_core::{HostFrameMemoryKind, PresentPassSignal};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_vaapi::{VaapiVideoBackendFactory, ZeroCopyImportTarget};

use crate::video_pipeline_selector::{VideoBackendKind, VideoPipelinePlan};

/// Renderer-side materializer path, который обязан совпадать с decoder backend-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateVideoMaterializerKind {
    /// VA-API decoded resources materialize-ятся через DMA-BUF zero-copy.
    DmaBufZeroCopy,

    /// FFmpeg software frames materialize-ятся через HostPlanar upload.
    HostPlanarUpload,
}

/// Exact app composition plan без concrete GPU/decoder owner types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CandidateVideoPipelineDescriptor {
    /// Backend class защищает active/candidate pairing diagnostics.
    backend_kind: VideoBackendKind,

    /// Materializer class предотвращает DMA-BUF/HostPlanar mixing.
    materializer_kind: CandidateVideoMaterializerKind,
}

impl CandidateVideoPipelineDescriptor {
    /// Строит descriptor только из двух поддерживаемых concrete production plans.
    #[must_use]
    pub(crate) const fn from_plan(plan: VideoPipelinePlan) -> Self {
        // Match остаётся exhaustive при добавлении нового selectable plan-а.
        match plan {
            VideoPipelinePlan::VaapiDmaBuf { .. } => Self {
                backend_kind: VideoBackendKind::HardwareZeroCopy,
                materializer_kind: CandidateVideoMaterializerKind::DmaBufZeroCopy,
            },
            VideoPipelinePlan::FfmpegHostUpload { .. } => Self {
                backend_kind: VideoBackendKind::FfmpegSoftware,
                materializer_kind: CandidateVideoMaterializerKind::HostPlanarUpload,
            },
        }
    }

    /// Возвращает backend class без concrete factory knowledge.
    #[must_use]
    pub(crate) const fn backend_kind(self) -> VideoBackendKind {
        // Copy enum используется только для matching и diagnostics.
        self.backend_kind
    }

    /// Возвращает matching renderer materializer class.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn materializer_kind(self) -> CandidateVideoMaterializerKind {
        // Copy enum не раскрывает WGPU handles.
        self.materializer_kind
    }
}

/// Fallible preparation stage concrete candidate pair-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateVideoPipelinePreparationStage {
    /// Runtime/driver не может выдать временный второй decoder resource set.
    BackendResource,

    /// Concrete decoder factory не смогла запустить detached backend.
    BackendStartup,

    /// Рендер не смог подготовить импорт кадров запущенного decoder-а.
    RendererImport,
}

/// Typed preparation failure до передачи decoder half-а player owner-у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateVideoPipelinePreparationError {
    /// Exact stage не позволяет смешать startup/provider/materializer failures.
    stage: CandidateVideoPipelinePreparationStage,

    /// Диагностическое сообщение не используется для branching.
    message: String,
}

impl CandidateVideoPipelinePreparationError {
    /// Создаёт failure конкретного fallible preparation stage-а.
    #[must_use]
    pub(crate) fn at_stage(
        stage: CandidateVideoPipelinePreparationStage,
        message: impl Into<String>,
    ) -> Self {
        // BackendResource обязан создаваться отдельным constructor-ом с availability.
        debug_assert_ne!(
            stage,
            CandidateVideoPipelinePreparationStage::BackendResource
        );
        // Обычный stage не притворяется resource availability failure.
        Self {
            stage,
            message: message.into(),
        }
    }

    /// Переводит app-owned failure в neutral reply для player resource request-а.
    #[must_use]
    pub(super) fn to_resource_error(&self, backend_id: &str) -> DetachedVideoBackendResourceError {
        DetachedVideoBackendResourceError::StartupFailed {
            backend_id: backend_id.to_owned(),
            message: format!("{:?}: {}", self.stage, self.message),
        }
    }
}

impl fmt::Display for CandidateVideoPipelinePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Stage и message достаточно для app diagnostics без concrete error type-а.
        write!(formatter, "{:?}: {}", self.stage, self.message)
    }
}

impl std::error::Error for CandidateVideoPipelinePreparationError {}

/// Обе согласованные половины candidate resource set-а до split handoff.
pub(crate) struct PreparedCandidateVideoPipelineResources<RendererImport> {
    /// Detached player half нельзя установить до successful stream configuration.
    pub(super) detached_backend: DetachedVideoBackend,

    /// Renderer-bound app half: подготовленный импорт того же provider-а декодера.
    pub(super) renderer_import: RendererImport,
}

/// Fake-able owner boundary создания конкретной candidate pair.
pub(crate) trait CandidateVideoPipelineResourceDriver {
    /// Импорт рендера: production `PendingVideoImport` или focused fake.
    type RendererImport;

    /// Семейство рендера, под которое готовится импорт (часть диагностической метки плана).
    fn renderer_family(&self) -> RendererFamily;

    /// Создаёт один bounded pair, не получая ссылок на active AppState resources.
    fn prepare_candidate_resources(
        &mut self,
        plan: VideoPipelinePlan,
    ) -> Result<
        PreparedCandidateVideoPipelineResources<Self::RendererImport>,
        CandidateVideoPipelinePreparationError,
    >;
}

/// Что декодеру нужно знать о рендере, который будет показывать его кадры.
///
/// Берётся у активного рендера в потоке окна и переносится туда, где запускается декодер
/// (у кандидата нового файла — поток плеера).
#[derive(Clone, Debug)]
pub(crate) struct DecoderRendererLink {
    /// Семейство рендера: цель zero-copy импорта VA-API и вид памяти программных кадров.
    pub(crate) family: RendererFamily,

    /// Сигнал рендера «проход показа отправлен» — такт аппаратного декодера (gles2-renderer
    /// 12a.2). `None` — рендер сигнала не даёт (wgpu), декодер работает без такта.
    pub(crate) present_pass_signal: Option<PresentPassSignal>,
}

impl DecoderRendererLink {
    /// Связь с активным рендером.
    #[must_use]
    pub(crate) fn for_renderer(renderer: &dyn RenderShell) -> Self {
        Self {
            family: renderer.family(),
            present_pass_signal: renderer.present_pass_signal(),
        }
    }
}

/// Production driver: concrete decoder factory + подготовщик импорта активного рендера.
pub(crate) struct RendererCandidateVideoPipelineResourceDriver {
    /// Подготовщик импорта exact renderer instance-а (импорт чужого рендера не установится).
    preparer: Box<dyn VideoImportPreparer>,

    /// Связь декодера с рендером: семейство (и метка плана) и сигнал показа.
    renderer_link: DecoderRendererLink,
}

impl RendererCandidateVideoPipelineResourceDriver {
    /// Берёт подготовщик импорта у активного рендера (вызывается в потоке окна).
    #[must_use]
    pub(crate) fn new(renderer: &mut dyn RenderShell) -> Self {
        Self {
            preparer: renderer.video_import_preparer(),
            renderer_link: DecoderRendererLink::for_renderer(renderer),
        }
    }
}

impl CandidateVideoPipelineResourceDriver for RendererCandidateVideoPipelineResourceDriver {
    type RendererImport = PendingVideoImport;

    fn renderer_family(&self) -> RendererFamily {
        self.renderer_link.family
    }

    fn prepare_candidate_resources(
        &mut self,
        plan: VideoPipelinePlan,
    ) -> Result<
        PreparedCandidateVideoPipelineResources<Self::RendererImport>,
        CandidateVideoPipelinePreparationError,
    > {
        let started_backend =
            start_decoder_backend(plan, &self.renderer_link).map_err(|message| {
                CandidateVideoPipelinePreparationError::at_stage(
                    CandidateVideoPipelinePreparationStage::BackendStartup,
                    message,
                )
            })?;
        // Рендер оборачивает decoder: releases candidate provider-а ждут его очередь GPU.
        let prepared = self
            .preparer
            .prepare_video_import(started_backend, plan.import_plan())
            .map_err(|error| {
                CandidateVideoPipelinePreparationError::at_stage(
                    CandidateVideoPipelinePreparationStage::RendererImport,
                    describe_import_prepare_error(&error),
                )
            })?;

        // Split ещё не выполнен: все ресурсы освобождаются вместе при early error/drop.
        Ok(PreparedCandidateVideoPipelineResources {
            detached_backend: DetachedVideoBackend::from_started(prepared.backend),
            renderer_import: prepared.pending,
        })
    }
}

/// Запускает decoder backend маршрута декода плана (VA-API или FFmpeg).
///
/// Общий для кандидатов strong open и прямого пути `AppState::install_video_pipeline_plan`;
/// ошибка — текст factory без потери категории (её задаёт вызывающий). `renderer` — рендер,
/// который будет импортировать кадры: от его семейства зависит цель zero-copy импорта VA-API
/// (имя контракта в логе, по которому evidence доказывает аппаратный путь), от его сигнала
/// показа — такт аппаратного декодера.
pub(crate) fn start_decoder_backend(
    plan: VideoPipelinePlan,
    renderer: &DecoderRendererLink,
) -> Result<video_backend_api::StartedVideoBackend, String> {
    let decoder_thread_config = plan.decoder_thread_config();
    // Exhaustive match гарантирует, что каждый маршрут запускает свой decoder.
    match plan {
        // VA-API factory создаёт отдельный decoder thread/resource owner.
        VideoPipelinePlan::VaapiDmaBuf { .. } => {
            vaapi_factory_for_renderer(decoder_thread_config, renderer)
                .start_for_composition()
                .map_err(|error| error.to_string())
        }
        // FFmpeg factory создаёт отдельный software decoder thread/resource owner.
        VideoPipelinePlan::FfmpegHostUpload { .. } => {
            ffmpeg_factory_for_renderer(decoder_thread_config, renderer.family)
                .start_for_composition()
                .map_err(|error| error.to_string())
        }
    }
}

/// Цель zero-copy импорта VA-API для семейства рендера (exhaustive: новое семейство
/// не скомпилируется, пока для него не выбрана цель).
pub(crate) const fn zero_copy_import_target_for(family: RendererFamily) -> ZeroCopyImportTarget {
    match family {
        RendererFamily::WgpuVulkan => ZeroCopyImportTarget::VulkanDmaBuf,
        RendererFamily::Gles2 => ZeroCopyImportTarget::EglDmaBuf,
    }
}

/// VA-API factory под рендер `renderer`: цель импорта по семейству и такт по сигналу показа.
pub(crate) fn vaapi_factory_for_renderer(
    decoder_thread_config: player_core::PlayerVideoDecoderThreadConfig,
    renderer: &DecoderRendererLink,
) -> VaapiVideoBackendFactory {
    let factory = VaapiVideoBackendFactory::new_with_decoder_config(decoder_thread_config)
        .with_zero_copy_import_target(zero_copy_import_target_for(renderer.family));
    // Рендер без сигнала (wgpu) — декодер без такта, ровно как до gles2-renderer 12a.2.
    match &renderer.present_pass_signal {
        Some(signal) => factory.with_present_pass_signal(signal.clone()),
        None => factory,
    }
}

/// Вид памяти блоков кадров программного декодера для семейства рендера (exhaustive).
///
/// GLES2 импортирует блок через dma-buf из `/dev/udmabuf`, а он делается только из
/// memfd; wgpu-путь (08b) импортирует блок через amdgpu `userptr`, который принимает
/// только анонимную память, — её менять нельзя.
pub(crate) const fn host_frame_memory_for(family: RendererFamily) -> HostFrameMemoryKind {
    match family {
        RendererFamily::WgpuVulkan => HostFrameMemoryKind::Anonymous,
        RendererFamily::Gles2 => HostFrameMemoryKind::SealedMemfd,
    }
}

/// Размер пула поверхностей VA-API под OpenGL ES 2.0 (старое железо).
///
/// Решение владельца 69 (gles2-renderer 12a) вернуло 24 вместо 16 из решения 66: на T410 с
/// пулом 16 аппаратный 720p60 перерисовывал интерфейс на половине кадров (GPU 86 → 94 %,
/// главный поток +2 п.п.), а потери кадров не уменьшились. Почему — бэклог.
///
/// `unwrap` в `const` вычисляется при компиляции: ноль здесь — ошибка сборки, а не паника.
const GLES2_HARDWARE_SURFACE_POOL_FRAMES: NonZeroUsize = NonZeroUsize::new(24).unwrap();

/// Размер пула поверхностей VA-API под wgpu/Vulkan (прежний default проекта).
const WGPU_HARDWARE_SURFACE_POOL_FRAMES: NonZeroUsize = NonZeroUsize::new(24).unwrap();

/// Размер пула поверхностей hardware decoder-а для семейства рендера (exhaustive).
///
/// Подставляется вместо `0` («авто») в `video.decoder_surface_pool_frames` и
/// `video.zero_copy_surface_pool_slots`; явное число пользователя это значение не трогает.
/// Сейчас оба семейства получают 24 (решение 69 отменило 16 для GLES2 из решения 66); шов
/// оставлен, чтобы число для старого железа можно было менять в одном месте по замеру.
/// Декодер всё равно поднимает пул до минимума стрима.
/// Единственное место соответствия «семейство -> число»: остальной код получает уже
/// готовый `AutomaticSurfacePool`. Нижняя граница для валидации watermark-ов в config —
/// `fastiplayer_config::MIN_AUTOMATIC_SURFACE_POOL_FRAMES` (закреплена тестом).
pub(crate) const fn hardware_surface_pool_default_for(
    family: RendererFamily,
) -> player_core::AutomaticSurfacePool {
    match family {
        RendererFamily::WgpuVulkan => {
            player_core::AutomaticSurfacePool::new(WGPU_HARDWARE_SURFACE_POOL_FRAMES)
        }
        RendererFamily::Gles2 => {
            player_core::AutomaticSurfacePool::new(GLES2_HARDWARE_SURFACE_POOL_FRAMES)
        }
    }
}

/// Тот же выбор, когда семейство ещё не сообщено (`None`): считаем рендер wgpu/Vulkan,
/// то есть прежнее поведение. В продакшене семейство известно до создания worker-а.
pub(crate) const fn hardware_surface_pool_default_or_wgpu(
    family: Option<RendererFamily>,
) -> player_core::AutomaticSurfacePool {
    match family {
        Some(known_family) => hardware_surface_pool_default_for(known_family),
        None => hardware_surface_pool_default_for(RendererFamily::WgpuVulkan),
    }
}

/// FFmpeg software factory с видом памяти блоков под рендер `renderer_family`.
pub(crate) fn ffmpeg_factory_for_renderer(
    decoder_thread_config: player_core::PlayerVideoDecoderThreadConfig,
    renderer_family: RendererFamily,
) -> FfmpegSoftwareVideoBackendFactory {
    FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(decoder_thread_config)
        .with_host_frame_memory(host_frame_memory_for(renderer_family))
}

/// Текст отказа подготовки импорта для диагностики кандидата.
fn describe_import_prepare_error(error: &VideoImportPrepareError) -> String {
    match error {
        VideoImportPrepareError::PathUnsupported { path } => {
            format!("renderer does not support the {path:?} import path")
        }
        VideoImportPrepareError::HandoffFromAnotherFamily { handoff_family } => format!(
            "renderer import handoff came from family '{}'",
            handoff_family.stable_id()
        ),
        VideoImportPrepareError::ResourceCreationFailed { message } => {
            format!("renderer import resources were not created: {message}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gles2_uses_memfd_blocks_and_wgpu_keeps_anonymous() {
        assert_eq!(
            host_frame_memory_for(RendererFamily::Gles2),
            HostFrameMemoryKind::SealedMemfd
        );
        assert_eq!(
            host_frame_memory_for(RendererFamily::WgpuVulkan),
            HostFrameMemoryKind::Anonymous
        );
    }

    #[test]
    fn both_families_resolve_the_automatic_pool_to_24() {
        assert_eq!(
            hardware_surface_pool_default_for(RendererFamily::Gles2).frames(),
            24
        );
        assert_eq!(
            hardware_surface_pool_default_for(RendererFamily::WgpuVulkan).frames(),
            24
        );
        assert_eq!(hardware_surface_pool_default_or_wgpu(None).frames(), 24);
    }

    /// Валидация watermark-ов в config при `slots = 0` опирается на этот минимум.
    #[test]
    fn every_family_automatic_pool_respects_config_validation_floor() {
        for family in [RendererFamily::WgpuVulkan, RendererFamily::Gles2] {
            assert!(
                hardware_surface_pool_default_for(family).frames()
                    >= fastiplayer_config::MIN_AUTOMATIC_SURFACE_POOL_FRAMES,
                "{family:?} automatic pool is below the config validation floor"
            );
        }
    }

    /// Путь GLES2: «авто» из default config доходит до decoder config и import plan.
    #[test]
    fn gles2_automatic_pool_reaches_decoder_config() {
        let decoder_thread_config = player_core::PlayerWorkerConfig::from_app_config(
            &fastiplayer_config::AppConfig::default(),
            hardware_surface_pool_default_for(RendererFamily::Gles2),
        )
        .decoder_thread_config;

        assert_eq!(decoder_thread_config.decoder_surface_pool_frames, 24);
        assert_eq!(decoder_thread_config.zero_copy_surface_pool_slots, 24);
    }

    #[test]
    fn ffmpeg_factory_gets_memory_kind_of_its_renderer() {
        let config = player_core::PlayerVideoDecoderThreadConfig::default();

        let gles2 = ffmpeg_factory_for_renderer(config, RendererFamily::Gles2);
        let wgpu = ffmpeg_factory_for_renderer(config, RendererFamily::WgpuVulkan);

        assert_eq!(gles2.host_frame_memory(), HostFrameMemoryKind::SealedMemfd);
        assert_eq!(wgpu.host_frame_memory(), HostFrameMemoryKind::Anonymous);
    }
}
