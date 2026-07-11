use super::*;

/// Typed failure controlled video-pipeline rebuild-а.
#[derive(Debug)]
pub(crate) enum VideoPipelineRebuildError {
    /// Новый backend/materializer не удалось подготовить до runtime mutation.
    Preparation(String),
    /// Worker отклонил commit или сообщил apply/rollback failure.
    Worker(player_core::PlayerRuntimeApplyError),
}

impl std::fmt::Display for VideoPipelineRebuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparation(message) => formatter.write_str(message),
            Self::Worker(error) => write!(formatter, "video backend worker commit failed: {error}"),
        }
    }
}

/// Именованный request controlled video-pipeline rebuild-а.
pub(crate) struct VideoPipelineRebuildRequest<'resource> {
    /// User policy выбора backend-а.
    pub(crate) backend_preference: rustiplayer_config::VideoBackendPreference,
    /// Lifecycle intent, определяющий retryable busy semantics.
    pub(crate) install_intent: player_core::PlayerVideoBackendInstallIntent,
    /// Queue/pool/thread config нового decoder-а.
    pub(crate) decoder_thread_config: PlayerVideoDecoderThreadConfig,
    /// Requirement текущего stream-а, если media уже активно.
    pub(crate) stream_requirement: Option<&'resource VideoDecodeRequirement>,
    /// WGPU instance для backend startup.
    pub(crate) instance: &'resource wgpu::Instance,
    /// WGPU adapter для materializer selection.
    pub(crate) adapter: &'resource wgpu::Adapter,
    /// WGPU device для materializer creation.
    pub(crate) device: &'resource wgpu::Device,
    /// WGPU queue для frame-resource bridge.
    pub(crate) queue: &'resource wgpu::Queue,
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

impl AppState {
    /// Инициализирует video pipeline и сохраняет WGPU materializer в shell layer-е.
    pub fn init_video_pipeline(
        &mut self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let decoder_thread_config = self.player_worker.decoder_thread_config();
        let backend_preference = self.committed_config_snapshot.video_backend_preference();
        if let Err(error) =
            self.rebuild_video_pipeline_with_decoder_config(VideoPipelineRebuildRequest {
                backend_preference,
                install_intent: player_core::PlayerVideoBackendInstallIntent::PipelineDemand,
                decoder_thread_config,
                stream_requirement: None,
                instance,
                adapter,
                device,
                queue,
            })
        {
            warn!(error = %error, "Video pipeline unavailable");
        }
    }

    /// Пересоздаёт concrete video backend через app-owned composition boundary.
    pub(crate) fn rebuild_video_pipeline_with_decoder_config(
        &mut self,
        request: VideoPipelineRebuildRequest<'_>,
    ) -> Result<(), VideoPipelineRebuildError> {
        let VideoPipelineRebuildRequest {
            backend_preference,
            install_intent,
            decoder_thread_config,
            stream_requirement,
            instance,
            adapter,
            device,
            queue,
        } = request;
        let plan = select_video_pipeline_plan(
            backend_preference,
            self.system_capabilities_snapshot.as_ref(),
            decoder_thread_config,
            stream_requirement,
        )
        .map_err(|error| {
            VideoPipelineRebuildError::Preparation(format!(
                "video pipeline selection failed: {error}"
            ))
        })?;
        let plan_label = plan.diagnostic_label();
        let plan_backend_kind = plan.backend_kind();

        let (player_backend, frame_materializer, submission_queue_binding): (
            player_core::StartedVideoBackend,
            Arc<dyn WgpuFrameTextureViewMaterializer>,
            WgpuSubmissionQueueBinding,
        ) = match plan {
            VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            } => {
                let backend_factory =
                    VaapiVideoBackendFactory::new_with_decoder_config(decoder_thread_config);
                let started_backend = backend_factory.start_for_composition().map_err(|error| {
                    VideoPipelineRebuildError::Preparation(format!(
                        "video backend startup failed for {plan_label}: {error}"
                    ))
                })?;
                let (player_backend, frame_resource_provider, submission_queue_binding) =
                    wrap_video_backend_for_wgpu_submission(started_backend, queue);
                let frame_materializer: Arc<dyn WgpuFrameTextureViewMaterializer> =
                    Arc::new(DmaBufWgpuFrameMaterializer::new(
                        instance,
                        adapter,
                        device,
                        frame_resource_provider,
                    ));

                (player_backend, frame_materializer, submission_queue_binding)
            }
            VideoPipelinePlan::FfmpegHostUploadWgpu {
                decoder_thread_config,
            } => {
                let backend_factory = FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(
                    decoder_thread_config,
                );
                let started_backend = backend_factory.start_for_composition().map_err(|error| {
                    VideoPipelineRebuildError::Preparation(format!(
                        "video backend startup failed for {plan_label}: {error}"
                    ))
                })?;
                let (player_backend, frame_resource_provider, submission_queue_binding) =
                    wrap_video_backend_for_wgpu_submission(started_backend, queue);
                let frame_materializer = Arc::new(HostPlanarWgpuFrameMaterializer::new(
                    device,
                    queue,
                    frame_resource_provider,
                ))
                    as Arc<dyn WgpuFrameTextureViewMaterializer>;

                (player_backend, frame_materializer, submission_queue_binding)
            }
        };

        let previous_backend_kind = self.current_video_backend_kind;

        if let Err(error) = self.player_worker.set_video_backend(
            player_backend,
            install_intent,
            (install_intent == player_core::PlayerVideoBackendInstallIntent::SettingsReconfigure)
                .then_some(decoder_thread_config),
        ) {
            return Err(VideoPipelineRebuildError::Worker(error));
        }

        self.clear_main_visual_override();
        self.wgpu_frame_materializer = Some(frame_materializer);
        self.wgpu_submission_queue_binding = Some(submission_queue_binding);

        // Живая смена backend-а (класс реально меняется): морозим последний кадр, пока
        // worker не переключится и не выдаст первый кадр нового backend-а, иначе кадры
        // старого backend-а уйдут в новый materializer → `Missing render resources`.
        if previous_backend_kind.is_some_and(|previous| previous != plan_backend_kind) {
            self.begin_backend_swap_video_freeze();
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
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
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
                // Нужный backend уже активен — player декодирует как есть, свап не требуется.
                if self.current_video_backend_kind == Some(plan.backend_kind()) {
                    return;
                }
                if let Err(error) =
                    self.rebuild_video_pipeline_with_decoder_config(VideoPipelineRebuildRequest {
                        backend_preference: preference,
                        install_intent:
                            player_core::PlayerVideoBackendInstallIntent::PipelineDemand,
                        decoder_thread_config,
                        stream_requirement: Some(&request.requirement),
                        instance,
                        adapter,
                        device,
                        queue,
                    })
                {
                    warn!(error = %error, "Не удалось переключить video backend под текущий стрим");
                    if !request.decodable_by_active_backend {
                        self.reject_pending_video_backend(format!(
                            "не удалось запустить совместимый backend: {error}"
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

    /// Сообщает worker-у, что совместимый backend для отложенного видео не найден.
    pub(super) fn reject_pending_video_backend(&self, reason: String) {
        if let Err(error) = self.player_worker.reject_pending_video_backend(reason) {
            warn!(error = %error, "Не удалось доставить reject pending video backend в worker");
        }
    }

    /// Возвращает WGPU materializer текущего concrete video backend-а.
    pub(crate) fn wgpu_frame_materializer(
        &self,
    ) -> Option<Arc<dyn WgpuFrameTextureViewMaterializer>> {
        self.wgpu_frame_materializer.clone()
    }

    /// Готовит materializer для candidate renderer-а без изменения active GPU path-а.
    pub(crate) fn prepare_materializer_for_renderer(
        &self,
        renderer: &render_wgpu_shell::Renderer,
    ) -> Option<Arc<dyn WgpuFrameTextureViewMaterializer>> {
        self.wgpu_frame_materializer.as_ref().map(|materializer| {
            materializer.recreate_for_renderer(
                renderer.instance(),
                renderer.adapter(),
                renderer.device(),
                renderer.queue(),
            )
        })
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

    /// Освобождает все visual leases/views, привязанные к старому WGPU device.
    pub(crate) fn release_renderer_bound_visual_state(&mut self) {
        self.clear_main_visual_override();
        self.finish_backend_swap_video_freeze();
        self.clear_cached_present_frame_for_runtime_drop();
    }

    /// Commit-ит materializer и completion queue нового renderer-а одним owner method-ом.
    pub(crate) fn commit_recreated_materializer(
        &mut self,
        materializer: Option<Arc<dyn WgpuFrameTextureViewMaterializer>>,
        queue: &wgpu::Queue,
    ) -> Result<(), WgpuSubmissionQueueRebindError> {
        if let Some(submission_queue_binding) = &self.wgpu_submission_queue_binding {
            submission_queue_binding.rebind(queue)?;
        }
        self.wgpu_frame_materializer = materializer;
        self.mark_pending_worker_redraw();
        Ok(())
    }

    /// Завершает submitted releases старого device-а после доказанного device lost.
    ///
    /// Exactly-once guards внутри queue binding-а не допускают double release, если
    /// поздний WGPU callback всё же будет доставлен.
    pub(crate) fn release_submitted_frames_after_device_lost(
        &self,
    ) -> Result<usize, WgpuSubmissionQueueRebindError> {
        self.wgpu_submission_queue_binding
            .as_ref()
            .map_or(Ok(0), WgpuSubmissionQueueBinding::release_after_device_lost)
    }

    /// Начинает заморозку последнего кадра на время живой смены backend-а.
    ///
    /// Фиксирует render generation момента свапа и копию последнего материализованного
    /// кадра старого backend-а (его texture views остаются валидны через Arc даже после
    /// дропа старого materializer-а), чтобы держать его на экране, пока worker не
    /// переключится и не выдаст первый кадр нового backend-а.
    pub(super) fn begin_backend_swap_video_freeze(&mut self) {
        self.backend_swap_from_generation = Some(self.last_player_snapshot.render_generation);
        self.backend_swap_frozen_frame = self.cached_renderable_present_frame.clone();
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
        let new_backend_frame_ready =
            worker_switched && player_snapshot.current_video_frame.is_some();
        let frozen_source_stale = self
            .backend_swap_frozen_frame
            .as_ref()
            .is_some_and(|frozen| {
                frozen.source_label.as_deref() != player_snapshot.source_label.as_deref()
            });

        if new_backend_frame_ready || frozen_source_stale {
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
            warn!(error = %error, "Не удалось отправить capability report в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }
}
