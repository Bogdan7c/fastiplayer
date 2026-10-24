//! Controlled transactional recreation renderer/device/surface lifecycle-а.
//!
//! Модуль живёт в app composition layer-е: только здесь одновременно доступны окно,
//! слот рендера и intent-boundary `AppState`. Settings/UI не получают GPU handles.
//!
//! С сессии 02 плана `user/gles2-renderer/` рендер — `Box<dyn RenderShell>`: новый экземпляр
//! того же семейства создаёт каталог (`render_backend::create_renderer`), импорт кадров
//! переносится контрактом (`export_video_import_for_recreation` → `prepare_recreated_video_import`
//! → `install_video_import` после ожидания старого GPU).

use std::sync::Arc;
use std::time::Duration;

use fastiplayer_config::RenderProfile;
use fastiplayer_settings::{
    AppRouteApplyResult, RenderCommittedSettingsUpdate, RendererRecreationApplyError,
    RendererRecreationApplyErrorKind, RendererRecreationRollbackError,
    RendererRecreationRollbackErrorKind, SettingStateOwner, SettingsApplyFailure,
    SettingsBoundaryActivity,
};
use render_shell_api::{PendingVideoImport, RenderShell, RenderSurfaceSize, RendererGpuDrainError};
use winit::window::Window;

use crate::render_backend::{RendererCreateRequest, RendererInitialColor, create_renderer};
use crate::state::AppState;
use crate::system_capabilities::probe_system_capabilities;

/// Bounded wait старой queue: event loop не должен зависнуть навсегда при driver failure.
const GPU_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Сериализует settings recreation с resize/fullscreen/surface lifecycle events.
#[derive(Debug, Default)]
pub(crate) struct RendererLifecycleCoordinator {
    /// Текущая non-interruptible shell operation; hidden queue намеренно отсутствует.
    activity: Option<SettingsBoundaryActivity>,

    /// Surface event уже выбран этим UI tick-ом и должен примениться первым.
    surface_event_pending: bool,
}

impl RendererLifecycleCoordinator {
    /// Read-only preflight for a settings transaction before any owner is mutated.
    pub(crate) fn settings_recreation_activity(&self) -> Option<SettingsBoundaryActivity> {
        if self.surface_event_pending || self.activity.is_some() {
            Some(SettingsBoundaryActivity::RendererLifecycle)
        } else {
            None
        }
    }

    /// Помечает resize/maximize/minimize action текущего UI tick-а.
    pub(crate) fn set_surface_event_pending(&mut self, pending: bool) {
        self.surface_event_pending = pending;
    }

    /// Выполняет обычный resize под тем же lifecycle boundary.
    pub(crate) fn resize_renderer(
        &mut self,
        renderer: &mut dyn RenderShell,
        width: u32,
        height: u32,
    ) {
        self.activity = Some(SettingsBoundaryActivity::RendererLifecycle);
        renderer.resize(RenderSurfaceSize { width, height });
        self.activity = None;
    }

    /// Запускает одну controlled recreation transaction без hidden retry queue.
    pub(crate) fn recreate<L>(
        &mut self,
        lifecycle: &mut L,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult
    where
        L: RendererRecreationLifecycle,
    {
        if self.surface_event_pending {
            return AppRouteApplyResult::RuntimeBusy {
                activity: SettingsBoundaryActivity::RendererLifecycle,
            };
        }
        if let Some(activity) = self.activity {
            return AppRouteApplyResult::RuntimeBusy { activity };
        }
        if let Some(activity) = lifecycle.preflight_activity() {
            return AppRouteApplyResult::RuntimeBusy { activity };
        }

        self.activity = Some(SettingsBoundaryActivity::RendererLifecycle);
        let result = Self::run_transaction(lifecycle, previous, next);
        self.activity = None;
        result
    }

    /// Выполняет ordered prepare -> release -> drain -> commit sequence.
    fn run_transaction<L>(
        lifecycle: &mut L,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult
    where
        L: RendererRecreationLifecycle,
    {
        let candidate = match lifecycle.prepare_candidate(next) {
            Ok(candidate) => candidate,
            Err(apply_error) => {
                return restore_after_failure(lifecycle, previous, apply_error);
            }
        };

        lifecycle.release_renderer_bound_visual_state();

        if let Err(apply_error) = lifecycle.drain_old_gpu_work() {
            drop(candidate);
            return restore_after_failure(lifecycle, previous, apply_error);
        }

        if let Err(apply_error) = lifecycle.commit_candidate(candidate) {
            return restore_after_failure(lifecycle, previous, apply_error);
        }

        lifecycle.resume_submissions();
        AppRouteApplyResult::Applied
    }
}

/// Минимальный lifecycle protocol, который fake tests проверяют без реального GPU.
pub(crate) trait RendererRecreationLifecycle {
    /// Полностью подготовленный, но ещё не active renderer path.
    type Candidate;

    /// Возвращает занятую внешнюю boundary до первой мутации.
    fn preflight_activity(&self) -> Option<SettingsBoundaryActivity>;

    /// Создаёт candidate resources, не меняя old runtime.
    fn prepare_candidate(
        &mut self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<Self::Candidate, RendererRecreationApplyError>;

    /// Освобождает renderer-bound leases/views перед ожиданием старой queue.
    fn release_renderer_bound_visual_state(&mut self);

    /// Дожидается callbacks и завершения старой GPU work.
    fn drain_old_gpu_work(&mut self) -> Result<(), RendererRecreationApplyError>;

    /// Делает готовый candidate active; partial mutation при Err запрещена.
    fn commit_candidate(
        &mut self,
        candidate: Self::Candidate,
    ) -> Result<(), RendererRecreationApplyError>;

    /// Пытается восстановить именно предыдущую конфигурацию.
    fn restore_previous(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        apply_error: &RendererRecreationApplyError,
    ) -> Result<(), RendererRecreationRollbackError>;

    /// Возобновляет submissions только после proven active path-а.
    fn resume_submissions(&mut self);
}

/// Всегда вызывает concrete restore boundary и сохраняет обе typed errors при failure.
fn restore_after_failure<L>(
    lifecycle: &mut L,
    previous: &RenderCommittedSettingsUpdate,
    apply_error: RendererRecreationApplyError,
) -> AppRouteApplyResult
where
    L: RendererRecreationLifecycle,
{
    match lifecycle.restore_previous(previous, &apply_error) {
        Ok(()) => {
            lifecycle.resume_submissions();
            AppRouteApplyResult::RendererRecreationFailed {
                failure: SettingsApplyFailure::ApplyFailed {
                    owner: SettingStateOwner::RendererLifecycle,
                    error: apply_error,
                },
            }
        }
        Err(rollback_error) => AppRouteApplyResult::RendererRecreationFailed {
            failure: SettingsApplyFailure::ApplyAndRollbackFailed {
                apply_owner: SettingStateOwner::RendererLifecycle,
                apply_error,
                rollback_owner: SettingStateOwner::RendererLifecycle,
                rollback_error,
            },
        },
    }
}

/// Candidate renderer path, подготовленный до release старых frame leases.
pub(crate) struct PreparedRendererCandidate {
    /// Новый рендер того же семейства (device/surface/video/egui).
    renderer: Box<dyn RenderShell>,

    /// Импорт кадров того же active decoder provider-а на ресурсах нового рендера.
    video_import: Option<PendingVideoImport>,

    /// Capability snapshot candidate-а, commit-имый вместе с renderer-ом.
    system_capabilities: capability_core::SystemCapabilities,
}

/// Production adapter одного frame-а; concrete GPU internals не выходят в settings/UI.
pub(crate) struct LiveRendererRecreation<'runtime> {
    /// Window владеет native surface target lifecycle-ом.
    window: Arc<Window>,

    /// Active renderer slot, которым владеет shell (commit заменяет рендер в нём).
    renderer: &'runtime mut Box<dyn RenderShell>,

    /// AppState владеет leases и neutral backend/materializer intent state.
    app_state: &'runtime mut AppState,

    /// Live render snapshot должен пережить device recreation.
    live_settings: render_core::RenderLiveSettings,
}

impl<'runtime> LiveRendererRecreation<'runtime> {
    /// Создаёт adapter без передачи GPU ownership в `AppState`.
    pub(crate) fn new(
        window: Arc<Window>,
        renderer: &'runtime mut Box<dyn RenderShell>,
        app_state: &'runtime mut AppState,
    ) -> Self {
        let live_settings = renderer.live_settings();
        Self {
            window,
            renderer,
            app_state,
            live_settings,
        }
    }

    /// Строит renderer path для указанного committed config-а.
    fn build_candidate(
        &mut self,
        settings: &RenderCommittedSettingsUpdate,
    ) -> Result<PreparedRendererCandidate, RendererRecreationApplyError> {
        if settings.profile == RenderProfile::OpenGles {
            return Err(apply_error(
                RendererRecreationApplyErrorKind::UnsupportedProfile,
                "OpenGL ES renderer is absent; the active Vulkan renderer was preserved",
            ));
        }

        let mut renderer = create_renderer(
            self.renderer.family(),
            RendererCreateRequest {
                window: self.window.clone(),
                vulkan: &settings.vulkan,
                initial_color: RendererInitialColor::RendererDefaults,
            },
        )
        .map_err(|error| {
            apply_error(
                RendererRecreationApplyErrorKind::CandidateCreation,
                format!("candidate renderer creation failed: {}", error.message),
            )
        })?;
        renderer
            .live_settings_adapter()
            .commit_live_settings(&self.live_settings)
            .map_err(|error| {
                apply_error(
                    RendererRecreationApplyErrorKind::CandidatePreparation,
                    format!("candidate live settings restore failed: {error}"),
                )
            })?;

        // Импорт активного decoder-а готовится на новом рендере «в стороне»: старый рендер
        // продолжает показывать кадры до commit-а.
        let video_import = match self
            .renderer
            .video_frames()
            .export_video_import_for_recreation()
        {
            Some(handoff) => Some(
                renderer
                    .video_frames()
                    .prepare_recreated_video_import(handoff)
                    .map_err(|error| {
                        apply_error(
                            RendererRecreationApplyErrorKind::CandidatePreparation,
                            format!("candidate video import preparation failed: {error:?}"),
                        )
                    })?,
            ),
            None => None,
        };
        let system_capabilities = probe_system_capabilities(renderer.capabilities().clone());
        self.app_state
            .validate_renderer_candidate_capabilities(&system_capabilities)
            .map_err(|error| {
                apply_error(
                    RendererRecreationApplyErrorKind::CandidatePreparation,
                    error.to_string(),
                )
            })?;

        Ok(PreparedRendererCandidate {
            renderer,
            video_import,
            system_capabilities,
        })
    }

    /// Commit-ит candidate так, чтобы fallible queue rebind произошёл до assignments.
    ///
    /// Установка импорта на новом рендере перепривязывает отложенные освобождения к его
    /// очереди; это делается только после ожидания старого GPU (`drain_old_gpu_work`).
    fn install_candidate(
        &mut self,
        candidate: PreparedRendererCandidate,
    ) -> Result<(), RendererRecreationApplyError> {
        let PreparedRendererCandidate {
            mut renderer,
            video_import,
            system_capabilities,
        } = candidate;
        if let Some(video_import) = video_import {
            renderer
                .video_frames()
                .install_video_import(video_import)
                .map_err(|error| {
                    apply_error(
                        RendererRecreationApplyErrorKind::Commit,
                        format!("candidate materializer queue commit failed: {error:?}"),
                    )
                })?;
        }
        self.app_state.set_system_capabilities(system_capabilities);
        *self.renderer = renderer;
        self.app_state.advance_renderer_generation();
        self.app_state.mark_pending_worker_redraw();
        Ok(())
    }
}

impl RendererRecreationLifecycle for LiveRendererRecreation<'_> {
    type Candidate = PreparedRendererCandidate;

    fn preflight_activity(&self) -> Option<SettingsBoundaryActivity> {
        None
    }

    fn prepare_candidate(
        &mut self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<Self::Candidate, RendererRecreationApplyError> {
        self.build_candidate(next)
    }

    fn release_renderer_bound_visual_state(&mut self) {
        self.app_state.release_renderer_bound_visual_state();
    }

    fn drain_old_gpu_work(&mut self) -> Result<(), RendererRecreationApplyError> {
        self.renderer
            .wait_for_gpu_idle(GPU_DRAIN_TIMEOUT)
            .map_err(renderer_drain_apply_error)
    }

    fn commit_candidate(
        &mut self,
        candidate: Self::Candidate,
    ) -> Result<(), RendererRecreationApplyError> {
        self.install_candidate(candidate)
    }

    fn restore_previous(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        apply_error: &RendererRecreationApplyError,
    ) -> Result<(), RendererRecreationRollbackError> {
        match apply_error.kind {
            RendererRecreationApplyErrorKind::DeviceLost => {
                self.renderer
                    .video_frames()
                    .release_after_device_lost()
                    .map_err(|error| RendererRecreationRollbackError {
                        kind: RendererRecreationRollbackErrorKind::DeviceLost,
                        message: format!(
                            "submitted frame release recovery after device lost failed: {error:?}"
                        ),
                    })?;
                let restored_candidate = self.build_candidate(previous).map_err(|error| {
                    RendererRecreationRollbackError {
                        kind: RendererRecreationRollbackErrorKind::DeviceLost,
                        message: format!(
                            "old configuration recreation after device lost failed: {}",
                            error.message
                        ),
                    }
                })?;
                self.install_candidate(restored_candidate).map_err(|error| {
                    RendererRecreationRollbackError {
                        kind: RendererRecreationRollbackErrorKind::ResourceRestore,
                        message: format!("old renderer commit failed: {}", error.message),
                    }
                })
            }
            RendererRecreationApplyErrorKind::GpuDrain => self
                .renderer
                .wait_for_gpu_idle(GPU_DRAIN_TIMEOUT)
                .map(|_| ())
                .map_err(|error| RendererRecreationRollbackError {
                    kind: RendererRecreationRollbackErrorKind::ResourceRestore,
                    message: format!("old GPU queue restore drain failed: {error}"),
                }),
            _ => Ok(()),
        }
    }

    fn resume_submissions(&mut self) {
        self.app_state.mark_pending_worker_redraw();
    }
}

/// Сохраняет typed distinction device-lost vs обычный drain failure.
fn renderer_drain_apply_error(error: RendererGpuDrainError) -> RendererRecreationApplyError {
    let kind = if matches!(error, RendererGpuDrainError::DeviceLost(_)) {
        RendererRecreationApplyErrorKind::DeviceLost
    } else {
        RendererRecreationApplyErrorKind::GpuDrain
    };
    apply_error(kind, error.to_string())
}

/// Создаёт typed apply error без повторения field boilerplate.
fn apply_error(
    kind: RendererRecreationApplyErrorKind,
    message: impl Into<String>,
) -> RendererRecreationApplyError {
    RendererRecreationApplyError {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Materializer path, который fake lifecycle переносит на candidate device.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeMaterializerPath {
        NoActiveVideo,
        DmaBuf,
        HostPlanar,
    }

    /// Наблюдаемые lifecycle steps для проверки release/drain/commit порядка.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeLifecycleEvent {
        Prepare(FakeMaterializerPath),
        ReleaseActiveLease,
        ReleaseVisualState,
        DrainGpuWork,
        Commit(FakeMaterializerPath),
        RestorePrevious,
        ResumeSubmissions,
    }

    /// Fake owner воспроизводит failures без WGPU/device зависимости тестов.
    struct FakeRendererLifecycle {
        materializer_path: FakeMaterializerPath,
        active_lease: bool,
        activity: Option<SettingsBoundaryActivity>,
        prepare_error: Option<RendererRecreationApplyError>,
        drain_error: Option<RendererRecreationApplyError>,
        commit_error: Option<RendererRecreationApplyError>,
        rollback_error: Option<RendererRecreationRollbackError>,
        events: Vec<FakeLifecycleEvent>,
    }

    impl FakeRendererLifecycle {
        fn ready(materializer_path: FakeMaterializerPath) -> Self {
            Self {
                materializer_path,
                active_lease: false,
                activity: None,
                prepare_error: None,
                drain_error: None,
                commit_error: None,
                rollback_error: None,
                events: Vec::new(),
            }
        }
    }

    impl RendererRecreationLifecycle for FakeRendererLifecycle {
        type Candidate = FakeMaterializerPath;

        fn preflight_activity(&self) -> Option<SettingsBoundaryActivity> {
            self.activity
        }

        fn prepare_candidate(
            &mut self,
            _next: &RenderCommittedSettingsUpdate,
        ) -> Result<Self::Candidate, RendererRecreationApplyError> {
            self.events
                .push(FakeLifecycleEvent::Prepare(self.materializer_path));
            if let Some(error) = self.prepare_error.clone() {
                return Err(error);
            }
            Ok(self.materializer_path)
        }

        fn release_renderer_bound_visual_state(&mut self) {
            if self.active_lease {
                self.events.push(FakeLifecycleEvent::ReleaseActiveLease);
                self.active_lease = false;
            }
            self.events.push(FakeLifecycleEvent::ReleaseVisualState);
        }

        fn drain_old_gpu_work(&mut self) -> Result<(), RendererRecreationApplyError> {
            self.events.push(FakeLifecycleEvent::DrainGpuWork);
            if let Some(error) = self.drain_error.clone() {
                return Err(error);
            }
            Ok(())
        }

        fn commit_candidate(
            &mut self,
            candidate: Self::Candidate,
        ) -> Result<(), RendererRecreationApplyError> {
            self.events.push(FakeLifecycleEvent::Commit(candidate));
            if let Some(error) = self.commit_error.clone() {
                return Err(error);
            }
            Ok(())
        }

        fn restore_previous(
            &mut self,
            _previous: &RenderCommittedSettingsUpdate,
            _apply_error: &RendererRecreationApplyError,
        ) -> Result<(), RendererRecreationRollbackError> {
            self.events.push(FakeLifecycleEvent::RestorePrevious);
            if let Some(error) = self.rollback_error.clone() {
                return Err(error);
            }
            Ok(())
        }

        fn resume_submissions(&mut self) {
            self.events.push(FakeLifecycleEvent::ResumeSubmissions);
        }
    }

    fn settings() -> RenderCommittedSettingsUpdate {
        let config = fastiplayer_config::AppConfig::default();
        RenderCommittedSettingsUpdate {
            profile: config.render.profile,
            tone_mapping: config.render.tone_mapping,
            vulkan: config.render.vulkan,
            opengles: config.render.opengles,
        }
    }

    #[test]
    fn successful_recreation_orders_prepare_release_drain_commit_and_resume() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert_eq!(result, AppRouteApplyResult::Applied);
        assert_eq!(
            lifecycle.events,
            vec![
                FakeLifecycleEvent::Prepare(FakeMaterializerPath::DmaBuf),
                FakeLifecycleEvent::ReleaseVisualState,
                FakeLifecycleEvent::DrainGpuWork,
                FakeLifecycleEvent::Commit(FakeMaterializerPath::DmaBuf),
                FakeLifecycleEvent::ResumeSubmissions,
            ]
        );
    }

    #[test]
    fn recreation_without_active_video_commits_renderer_without_materializer() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::NoActiveVideo);

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert_eq!(result, AppRouteApplyResult::Applied);
        assert!(lifecycle.events.contains(&FakeLifecycleEvent::Commit(
            FakeMaterializerPath::NoActiveVideo
        )));
    }

    #[test]
    fn resource_busy_is_retryable_and_does_not_start_or_queue_recreation() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
        lifecycle.activity = Some(SettingsBoundaryActivity::SettingsTransaction);

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert_eq!(
            result,
            AppRouteApplyResult::RuntimeBusy {
                activity: SettingsBoundaryActivity::SettingsTransaction,
            }
        );
        assert!(lifecycle.events.is_empty());
    }

    #[test]
    fn creation_failure_attempts_restore_and_keeps_original_error_typed() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
        lifecycle.prepare_error = Some(apply_error(
            RendererRecreationApplyErrorKind::CandidateCreation,
            "fake creation failure",
        ));

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert!(matches!(
            result,
            AppRouteApplyResult::RendererRecreationFailed {
                failure: SettingsApplyFailure::ApplyFailed {
                    error: RendererRecreationApplyError {
                        kind: RendererRecreationApplyErrorKind::CandidateCreation,
                        ..
                    },
                    ..
                }
            }
        ));
        assert_eq!(
            lifecycle.events,
            vec![
                FakeLifecycleEvent::Prepare(FakeMaterializerPath::DmaBuf),
                FakeLifecycleEvent::RestorePrevious,
                FakeLifecycleEvent::ResumeSubmissions,
            ]
        );
    }

    #[test]
    fn device_lost_with_failed_restore_preserves_both_typed_causes() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
        lifecycle.drain_error = Some(apply_error(
            RendererRecreationApplyErrorKind::DeviceLost,
            "fake device lost",
        ));
        lifecycle.rollback_error = Some(RendererRecreationRollbackError {
            kind: RendererRecreationRollbackErrorKind::DeviceLost,
            message: "fake old renderer restore failure".into(),
        });

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert!(matches!(
            result,
            AppRouteApplyResult::RendererRecreationFailed {
                failure: SettingsApplyFailure::ApplyAndRollbackFailed {
                    apply_error: RendererRecreationApplyError {
                        kind: RendererRecreationApplyErrorKind::DeviceLost,
                        ..
                    },
                    rollback_error: RendererRecreationRollbackError {
                        kind: RendererRecreationRollbackErrorKind::DeviceLost,
                        ..
                    },
                    ..
                }
            }
        ));
        assert!(
            !lifecycle
                .events
                .contains(&FakeLifecycleEvent::ResumeSubmissions)
        );
    }

    #[test]
    fn active_dma_buf_lease_is_released_before_gpu_drain() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
        lifecycle.active_lease = true;

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert_eq!(result, AppRouteApplyResult::Applied);
        let release_index = lifecycle
            .events
            .iter()
            .position(|event| *event == FakeLifecycleEvent::ReleaseActiveLease)
            .expect("active DMA-BUF lease must be released");
        let drain_index = lifecycle
            .events
            .iter()
            .position(|event| *event == FakeLifecycleEvent::DrainGpuWork)
            .expect("old GPU work must be drained");
        assert!(release_index < drain_index);
    }

    #[test]
    fn host_planar_path_recreates_the_same_materializer_kind() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::HostPlanar);

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert_eq!(result, AppRouteApplyResult::Applied);
        assert!(lifecycle.events.contains(&FakeLifecycleEvent::Commit(
            FakeMaterializerPath::HostPlanar
        )));
    }

    #[test]
    fn commit_failure_restores_previous_configuration_before_resuming() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
        lifecycle.commit_error = Some(apply_error(
            RendererRecreationApplyErrorKind::Commit,
            "fake commit failure",
        ));

        let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

        assert!(matches!(
            result,
            AppRouteApplyResult::RendererRecreationFailed {
                failure: SettingsApplyFailure::ApplyFailed {
                    error: RendererRecreationApplyError {
                        kind: RendererRecreationApplyErrorKind::Commit,
                        ..
                    },
                    ..
                }
            }
        ));
        assert_eq!(
            lifecycle.events.last(),
            Some(&FakeLifecycleEvent::ResumeSubmissions)
        );
    }

    #[test]
    fn resize_or_fullscreen_tick_conflicts_then_same_draft_can_retry() {
        let mut coordinator = RendererLifecycleCoordinator::default();
        coordinator.set_surface_event_pending(true);
        let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);

        let blocked = coordinator.recreate(&mut lifecycle, &settings(), &settings());
        assert_eq!(
            blocked,
            AppRouteApplyResult::RuntimeBusy {
                activity: SettingsBoundaryActivity::RendererLifecycle,
            }
        );
        assert!(lifecycle.events.is_empty());

        coordinator.set_surface_event_pending(false);
        let retried = coordinator.recreate(&mut lifecycle, &settings(), &settings());
        assert_eq!(retried, AppRouteApplyResult::Applied);
    }
}
