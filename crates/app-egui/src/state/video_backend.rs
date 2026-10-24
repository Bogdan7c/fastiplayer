use super::*;
use crate::video_pipeline_candidate::start_decoder_backend;

/// Typed failure controlled video-pipeline rebuild-а.
#[derive(Debug)]
pub(crate) enum VideoPipelineRebuildError {
    /// Новый backend/materializer не удалось подготовить до runtime mutation.
    Preparation(String),
    /// Concrete backend factory не смог запустить выбранный plan.
    BackendStartup {
        /// Stable diagnostic label выбранного plan-а.
        plan_label: &'static str,
        /// Backend-owned startup detail без стирания failure category.
        message: String,
    },
    /// Worker отклонил commit или сообщил apply/rollback failure.
    Worker(player_core::PlayerRuntimeApplyError),
}

impl std::fmt::Display for VideoPipelineRebuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparation(message) => formatter.write_str(message),
            Self::BackendStartup {
                plan_label,
                message,
            } => write!(
                formatter,
                "video backend startup failed for {plan_label}: {message}"
            ),
            Self::Worker(error) => write!(formatter, "video backend worker commit failed: {error}"),
        }
    }
}

/// Куда ставится импорт кадров нового decoder-а: склад активного рендера и его семейство.
///
/// С сессии 02 плана `user/gles2-renderer/` пересборке пайплайна не нужны GPU-объекты: рендер
/// сам оборачивает decoder и создаёт материализатор (`VideoFrameImport`).
pub(crate) struct RendererVideoImportTarget<'renderer> {
    /// Семейство рендера (диагностическая метка плана).
    pub(crate) renderer_family: render_shell_api::RendererFamily,
    /// Склад кадров активного рендера.
    pub(crate) video_frames: &'renderer mut dyn render_shell_api::VideoFrameImport,
}

impl<'renderer> RendererVideoImportTarget<'renderer> {
    /// Цель импорта активного рендера.
    pub(crate) fn for_renderer(renderer: &'renderer mut dyn render_shell_api::RenderShell) -> Self {
        Self {
            renderer_family: renderer.family(),
            video_frames: renderer.video_frames(),
        }
    }
}

/// Именованный request controlled video-pipeline rebuild-а.
///
/// Два времени жизни: требование стрима живёт у владельца стрима, склад кадров — у рендера.
pub(crate) struct VideoPipelineRebuildRequest<'requirement, 'renderer> {
    /// User policy выбора backend-а.
    pub(crate) backend_preference: fastiplayer_config::VideoBackendPreference,
    /// Lifecycle intent, определяющий retryable busy semantics.
    pub(crate) install_intent: player_core::PlayerVideoBackendInstallIntent,
    /// Queue/pool/thread config нового decoder-а.
    pub(crate) decoder_thread_config: PlayerVideoDecoderThreadConfig,
    /// Requirement текущего stream-а, если media уже активно.
    pub(crate) stream_requirement: Option<&'requirement VideoDecodeRequirement>,
    /// Склад кадров активного рендера, куда ставится импорт нового decoder-а.
    pub(crate) import_target: RendererVideoImportTarget<'renderer>,
}

/// Решение video-пайплайна на время живой смены backend-а.
pub(crate) enum BackendSwapVideoPhase<'frame> {
    /// Свап не идёт — обычный путь acquire/materialize.
    NotSwapping,

    /// Worker ещё не переключился или не выдал первый кадр нового backend-а: держим
    /// замороженный кадр (или ничего, если кэша не было) и НЕ материализуем кадры
    /// старого backend-а новым materializer-ом.
    HoldFrozenFrame(Option<&'frame RenderablePresentFrame>),
}

/// Pre-barrier snapshot визуального состояния старого video pipeline-а.
///
/// Checkpoint хранит весь renderable frame, поэтому texture path и SDR/HDR metadata
/// переходят границу вместе и не смешиваются с новым materializer-ом.
pub(crate) struct BackendSwapVideoCheckpoint {
    from_generation: u64,
    frozen_frame: Option<CachedRenderablePresentFrame>,
}

/// Новый visual frame совместим с committed pipeline только после смены generation.
fn backend_swap_new_frame_ready(
    from_generation: u64,
    current_generation: u64,
    current_video_frame_present: bool,
) -> bool {
    current_generation != from_generation && current_video_frame_present
}

/// Проверяет, что уже установленный video pipeline действительно обслуживает запрос.
///
/// Совпадения класса backend-а недостаточно: при compatibility media install player
/// сначала снимает старый decoder, а затем просит shell установить decoder того же
/// класса уже под новый stream. Источник истины о фактической готовности decoder-а —
/// `VideoBackendSelectionRequest`, а не сохранённый shell-ом enum backend-а.
fn active_video_pipeline_satisfies_reselection(
    current_backend_kind: Option<VideoBackendKind>,
    selected_plan: VideoPipelinePlan,
    request: &VideoBackendSelectionRequest,
) -> bool {
    current_backend_kind == Some(selected_plan.backend_kind())
        && request.decodable_by_active_backend
}

impl AppState {
    /// Инициализирует video pipeline и ставит импорт кадров decoder-а в активный рендер.
    pub fn init_video_pipeline(&mut self, import_target: RendererVideoImportTarget<'_>) {
        let decoder_thread_config = self.player_worker.decoder_thread_config();
        let backend_preference = self.committed_config_snapshot.video_backend_preference();
        if let Err(error) =
            self.rebuild_video_pipeline_with_decoder_config(VideoPipelineRebuildRequest {
                backend_preference,
                install_intent: player_core::PlayerVideoBackendInstallIntent::PipelineDemand,
                decoder_thread_config,
                stream_requirement: None,
                import_target,
            })
        {
            warn!(error = %error, "Video pipeline unavailable");
        }
    }

    /// Пересоздаёт concrete video backend через app-owned composition boundary.
    pub(crate) fn rebuild_video_pipeline_with_decoder_config(
        &mut self,
        request: VideoPipelineRebuildRequest<'_, '_>,
    ) -> Result<(), VideoPipelineRebuildError> {
        let plan = select_video_pipeline_plan(
            request.backend_preference,
            self.system_capabilities_snapshot.as_ref(),
            request.decoder_thread_config,
            request.stream_requirement,
        )
        .map_err(|error| {
            VideoPipelineRebuildError::Preparation(format!(
                "video pipeline selection failed: {error}"
            ))
        })?;

        self.install_video_pipeline_plan(plan, request.install_intent, request.import_target)
    }

    /// Запускает уже выбранный plan и атомарно устанавливает decoder + импорт рендера.
    ///
    /// Второй путь установки decoder-а (кроме кандидатов strong open): старт приложения,
    /// реактивная смена backend-а, runtime fallback DMA-BUF → SW, пересборка из настроек.
    fn install_video_pipeline_plan(
        &mut self,
        plan: VideoPipelinePlan,
        install_intent: player_core::PlayerVideoBackendInstallIntent,
        import_target: RendererVideoImportTarget<'_>,
    ) -> Result<(), VideoPipelineRebuildError> {
        let plan_label = plan.diagnostic_label(import_target.renderer_family);
        let plan_backend_kind = plan.backend_kind();
        let decoder_thread_config = plan.decoder_thread_config();

        let started_backend = start_decoder_backend(plan).map_err(|message| {
            VideoPipelineRebuildError::BackendStartup {
                plan_label,
                message,
            }
        })?;
        // Рендер оборачивает decoder (releases ждут его GPU) и готовит импорт «в стороне».
        let prepared = import_target
            .video_frames
            .prepare_video_import(started_backend, plan.import_plan())
            .map_err(|error| {
                VideoPipelineRebuildError::Preparation(format!(
                    "renderer could not prepare the video import for {plan_label}: {error:?}"
                ))
            })?;

        let previous_backend_kind = self.current_video_backend_kind;
        let backend_swap_checkpoint = previous_backend_kind
            .filter(|previous| *previous != plan_backend_kind)
            .map(|_| self.capture_backend_swap_video_checkpoint());

        self.player_worker
            .set_video_backend(
                prepared.backend,
                install_intent,
                (install_intent
                    == player_core::PlayerVideoBackendInstallIntent::SettingsReconfigure)
                    .then_some(decoder_thread_config),
            )
            .map_err(VideoPipelineRebuildError::Worker)?;

        self.clear_main_visual_override();
        // Импорт только что подготовлен этим же рендером, поэтому отказ установки — нарушение
        // инварианта, а не рабочая ветка; он всё равно типизирован и не глотается.
        import_target
            .video_frames
            .install_video_import(prepared.pending)
            .map_err(|error| {
                tracing::error!(error = ?error, plan = plan_label, "Renderer rejected its own video import");
                VideoPipelineRebuildError::Preparation(format!(
                    "renderer rejected the prepared video import for {plan_label}: {error:?}"
                ))
            })?;

        // Живая смена backend-а: сохраняем последний кадр до первого кадра нового
        // generation, а worker-owned install boundary сохраняет position/playback intent.
        if let Some(checkpoint) = backend_swap_checkpoint {
            self.begin_backend_swap_video_freeze(checkpoint);
        }
        self.current_video_backend_kind = Some(plan_backend_kind);
        info!(plan = plan_label, "Selected video pipeline");
        self.mark_pending_worker_redraw();
        Ok(())
    }

    /// Запоминает последний запрос player-core на подбор backend-а под активный стрим.
    pub(crate) fn note_video_backend_reselection_request(
        &mut self,
        request: VideoBackendSelectionRequest,
    ) {
        // Requirement активного стрима живёт дольше одного кадра: его использует
        // live-смена настроек, чтобы пересобрать pipeline под реальный кодек/профиль.
        self.active_video_stream_requirement = Some(request.requirement.clone());
        self.pending_video_backend_reselection = Some(request);
    }

    /// Requirement активного video-стрима, известный shell-у на текущий момент.
    pub(crate) fn active_video_stream_requirement(&self) -> Option<&VideoDecodeRequirement> {
        self.active_video_stream_requirement.as_ref()
    }

    /// Забирает отложенный запрос на подбор backend-а для обработки в текущем кадре.
    pub(crate) fn take_pending_video_backend_reselection(
        &mut self,
    ) -> Option<VideoBackendSelectionRequest> {
        self.pending_video_backend_reselection.take()
    }

    /// Подбирает backend под текущий стрим по committed preference и при необходимости
    /// бесшовно переключает video pipeline; если нужный класс backend-а запрещён политикой
    /// и видео отложено — отклоняет его, сохраняя typed unsupported error.
    pub(crate) fn apply_video_backend_reselection(
        &mut self,
        request: &VideoBackendSelectionRequest,
        import_target: RendererVideoImportTarget<'_>,
    ) {
        let decoder_thread_config = self.current_decoder_thread_config();
        let preference = self.committed_config_snapshot.video_backend_preference();
        match select_video_pipeline_plan(
            preference,
            self.system_capabilities_snapshot.as_ref(),
            decoder_thread_config,
            Some(&request.requirement),
        ) {
            Ok(plan) => {
                // Сохраняем текущий pipeline только когда совпадает его класс И player
                // подтверждает наличие совместимого активного decoder-а. После media
                // install класс может не измениться, хотя старый decoder уже снят.
                if active_video_pipeline_satisfies_reselection(
                    self.current_video_backend_kind,
                    plan,
                    request,
                ) {
                    return;
                }
                if let Err(error) =
                    self.rebuild_video_pipeline_with_decoder_config(VideoPipelineRebuildRequest {
                        backend_preference: preference,
                        install_intent:
                            player_core::PlayerVideoBackendInstallIntent::PipelineDemand,
                        decoder_thread_config,
                        stream_requirement: Some(&request.requirement),
                        import_target,
                    })
                {
                    warn!(error = %error, "Could not switch the video backend to the current stream");
                    if !request.decodable_by_active_backend {
                        self.reject_pending_video_backend(format!(
                            "Failed to start compatible backend: {error}"
                        ));
                    }
                }
            }
            Err(error) => {
                // Текущий preference не допускает backend для этого стрима (например
                // hardware + AV1). Если видео отложено — отклоняем, иначе оставляем как есть.
                if !request.decodable_by_active_backend {
                    self.reject_pending_video_backend(error.to_string());
                }
            }
        }
    }

    /// Ставит typed runtime layout rejection в очередь до app composition boundary.
    pub(crate) fn note_dma_buf_layout_rejection(
        &mut self,
        layout_rejection: video_core::DmaBufDescriptorRejection,
        render_generation: u64,
        player_error: PlayerRenderError,
    ) {
        if self.pending_dma_buf_layout_rejection.is_none() {
            self.pending_dma_buf_layout_rejection = Some(PendingDmaBufLayoutRejection {
                layout_rejection,
                render_generation,
                player_error,
            });
        }
    }

    /// Начинает новый exactly-once recovery scope при открытии другого media.
    pub(crate) fn reset_dma_buf_runtime_fallback_for_new_media(&mut self) {
        self.dma_buf_runtime_fallback.reset_for_new_media();
        self.pending_dma_buf_layout_rejection = None;
    }

    /// Выполняет exactly-once auto fallback либо возвращает исходную typed layout
    /// rejection вместе с typed policy/startup/restore failure context.
    pub(crate) fn apply_pending_dma_buf_runtime_fallback(
        &mut self,
        import_target: RendererVideoImportTarget<'_>,
    ) -> Result<(), Box<DmaBufRuntimeFallbackFailure>> {
        let Some(pending_rejection) = self.pending_dma_buf_layout_rejection.take() else {
            return Ok(());
        };
        let PendingDmaBufLayoutRejection {
            layout_rejection,
            render_generation,
            mut player_error,
        } = pending_rejection;

        if !matches!(
            layout_rejection,
            video_core::DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. }
        ) {
            let policy_error =
                DmaBufRuntimeFallbackError::FallbackNotApplicable { layout_rejection };
            player_error.message = policy_error.to_string();
            return Err(Box::new(DmaBufRuntimeFallbackFailure {
                error: policy_error,
                player_error,
            }));
        }

        let decoder_thread_config = self.current_decoder_thread_config();
        let stream_requirement = self.active_video_stream_requirement.as_ref();
        let software_plan = select_confirmed_software_fallback_plan(
            self.system_capabilities_snapshot.as_ref(),
            decoder_thread_config,
            stream_requirement,
        );
        let original_layout_rejection = layout_rejection.clone();
        let plan = match self.dma_buf_runtime_fallback.begin(
            self.committed_config_snapshot.video_backend_preference(),
            render_generation,
            layout_rejection,
            software_plan,
        ) {
            Ok(plan) => plan,
            Err(policy_error) => {
                player_error.message = policy_error.to_string();
                return Err(Box::new(DmaBufRuntimeFallbackFailure {
                    error: policy_error,
                    player_error,
                }));
            }
        };

        match self.install_video_pipeline_plan(
            plan,
            player_core::PlayerVideoBackendInstallIntent::PipelineDemand,
            import_target,
        ) {
            Ok(()) => {
                info!(
                    render_generation,
                    "Runtime DMA-BUF layout rejected; controlled software fallback applied"
                );
                Ok(())
            }
            Err(fallback_failure) => {
                let policy_error = DmaBufRuntimeFallbackController::fallback_failed(
                    original_layout_rejection,
                    fallback_failure,
                );
                player_error.message = policy_error.to_string();
                Err(Box::new(DmaBufRuntimeFallbackFailure {
                    error: policy_error,
                    player_error,
                }))
            }
        }
    }

    /// Сообщает worker-у, что совместимый backend для отложенного видео не найден.
    pub(super) fn reject_pending_video_backend(&self, reason: String) {
        if let Err(error) = self.player_worker.reject_pending_video_backend(reason) {
            warn!(error = %error, "Failed to deliver reject pending video backend into worker");
        }
    }

    /// Проверяет, что candidate renderer сохраняет контракт уже установленного backend-а.
    ///
    /// Renderer recreation не имеет права молча превратить DMA-BUF path в HostPlanar
    /// или наоборот: такой переход требует отдельного transactional backend rebuild-а.
    pub(crate) fn validate_renderer_candidate_capabilities(
        &self,
        capabilities: &SystemCapabilities,
    ) -> Result<(), VideoPipelineRebuildError> {
        let Some(current_backend_kind) = self.current_video_backend_kind else {
            return Ok(());
        };
        let candidate_plan = select_video_pipeline_plan(
            self.video_backend_preference(),
            Some(capabilities),
            self.current_decoder_thread_config(),
            self.active_video_stream_requirement.as_ref(),
        )
        .map_err(|error| {
            VideoPipelineRebuildError::Preparation(format!(
                "candidate renderer incompatible with active video requirement: {error}"
            ))
        })?;
        let candidate_backend_kind = candidate_plan.backend_kind();
        if candidate_backend_kind != current_backend_kind {
            return Err(VideoPipelineRebuildError::Preparation(format!(
                "candidate renderer requires backend transition {current_backend_kind:?} -> {candidate_backend_kind:?}"
            )));
        }
        Ok(())
    }

    /// Освобождает все visual leases/ключи кадров, привязанные к старому рендеру.
    pub(crate) fn release_renderer_bound_visual_state(&mut self) {
        self.clear_main_visual_override();
        self.finish_backend_swap_video_freeze();
        self.clear_cached_present_frame_for_runtime_drop();
    }

    /// Снимает visual checkpoint до player install barrier-а.
    ///
    /// После barrier-а snapshot уже может содержать новую generation, поэтому старую
    /// identity нельзя вычислять в post-Installed callback-е.
    pub(super) fn capture_backend_swap_video_checkpoint(&self) -> BackendSwapVideoCheckpoint {
        BackendSwapVideoCheckpoint {
            from_generation: self.last_player_snapshot.render_generation,
            frozen_frame: self.cached_renderable_present_frame.clone(),
        }
    }

    /// Активирует заранее снятый checkpoint после смены backend/materializer pointers.
    pub(super) fn begin_backend_swap_video_freeze(
        &mut self,
        checkpoint: BackendSwapVideoCheckpoint,
    ) {
        self.backend_swap_from_generation = Some(checkpoint.from_generation);
        self.backend_swap_frozen_frame = checkpoint.frozen_frame;
    }

    /// Завершает заморозку: новый backend выдал кадр или источник сменился.
    pub(super) fn finish_backend_swap_video_freeze(&mut self) {
        self.backend_swap_from_generation = None;
        self.backend_swap_frozen_frame = None;
    }

    /// Определяет, что показывать видео-пайплайну во время живой смены backend-а.
    ///
    /// Пока `render_generation` не превысил зафиксированный на свапе — worker ещё на
    /// старом backend-е, и его кадры несовместимы с новым materializer-ом, поэтому
    /// держим замороженный кадр. После переключения worker-а ждём первый реальный кадр
    /// нового backend-а (`current_video_frame`), и только тогда выходим в обычный путь.
    /// Смена источника тоже завершает заморозку, чтобы кадр прошлого media не залипал.
    pub(crate) fn backend_swap_video_phase(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> BackendSwapVideoPhase<'_> {
        let Some(from_generation) = self.backend_swap_from_generation else {
            return BackendSwapVideoPhase::NotSwapping;
        };

        let worker_switched = player_snapshot.render_generation != from_generation;
        let new_backend_frame_ready = backend_swap_new_frame_ready(
            from_generation,
            player_snapshot.render_generation,
            player_snapshot.current_video_frame.is_some(),
        );
        let switched_to_audio_only = worker_switched
            && !player_snapshot
                .tracks
                .iter()
                .any(|track| track.kind == media_core::TrackKind::Video);
        let frozen_source_stale = self
            .backend_swap_frozen_frame
            .as_ref()
            .is_some_and(|frozen| {
                frozen.source_label.as_deref() != player_snapshot.source_label.as_deref()
            });

        if new_backend_frame_ready || switched_to_audio_only || frozen_source_stale {
            self.finish_backend_swap_video_freeze();
            return BackendSwapVideoPhase::NotSwapping;
        }

        BackendSwapVideoPhase::HoldFrozenFrame(
            self.backend_swap_frozen_frame
                .as_ref()
                .map(|frozen| &frozen.renderable_frame),
        )
    }

    /// Сохраняет capability report для app-owned selector-а и передаёт clone в worker.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        self.system_capabilities_snapshot = Some(capabilities.clone());

        if let Err(error) = self.player_worker.set_system_capabilities(capabilities) {
            warn!(error = %error, "Could not send the capability report to the worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }
}

#[cfg(test)]
mod tests {
    use codec_core::{VideoCodec, VideoDecodeRequirement};
    use player_core::VideoBackendSelectionRequest;

    use super::{
        PlayerVideoDecoderThreadConfig, VideoBackendKind, VideoPipelinePlan,
        active_video_pipeline_satisfies_reselection, backend_swap_new_frame_ready,
    };

    #[test]
    fn pre_barrier_generation_releases_freeze_on_first_installed_frame() {
        assert!(backend_swap_new_frame_ready(6, 7, true));
        assert!(!backend_swap_new_frame_ready(6, 7, false));

        // Захват уже post-Installed generation воспроизводит бесконечный black freeze.
        assert!(!backend_swap_new_frame_ready(7, 7, true));
    }

    /// Воспроизводит lifecycle второго HLS/TS media: shell всё ещё помнит VA-API,
    /// но player уже снял decoder предыдущего media и ждёт новый decoder того же класса.
    #[test]
    fn same_backend_kind_without_active_decoder_requires_pipeline_rebuild() {
        let selected_plan = VideoPipelinePlan::VaapiDmaBuf {
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        };
        let request_without_active_decoder = VideoBackendSelectionRequest {
            requirement: VideoDecodeRequirement::new(VideoCodec::H264),
            decodable_by_active_backend: false,
        };

        assert!(!active_video_pipeline_satisfies_reselection(
            Some(VideoBackendKind::HardwareZeroCopy),
            selected_plan,
            &request_without_active_decoder,
        ));
    }

    /// Не допускает лишний rebuild, когда backend совпадает и player подтверждает,
    /// что его активный decoder уже обслуживает текущий stream.
    #[test]
    fn same_backend_kind_with_compatible_active_decoder_is_reused() {
        let selected_plan = VideoPipelinePlan::VaapiDmaBuf {
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        };
        let request_with_active_decoder = VideoBackendSelectionRequest {
            requirement: VideoDecodeRequirement::new(VideoCodec::H264),
            decodable_by_active_backend: true,
        };

        assert!(active_video_pipeline_satisfies_reselection(
            Some(VideoBackendKind::HardwareZeroCopy),
            selected_plan,
            &request_with_active_decoder,
        ));
    }
}
