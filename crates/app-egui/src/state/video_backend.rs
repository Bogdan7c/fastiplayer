use super::*;

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
        if let Err(error) = self.rebuild_video_pipeline_with_decoder_config(
            decoder_thread_config,
            None,
            instance,
            adapter,
            device,
            queue,
        ) {
            warn!(error = %error, "Video pipeline unavailable");
        }
    }

    /// Пересоздаёт concrete video backend через app-owned composition boundary.
    pub(crate) fn rebuild_video_pipeline_with_decoder_config(
        &mut self,
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
        stream_requirement: Option<&VideoDecodeRequirement>,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), String> {
        let plan = select_video_pipeline_plan(
            self.committed_config_snapshot.video_backend_preference(),
            self.system_capabilities_snapshot.as_ref(),
            decoder_thread_config,
            stream_requirement,
        )
        .map_err(|error| format!("video pipeline selection failed: {error}"))?;
        let plan_label = plan.diagnostic_label();
        let plan_backend_kind = plan.backend_kind();

        let (player_backend, frame_materializer): (
            player_core::StartedVideoBackend,
            Arc<dyn WgpuFrameTextureViewMaterializer>,
        ) = match plan {
            VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            } => {
                let backend_factory =
                    VaapiVideoBackendFactory::new_with_decoder_config(decoder_thread_config);
                let started_backend = backend_factory.start_for_composition().map_err(|error| {
                    format!("video backend startup failed for {plan_label}: {error}")
                })?;
                let (player_backend, frame_resource_provider) =
                    wrap_video_backend_for_wgpu_submission(started_backend, queue);
                let frame_materializer: Arc<dyn WgpuFrameTextureViewMaterializer> =
                    Arc::new(DmaBufWgpuFrameMaterializer::new(
                        instance,
                        adapter,
                        device,
                        frame_resource_provider,
                    ));

                (player_backend, frame_materializer)
            }
            VideoPipelinePlan::FfmpegHostUploadWgpu {
                decoder_thread_config,
            } => {
                let backend_factory = FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(
                    decoder_thread_config,
                );
                let started_backend = backend_factory.start_for_composition().map_err(|error| {
                    format!("video backend startup failed for {plan_label}: {error}")
                })?;
                let (player_backend, frame_resource_provider) =
                    wrap_video_backend_for_wgpu_submission(started_backend, queue);
                let frame_materializer = Arc::new(HostPlanarWgpuFrameMaterializer::new(
                    device,
                    queue,
                    frame_resource_provider,
                ))
                    as Arc<dyn WgpuFrameTextureViewMaterializer>;

                (player_backend, frame_materializer)
            }
        };

        let previous_backend_kind = self.current_video_backend_kind;
        let hover_budget_diagnostics_provider = player_backend.hover_budget_diagnostics_provider();

        if let Err(error) = self.player_worker.set_video_backend(player_backend) {
            return Err(format!("video backend command delivery failed: {error}"));
        }

        self.clear_main_visual_override();
        self.timeline_hover_prepare_controller
            .cancel_active_span(TimelineHoverPrepareCancellationReason::BackendSwitched);
        self.timeline_hover_preview_render_state.clear();
        self.wgpu_frame_materializer = Some(frame_materializer);

        // Живая смена backend-а (класс реально меняется): морозим последний кадр, пока
        // worker не переключится и не выдаст первый кадр нового backend-а, иначе кадры
        // старого backend-а уйдут в новый materializer → `Missing render resources`.
        if previous_backend_kind.is_some_and(|previous| previous != plan_backend_kind) {
            self.begin_backend_swap_video_freeze();
        }
        self.current_video_backend_kind = Some(plan_backend_kind);
        self.current_hover_budget_diagnostics_provider = hover_budget_diagnostics_provider;
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
                if let Err(error) = self.rebuild_video_pipeline_with_decoder_config(
                    decoder_thread_config,
                    Some(&request.requirement),
                    instance,
                    adapter,
                    device,
                    queue,
                ) {
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
