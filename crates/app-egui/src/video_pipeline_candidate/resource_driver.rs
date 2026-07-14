//! Concrete/fake-able preparation driver renderer-bound candidate pair-а.

use std::fmt;
use std::sync::Arc;

use render_wgpu_video::{
    DmaBufWgpuFrameMaterializer, HostPlanarWgpuFrameMaterializer, WgpuFrameTextureViewMaterializer,
    WgpuSubmissionQueueBinding, wrap_video_backend_for_wgpu_submission,
};
use video_backend_api::{DetachedVideoBackend, DetachedVideoBackendResourceError};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_vaapi::VaapiVideoBackendFactory;

use crate::video_pipeline_selector::{VideoBackendKind, VideoPipelinePlan};

/// Renderer-side materializer path, который обязан совпадать с decoder backend-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateVideoMaterializerKind {
    /// VA-API decoded resources materialize-ятся через DMA-BUF zero-copy.
    DmaBufZeroCopy,

    /// FFmpeg software frames materialize-ятся через HostPlanar upload.
    HostPlanarUpload,
}

/// Exact app composition plan без concrete WGPU/decoder owner types.
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
            VideoPipelinePlan::VaapiDmaBufWgpu { .. } => Self {
                backend_kind: VideoBackendKind::HardwareZeroCopy,
                materializer_kind: CandidateVideoMaterializerKind::DmaBufZeroCopy,
            },
            VideoPipelinePlan::FfmpegHostUploadWgpu { .. } => Self {
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

    /// Renderer submission provider/binding preparation завершилась ошибкой.
    ProviderBinding,

    /// Matching renderer materializer не удалось создать.
    MaterializerCreation,
}

/// Availability-класс backend resource failure без destructive fallback policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateVideoBackendAvailability {
    /// Backend отсутствует или временно недоступен в runtime.
    Unavailable,

    /// Driver разрешает active decoder, но отвергает второй candidate decoder.
    ResourceExhausted,
}

/// Typed preparation failure до передачи decoder half-а player owner-у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateVideoPipelinePreparationError {
    /// Exact stage не позволяет смешать startup/provider/materializer failures.
    stage: CandidateVideoPipelinePreparationStage,

    /// Availability уточняется только для BackendResource stage.
    availability: Option<CandidateVideoBackendAvailability>,

    /// Диагностическое сообщение не используется для branching.
    message: String,
}

impl CandidateVideoPipelinePreparationError {
    /// Создаёт unavailable/resource-exhausted failure без fallback semantics.
    #[must_use]
    pub(crate) fn backend_resource(
        availability: CandidateVideoBackendAvailability,
        message: impl Into<String>,
    ) -> Self {
        // Typed availability сохраняется отдельно от human-readable diagnostics.
        Self {
            stage: CandidateVideoPipelinePreparationStage::BackendResource,
            availability: Some(availability),
            message: message.into(),
        }
    }

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
            availability: None,
            message: message.into(),
        }
    }

    /// Возвращает exact preparation stage для policy/diagnostics.
    #[must_use]
    pub(crate) const fn stage(&self) -> CandidateVideoPipelinePreparationStage {
        // Stage является главным typed discriminator-ом.
        self.stage
    }

    /// Переводит app-owned failure в neutral reply для player resource request-а.
    #[must_use]
    pub(super) fn to_resource_error(&self, backend_id: &str) -> DetachedVideoBackendResourceError {
        // Resource availability остаётся distinct от factory startup failure.
        match self.availability {
            Some(CandidateVideoBackendAvailability::Unavailable) => {
                DetachedVideoBackendResourceError::Unavailable {
                    reason: self.message.clone(),
                }
            }
            Some(CandidateVideoBackendAvailability::ResourceExhausted) => {
                DetachedVideoBackendResourceError::ResourceExhausted {
                    reason: self.message.clone(),
                }
            }
            None => DetachedVideoBackendResourceError::StartupFailed {
                backend_id: backend_id.to_owned(),
                message: format!("{:?}: {}", self.stage, self.message),
            },
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
pub(crate) struct PreparedCandidateVideoPipelineResources<Materializer, SubmissionBinding> {
    /// Detached player half нельзя установить до successful stream configuration.
    pub(super) detached_backend: DetachedVideoBackend,

    /// Renderer-bound app half materialize-ит resources того же provider-а.
    pub(super) materializer: Materializer,

    /// Submission binding сохраняет old/new release callback ordering.
    pub(super) submission_binding: SubmissionBinding,
}

/// Fake-able owner boundary создания конкретной candidate pair.
pub(crate) trait CandidateVideoPipelineResourceDriver {
    /// Materializer может быть production WGPU trait object-ом или focused fake-ом.
    type Materializer;

    /// Submission binding может быть production queue binding-ом или focused fake-ом.
    type SubmissionBinding;

    /// Создаёт один bounded pair, не получая ссылок на active AppState resources.
    fn prepare_candidate_resources(
        &mut self,
        plan: VideoPipelinePlan,
    ) -> Result<
        PreparedCandidateVideoPipelineResources<Self::Materializer, Self::SubmissionBinding>,
        CandidateVideoPipelinePreparationError,
    >;
}

/// Production driver сохраняет concrete backend/render knowledge в composition root-е.
pub(crate) struct WgpuCandidateVideoPipelineResourceDriver {
    /// WGPU instance принадлежит exact renderer generation.
    instance: wgpu::Instance,

    /// WGPU adapter принадлежит тому же renderer generation.
    adapter: wgpu::Adapter,

    /// WGPU device используется только во время fallible preparation.
    device: wgpu::Device,

    /// WGPU queue связывает submitted release callbacks exact renderer-а.
    queue: wgpu::Queue,
}

impl WgpuCandidateVideoPipelineResourceDriver {
    /// Создаёт owned ref-counted driver handles exact renderer generation-а.
    #[must_use]
    pub(crate) fn new(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Self {
        Self {
            instance: instance.clone(),
            adapter: adapter.clone(),
            device: device.clone(),
            queue: queue.clone(),
        }
    }
}

impl CandidateVideoPipelineResourceDriver for WgpuCandidateVideoPipelineResourceDriver {
    type Materializer = Arc<dyn WgpuFrameTextureViewMaterializer>;
    type SubmissionBinding = WgpuSubmissionQueueBinding;

    fn prepare_candidate_resources(
        &mut self,
        plan: VideoPipelinePlan,
    ) -> Result<
        PreparedCandidateVideoPipelineResources<Self::Materializer, Self::SubmissionBinding>,
        CandidateVideoPipelinePreparationError,
    > {
        // Exhaustive match гарантирует matching decoder/materializer path.
        match plan {
            VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            } => {
                // VA-API factory создаёт отдельный decoder thread/resource owner.
                let started_backend =
                    VaapiVideoBackendFactory::new_with_decoder_config(decoder_thread_config)
                        .start_for_composition()
                        .map_err(|error| {
                            CandidateVideoPipelinePreparationError::at_stage(
                                CandidateVideoPipelinePreparationStage::BackendStartup,
                                error.to_string(),
                            )
                        })?;
                // Wrapper связывает releases candidate provider-а только с candidate queue.
                let (wrapped_backend, resource_provider, submission_binding) =
                    wrap_video_backend_for_wgpu_submission(started_backend, &self.queue);
                // DMA-BUF materializer получает provider именно этого wrapped backend-а.
                let materializer: Arc<dyn WgpuFrameTextureViewMaterializer> =
                    Arc::new(DmaBufWgpuFrameMaterializer::new(
                        &self.instance,
                        &self.adapter,
                        &self.device,
                        resource_provider,
                    ));

                // Split ещё не выполнен: все ресурсы освобождаются вместе при early error/drop.
                Ok(PreparedCandidateVideoPipelineResources {
                    detached_backend: DetachedVideoBackend::from_started(wrapped_backend),
                    materializer,
                    submission_binding,
                })
            }
            VideoPipelinePlan::FfmpegHostUploadWgpu {
                decoder_thread_config,
            } => {
                // FFmpeg factory создаёт отдельный software decoder thread/resource owner.
                let started_backend = FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(
                    decoder_thread_config,
                )
                .start_for_composition()
                .map_err(|error| {
                    CandidateVideoPipelinePreparationError::at_stage(
                        CandidateVideoPipelinePreparationStage::BackendStartup,
                        error.to_string(),
                    )
                })?;
                // Тот же submission wrapper удерживает HostPlanar releases до queue callback.
                let (wrapped_backend, resource_provider, submission_binding) =
                    wrap_video_backend_for_wgpu_submission(started_backend, &self.queue);
                // HostPlanar materializer использует exact provider и queue этого pair-а.
                let materializer = Arc::new(HostPlanarWgpuFrameMaterializer::new(
                    &self.device,
                    &self.queue,
                    resource_provider,
                )) as Arc<dyn WgpuFrameTextureViewMaterializer>;

                // Candidate остаётся detached и не изменяет active software/hardware path.
                Ok(PreparedCandidateVideoPipelineResources {
                    detached_backend: DetachedVideoBackend::from_started(wrapped_backend),
                    materializer,
                    submission_binding,
                })
            }
        }
    }
}
