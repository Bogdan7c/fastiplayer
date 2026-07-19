use super::*;

/// Явный результат получения frame lease-а для render boundary.
pub enum PresentFrameAcquisition {
    /// Worker ещё не публиковал zero-copy frame для текущей media session.
    NoFrameYet,

    /// Renderer повторяет последний безопасно удерживаемый lease.
    ReusedPreviousFrame(VideoFrameLease),

    /// Renderer получил новый lease с другим generation/texture handle.
    NewFrameAcquired(VideoFrameLease),

    /// Кандидат был отвергнут, потому что принадлежит старому render generation.
    StaleFrameRejected,
}

/// Кадр, для которого уже получены WGPU texture views и удерживается правильный render lease.
#[derive(Clone)]
pub struct RenderablePresentFrame {
    /// Lease удерживает backend texture resource до завершения render-side использования.
    pub present_frame: VideoFrameLease,

    /// WGPU texture views соответствуют `present_frame` и не используются без его lease-а.
    pub texture_views: WgpuFrameTextureViews,
}

impl RenderablePresentFrame {
    /// Собирает renderable frame из lease-а и WGPU texture views одного decoded кадра.
    #[must_use]
    pub fn new(present_frame: VideoFrameLease, texture_views: WgpuFrameTextureViews) -> Self {
        Self {
            present_frame,
            texture_views,
        }
    }
}

/// Cached renderable frame вместе с media source identity.
#[derive(Clone)]
pub(super) struct CachedRenderablePresentFrame {
    /// Последний кадр, который точно прошёл WGPU texture view lookup.
    pub(super) renderable_frame: RenderablePresentFrame,

    /// Source label защищает от reuse после открытия другого media.
    pub(super) source_label: Option<String>,
}

/// Стабильная identity decoded кадра на renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PresentFrameIdentity {
    /// Поколение render resources, которому принадлежит texture handle.
    pub(super) render_generation: u64,

    /// Opaque handle backend texture resource.
    pub(super) resource_handle: video_core::FrameResourceHandle,

    /// Поколение decoded frame внутри текущего seek/decode lifecycle.
    pub(super) decoded_generation: u64,

    /// Presentation timestamp decoded frame-а.
    pub(super) pts: Duration,
}

impl PresentFrameIdentity {
    /// Создаёт identity из public lease fields без доступа к player pipeline.
    pub(super) fn from_decoded_frame(
        render_generation: u64,
        frame: &video_core::DecodedFrame,
    ) -> Self {
        Self {
            render_generation,
            resource_handle: frame.resource_handle,
            decoded_generation: frame.generation,
            pts: frame.pts,
        }
    }
}

/// Причина явного освобождения cached present frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CachedPresentFrameDiscardReason {
    /// Пользователь или shell начинает открывать другой media source.
    MediaOpenBoundary,

    /// Runtime с window/surface уничтожается целиком.
    RuntimeDrop,

    /// Player больше не держит текущий video frame для этой session.
    CurrentVideoFrameMissing,

    /// Cached frame относится к другому media source.
    SourceLabelChanged,

    /// Cached frame относится к старому render generation.
    RenderGenerationChanged,

    /// Swapchain/window lifecycle сделал cached texture небезопасной для удержания.
    SurfaceLifecycleBreak,

    /// Renderer/device path перешёл в fatal failure.
    RenderFailure,

    /// Worker сообщил render error вне текущего render call stack-а.
    WorkerRenderError,

    /// Player начал открытие media через event stream.
    PlayerMediaOpenRequested,

    /// Player завершил открытие media и source identity сменился на новую session.
    PlayerMediaOpened,

    /// Player остановил текущий media.
    PlayerStopped,

    /// Exact Clear receipt подтвердил полный player media reset.
    PlaylistClearReset,

    /// Player перешёл в failed state.
    PlayerFailed,

    /// Player завершает session.
    PlayerShutdownRequested,

    /// Player сообщил fatal media/runtime error.
    PlayerFatalError,
}

/// Данные для pure-проверки, остаётся ли cached frame валидным для текущего player snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CachedPresentFrameValidationState {
    /// Есть ли у player-а текущий video frame в этой session.
    pub(super) current_video_frame_present: bool,

    /// Совпадает ли media source cached frame-а с текущим source.
    pub(super) source_matches: bool,

    /// Render generation cached frame-а.
    pub(super) cached_generation: u64,

    /// Актуальное render generation из player snapshot-а.
    pub(super) current_generation: u64,
}

/// Возвращает причину invalidation, если cached frame нельзя больше reuse-ить.
pub(super) fn cached_present_frame_stale_reason(
    state: CachedPresentFrameValidationState,
) -> Option<CachedPresentFrameDiscardReason> {
    if !state.current_video_frame_present {
        return Some(CachedPresentFrameDiscardReason::CurrentVideoFrameMissing);
    }

    if !state.source_matches {
        return Some(CachedPresentFrameDiscardReason::SourceLabelChanged);
    }

    if state.cached_generation != state.current_generation {
        return Some(CachedPresentFrameDiscardReason::RenderGenerationChanged);
    }

    None
}

/// Мапит player event stream в cache lifecycle invalidation.
pub(super) fn cached_present_frame_discard_reason_for_player_event(
    player_event: &PlayerEvent,
) -> Option<CachedPresentFrameDiscardReason> {
    match player_event {
        PlayerEvent::MediaOpenRequested(_) => {
            Some(CachedPresentFrameDiscardReason::PlayerMediaOpenRequested)
        }
        PlayerEvent::MediaOpened(_) => Some(CachedPresentFrameDiscardReason::PlayerMediaOpened),
        PlayerEvent::PlaybackStateChanged(PlaybackState::Stopped) => {
            Some(CachedPresentFrameDiscardReason::PlayerStopped)
        }
        PlayerEvent::PlaybackStateChanged(PlaybackState::Failed) => {
            Some(CachedPresentFrameDiscardReason::PlayerFailed)
        }
        PlayerEvent::ShutdownRequested => {
            Some(CachedPresentFrameDiscardReason::PlayerShutdownRequested)
        }
        PlayerEvent::FatalError(_) => Some(CachedPresentFrameDiscardReason::PlayerFatalError),
        PlayerEvent::PlaybackStateChanged(_)
        | PlayerEvent::PositionChanged(_)
        | PlayerEvent::SeekRequested(_)
        | PlayerEvent::SeekTargetFramePresented(_)
        | PlayerEvent::SeekCommitted(_)
        | PlayerEvent::AudioResumedAfterSeek(_)
        | PlayerEvent::VideoFrameReady(_)
        | PlayerEvent::BufferingStateChanged(_)
        | PlayerEvent::CapabilityScanCompleted(_)
        | PlayerEvent::VideoTrackSelected(_)
        | PlayerEvent::AudioTrackSelected(_)
        | PlayerEvent::SubtitleTrackSelected(_)
        | PlayerEvent::QualitySelectionChanged(_)
        | PlayerEvent::ConfigReloadRequested
        | PlayerEvent::VideoBackendSelectionRequested(_)
        | PlayerEvent::RecoverableError(_) => None,
    }
}

/// Минимальная state-модель для проверки safe previous-frame reuse без GPU handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TextureBusyFallbackReuseState {
    /// Render generation cached frame-а.
    pub(super) cached_generation: u64,

    /// Актуальное render generation из player snapshot-а.
    pub(super) current_generation: u64,

    /// Совпадает ли media source cached frame-а с текущим source.
    pub(super) source_matches: bool,

    /// Есть ли у player-а текущий video frame для этой media session.
    pub(super) has_current_video_frame: bool,

    /// Был ли cached frame уже помечен stale при публикации lease-а.
    ///
    /// Для Busy fallback-а этот флаг не является lifecycle invalidation: stale frame
    /// остаётся только визуальной заглушкой и не должен засчитываться как seek landing.
    pub(super) cached_frame_is_stale: bool,

    /// Помечает ли session текущую картинку stale относительно seek/scrub состояния.
    ///
    /// Active seek/scrub может выставить этот флаг, пока decoder ещё не отдал target
    /// frame. Это не запрещает визуальный reuse, если lifecycle identity всё ещё валиден.
    pub(super) timeline_marks_frame_stale: bool,
}

impl TextureBusyFallbackReuseState {
    /// Возвращает `true`, если reuse будет только визуальной заглушкой для stale seek state.
    #[must_use]
    pub(super) fn carries_seek_stale_marker(self) -> bool {
        self.cached_frame_is_stale || self.timeline_marks_frame_stale
    }
}

/// Lifecycle-причина, по которой Busy fallback не имеет права повторить cached frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextureBusyFallbackRejectReason {
    /// Cached frame относится к старому render generation.
    RenderGenerationChanged,

    /// Cached frame был получен для другого media source.
    SourceLabelChanged,

    /// Player snapshot больше не содержит текущий video frame.
    CurrentVideoFrameMissing,
}

/// Возвращает lifecycle-причину отказа Busy fallback-а или `None`, если reuse безопасен.
pub(super) fn texture_busy_fallback_reject_reason(
    state: TextureBusyFallbackReuseState,
) -> Option<TextureBusyFallbackRejectReason> {
    if state.cached_generation != state.current_generation {
        return Some(TextureBusyFallbackRejectReason::RenderGenerationChanged);
    }
    if !state.source_matches {
        return Some(TextureBusyFallbackRejectReason::SourceLabelChanged);
    }
    if !state.has_current_video_frame {
        return Some(TextureBusyFallbackRejectReason::CurrentVideoFrameMissing);
    }

    None
}

/// Решает, можно ли использовать previous renderable frame при busy texture lock-е.
#[cfg(test)]
pub(super) fn texture_busy_fallback_can_reuse_previous_frame(
    state: TextureBusyFallbackReuseState,
) -> bool {
    texture_busy_fallback_reject_reason(state).is_none()
}

impl PresentFrameAcquisition {
    /// Возвращает frame lease, если acquisition state разрешает rendering video.
    #[must_use]
    pub fn into_present_frame(self) -> Option<VideoFrameLease> {
        match self {
            Self::ReusedPreviousFrame(present_frame) | Self::NewFrameAcquired(present_frame) => {
                Some(present_frame)
            }
            Self::NoFrameYet | Self::StaleFrameRejected => None,
        }
    }

    /// Возвращает `true`, если render tick повторно использует предыдущий frame.
    #[must_use]
    pub const fn reused_previous_frame(&self) -> bool {
        matches!(self, Self::ReusedPreviousFrame(_))
    }

    /// Стабильное имя state для trace diagnostics.
    #[must_use]
    pub const fn metric_name(&self) -> &'static str {
        match self {
            Self::NoFrameYet => "no_frame_yet",
            Self::ReusedPreviousFrame(_) => "reused_previous_frame",
            Self::NewFrameAcquired(_) => "new_frame_acquired",
            Self::StaleFrameRejected => "stale_frame_rejected",
        }
    }
}

impl AppState {
    /// Пытается получить текущий video frame для renderer-а.
    #[must_use]
    pub fn acquire_present_frame_for_render(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> PresentFrameAcquisition {
        let rejected_stale_cached_frame = self.drop_stale_cached_present_frame(player_snapshot);

        if let Some(mut present_frame) = self.player_worker.try_acquire_present_frame() {
            if present_frame.render_generation() != player_snapshot.render_generation {
                self.clear_cached_present_frame(
                    CachedPresentFrameDiscardReason::RenderGenerationChanged,
                );
                return PresentFrameAcquisition::StaleFrameRejected;
            }

            if player_snapshot.timeline.stale_frame {
                present_frame.mark_timeline_stale();
            }
            let cached_frame_identity =
                self.cached_renderable_present_frame
                    .as_ref()
                    .map(|cached_frame| {
                        Self::present_frame_identity(&cached_frame.renderable_frame.present_frame)
                    });
            let acquired_frame_identity = Self::present_frame_identity(&present_frame);

            if cached_frame_identity == Some(acquired_frame_identity) {
                return self
                    .cached_renderable_present_frame
                    .as_ref()
                    .map(|cached_frame| cached_frame.renderable_frame.present_frame.clone())
                    .map(|mut cached_present_frame| {
                        if present_frame.is_stale() {
                            cached_present_frame.mark_timeline_stale();
                        }
                        PresentFrameAcquisition::ReusedPreviousFrame(cached_present_frame)
                    })
                    .unwrap_or(PresentFrameAcquisition::NoFrameYet);
            }

            return PresentFrameAcquisition::NewFrameAcquired(present_frame);
        }

        if let Some(mut cached_present_frame) = self
            .cached_renderable_present_frame
            .as_ref()
            .map(|cached_frame| cached_frame.renderable_frame.present_frame.clone())
        {
            if player_snapshot.timeline.stale_frame {
                cached_present_frame.mark_timeline_stale();
            }
            return PresentFrameAcquisition::ReusedPreviousFrame(cached_present_frame);
        }

        if rejected_stale_cached_frame {
            PresentFrameAcquisition::StaleFrameRejected
        } else {
            PresentFrameAcquisition::NoFrameYet
        }
    }

    /// Сбрасывает cached frame, когда он уже не принадлежит текущему media/render поколению.
    pub(super) fn drop_stale_cached_present_frame(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> bool {
        let Some(cached_renderable_frame) = &self.cached_renderable_present_frame else {
            return false;
        };

        let validation_state = CachedPresentFrameValidationState {
            current_video_frame_present: player_snapshot.current_video_frame.is_some(),
            source_matches: cached_renderable_frame.source_label.as_deref()
                == player_snapshot.source_label.as_deref(),
            cached_generation: cached_renderable_frame
                .renderable_frame
                .present_frame
                .render_generation(),
            current_generation: player_snapshot.render_generation,
        };
        let Some(reason) = cached_present_frame_stale_reason(validation_state) else {
            return false;
        };
        let rejected_generation =
            reason == CachedPresentFrameDiscardReason::RenderGenerationChanged;

        self.clear_cached_present_frame(reason);
        rejected_generation
    }

    /// Возвращает identity frame-а, достаточную для отличия нового lease-а от reuse.
    pub(super) fn present_frame_identity(present_frame: &VideoFrameLease) -> PresentFrameIdentity {
        PresentFrameIdentity::from_decoded_frame(
            present_frame.render_generation(),
            present_frame.decoded_frame(),
        )
    }

    /// Освобождает cached present frame и отправляет drop-ack worker-у через lease guard.
    pub(super) fn clear_cached_present_frame(&mut self, reason: CachedPresentFrameDiscardReason) {
        if self.cached_renderable_present_frame.is_some() {
            debug!(?reason, "Clearing cached present frame");
        }
        self.cached_renderable_present_frame = None;
    }

    /// Освобождает cached frame перед уничтожением app/window runtime.
    pub fn clear_cached_present_frame_for_runtime_drop(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::RuntimeDrop);
    }

    /// Освобождает cached frame после swapchain/surface lifecycle break-а.
    pub fn clear_cached_present_frame_after_surface_lifecycle_break(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::SurfaceLifecycleBreak);
    }

    /// Освобождает cached frame после renderer/device failure.
    pub fn clear_cached_present_frame_after_render_failure(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::RenderFailure);
    }

    /// Освобождает cached frame после worker-side render error event-а.
    pub fn clear_cached_present_frame_after_worker_render_error(&mut self) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::WorkerRenderError);
    }

    /// Синхронизирует cache lifecycle с событиями player state machine.
    pub fn handle_cached_present_frame_player_event(&mut self, player_event: &PlayerEvent) {
        let Some(reason) = cached_present_frame_discard_reason_for_player_event(player_event)
        else {
            return;
        };

        self.clear_cached_present_frame(reason);
    }

    /// Запоминает последний frame, который реально получил texture views.
    pub fn remember_renderable_present_frame(
        &mut self,
        renderable_frame: RenderablePresentFrame,
        player_snapshot: &PlayerSnapshot,
    ) {
        self.cached_renderable_present_frame = Some(CachedRenderablePresentFrame {
            renderable_frame,
            source_label: player_snapshot.source_label.clone(),
        });
    }

    /// Возвращает previous renderable frame для busy fallback, если lifecycle всё ещё валиден.
    #[must_use]
    pub fn reusable_renderable_frame_for_texture_busy(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<RenderablePresentFrame> {
        self.drop_stale_cached_present_frame(player_snapshot);

        let cached_renderable_frame = self.cached_renderable_present_frame.as_ref()?;
        let reuse_state = TextureBusyFallbackReuseState {
            cached_generation: cached_renderable_frame
                .renderable_frame
                .present_frame
                .render_generation(),
            current_generation: player_snapshot.render_generation,
            source_matches: cached_renderable_frame.source_label.as_deref()
                == player_snapshot.source_label.as_deref(),
            has_current_video_frame: player_snapshot.current_video_frame.is_some(),
            cached_frame_is_stale: cached_renderable_frame
                .renderable_frame
                .present_frame
                .is_stale(),
            timeline_marks_frame_stale: player_snapshot.timeline.stale_frame,
        };

        if let Some(reject_reason) = texture_busy_fallback_reject_reason(reuse_state) {
            debug!(
                ?reject_reason,
                "Texture view Busy fallback rejected cached renderable frame"
            );
            return None;
        }
        if reuse_state.carries_seek_stale_marker() {
            debug!(
                cached_frame_is_stale = reuse_state.cached_frame_is_stale,
                timeline_marks_frame_stale = reuse_state.timeline_marks_frame_stale,
                "Texture view Busy fallback reusing stale cached renderable frame as visual placeholder"
            );
        }

        Some(cached_renderable_frame.renderable_frame.clone())
    }

    /// Передаёт typed render bridge error в worker-owned player session.
    pub fn report_render_error(&mut self, error: PlayerRenderError) {
        if let Err(send_error) = self.player_worker.report_render_error(error) {
            warn!(error = %send_error, "Не удалось отправить typed render error в worker");
            return;
        }

        self.mark_pending_worker_redraw();
    }

    /// Передаёт renderer submit/present timing в player diagnostics без render-side business logic.
    pub fn report_gpu_submit_present_latency(&self, submit_present_elapsed: Duration) {
        self.player_worker
            .report_gpu_submit_present_latency(submit_present_elapsed);
    }

    /// Передаёт player diagnostics событие reuse previous frame-а из-за busy resource lock-а.
    pub fn report_render_resource_previous_frame_reuse(&self) {
        self.player_worker
            .report_render_resource_previous_frame_reuse();
    }

    /// Забирает worker events для shell telemetry.
    #[must_use]
    pub fn drain_worker_events(&mut self) -> Vec<PlayerWorkerEvent> {
        self.player_worker.drain_events()
    }
}
