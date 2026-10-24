//! Fake-тесты транзакции пересоздания рендера (`RendererLifecycleCoordinator`).
use std::cell::RefCell;
use std::rc::Rc;

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
    /// Старый рендер отпустил окно (02a).
    DetachPresentation,
    Prepare(FakeMaterializerPath),
    ReleaseActiveLease,
    ReleaseVisualState,
    DrainGpuWork,
    Commit(FakeMaterializerPath),
    /// Неустановленный кандидат уничтожен (вместе со своей surface) (02a).
    CandidateDestroyed,
    RestorePrevious,
    /// Старый рендер вернул себе окно (02a).
    ReattachPresentation,
    /// Перенастройка показа текущего рендера на месте (02a).
    ReconfigurePresentation,
    ResumeSubmissions,
}

/// Общий журнал шагов: его пишут и fake lifecycle, и drop кандидата.
type FakeEventLog = Rc<RefCell<Vec<FakeLifecycleEvent>>>;

/// Кандидат fake-а: при drop без установки пишет `CandidateDestroyed` в журнал.
struct FakeCandidate {
    path: FakeMaterializerPath,
    events: FakeEventLog,
    installed: bool,
}

impl Drop for FakeCandidate {
    fn drop(&mut self) {
        if !self.installed {
            self.events
                .borrow_mut()
                .push(FakeLifecycleEvent::CandidateDestroyed);
        }
    }
}

/// Fake owner воспроизводит failures без WGPU/device зависимости тестов.
struct FakeRendererLifecycle {
    materializer_path: FakeMaterializerPath,
    active_lease: bool,
    activity: Option<SettingsBoundaryActivity>,
    settings_error: Option<RendererRecreationApplyError>,
    prepare_error: Option<RendererRecreationApplyError>,
    drain_error: Option<RendererRecreationApplyError>,
    commit_error: Option<RendererRecreationApplyError>,
    rollback_error: Option<RendererRecreationRollbackError>,
    reattach_error: Option<RendererRecreationRollbackError>,
    reconfigure_error: Option<PresentationReconfigureFailure>,
    events: FakeEventLog,
}

impl FakeRendererLifecycle {
    fn ready(materializer_path: FakeMaterializerPath) -> Self {
        Self {
            materializer_path,
            active_lease: false,
            activity: None,
            settings_error: None,
            prepare_error: None,
            drain_error: None,
            commit_error: None,
            rollback_error: None,
            reattach_error: None,
            reconfigure_error: None,
            events: Rc::default(),
        }
    }

    /// Снимок журнала шагов.
    fn events(&self) -> Vec<FakeLifecycleEvent> {
        self.events.borrow().clone()
    }

    fn record(&self, event: FakeLifecycleEvent) {
        self.events.borrow_mut().push(event);
    }
}

impl RendererRecreationLifecycle for FakeRendererLifecycle {
    type Candidate = FakeCandidate;

    fn preflight_activity(&self) -> Option<SettingsBoundaryActivity> {
        self.activity
    }

    fn check_candidate_settings(
        &self,
        _next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), RendererRecreationApplyError> {
        self.settings_error.clone().map_or(Ok(()), Err)
    }

    fn reconfigure_presentation(
        &mut self,
        _next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), PresentationReconfigureFailure> {
        self.record(FakeLifecycleEvent::ReconfigurePresentation);
        self.reconfigure_error.clone().map_or(Ok(()), Err)
    }

    fn detach_presentation(&mut self) {
        self.record(FakeLifecycleEvent::DetachPresentation);
    }

    fn reattach_presentation(&mut self) -> Result<(), RendererRecreationRollbackError> {
        self.record(FakeLifecycleEvent::ReattachPresentation);
        self.reattach_error.clone().map_or(Ok(()), Err)
    }

    fn prepare_candidate(
        &mut self,
        _next: &RenderCommittedSettingsUpdate,
    ) -> Result<Self::Candidate, RendererRecreationApplyError> {
        self.record(FakeLifecycleEvent::Prepare(self.materializer_path));
        if let Some(error) = self.prepare_error.clone() {
            return Err(error);
        }
        Ok(FakeCandidate {
            path: self.materializer_path,
            events: self.events.clone(),
            installed: false,
        })
    }

    fn release_renderer_bound_visual_state(&mut self) {
        if self.active_lease {
            self.record(FakeLifecycleEvent::ReleaseActiveLease);
            self.active_lease = false;
        }
        self.record(FakeLifecycleEvent::ReleaseVisualState);
    }

    fn drain_old_gpu_work(&mut self) -> Result<(), RendererRecreationApplyError> {
        self.record(FakeLifecycleEvent::DrainGpuWork);
        if let Some(error) = self.drain_error.clone() {
            return Err(error);
        }
        Ok(())
    }

    fn commit_candidate(
        &mut self,
        mut candidate: Self::Candidate,
    ) -> Result<(), RendererRecreationApplyError> {
        self.record(FakeLifecycleEvent::Commit(candidate.path));
        if let Some(error) = self.commit_error.clone() {
            // Как у настоящего: кандидат роняется до возврата ошибки.
            return Err(error);
        }
        candidate.installed = true;
        Ok(())
    }

    fn restore_previous(
        &mut self,
        _previous: &RenderCommittedSettingsUpdate,
        _apply_error: &RendererRecreationApplyError,
    ) -> Result<(), RendererRecreationRollbackError> {
        self.record(FakeLifecycleEvent::RestorePrevious);
        if let Some(error) = self.rollback_error.clone() {
            return Err(error);
        }
        Ok(())
    }

    fn resume_submissions(&mut self) {
        self.record(FakeLifecycleEvent::ResumeSubmissions);
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

/// Правка, требующая полного пересоздания (заглушка `render.tone_mapping`).
fn full_recreation_settings() -> RenderCommittedSettingsUpdate {
    let mut next = settings();
    next.tone_mapping = match next.tone_mapping {
        fastiplayer_config::ToneMappingMode::Auto => fastiplayer_config::ToneMappingMode::Disabled,
        fastiplayer_config::ToneMappingMode::Disabled => fastiplayer_config::ToneMappingMode::Auto,
    };
    next
}

/// Правка только настроек показа (`[render.vulkan]`).
fn presentation_only_settings() -> RenderCommittedSettingsUpdate {
    let mut next = settings();
    next.vulkan.max_frame_latency += 1;
    next
}

#[test]
fn successful_recreation_orders_prepare_release_drain_commit_and_resume() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    assert_eq!(result, AppRouteApplyResult::Applied);
    assert_eq!(
        lifecycle.events(),
        vec![
            // 02a: окно отпущено до создания кандидата; после успешного commit-а старому
            // рендеру окно не возвращается (старый рендер заменён).
            FakeLifecycleEvent::DetachPresentation,
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

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    assert_eq!(result, AppRouteApplyResult::Applied);
    assert!(lifecycle.events().contains(&FakeLifecycleEvent::Commit(
        FakeMaterializerPath::NoActiveVideo
    )));
}

#[test]
fn resource_busy_is_retryable_and_does_not_start_or_queue_recreation() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.activity = Some(SettingsBoundaryActivity::SettingsTransaction);

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    assert_eq!(
        result,
        AppRouteApplyResult::RuntimeBusy {
            activity: SettingsBoundaryActivity::SettingsTransaction,
        }
    );
    assert!(lifecycle.events().is_empty());
}

#[test]
fn creation_failure_attempts_restore_and_keeps_original_error_typed() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.prepare_error = Some(apply_error(
        RendererRecreationApplyErrorKind::CandidateCreation,
        "fake creation failure",
    ));

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

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
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::DetachPresentation,
            FakeLifecycleEvent::Prepare(FakeMaterializerPath::DmaBuf),
            FakeLifecycleEvent::RestorePrevious,
            // Откат возвращает окно старому рендеру до возобновления показа.
            FakeLifecycleEvent::ReattachPresentation,
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

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

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
    let events = lifecycle.events();
    assert!(!events.contains(&FakeLifecycleEvent::ResumeSubmissions));
    // 02a: путь DeviceLost — старый рендер остаётся отпущенным, окно ему не возвращается.
    assert!(!events.contains(&FakeLifecycleEvent::ReattachPresentation));
}

#[test]
fn active_dma_buf_lease_is_released_before_gpu_drain() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.active_lease = true;

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    assert_eq!(result, AppRouteApplyResult::Applied);
    let release_index = lifecycle
        .events()
        .iter()
        .position(|event| *event == FakeLifecycleEvent::ReleaseActiveLease)
        .expect("active DMA-BUF lease must be released");
    let drain_index = lifecycle
        .events()
        .iter()
        .position(|event| *event == FakeLifecycleEvent::DrainGpuWork)
        .expect("old GPU work must be drained");
    assert!(release_index < drain_index);
}

#[test]
fn host_planar_path_recreates_the_same_materializer_kind() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::HostPlanar);

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    assert_eq!(result, AppRouteApplyResult::Applied);
    assert!(lifecycle.events().contains(&FakeLifecycleEvent::Commit(
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

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

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
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::DetachPresentation,
            FakeLifecycleEvent::Prepare(FakeMaterializerPath::DmaBuf),
            FakeLifecycleEvent::ReleaseVisualState,
            FakeLifecycleEvent::DrainGpuWork,
            FakeLifecycleEvent::Commit(FakeMaterializerPath::DmaBuf),
            // 02a: отказавший кандидат уничтожен ДО возврата окна старому рендеру.
            FakeLifecycleEvent::CandidateDestroyed,
            FakeLifecycleEvent::RestorePrevious,
            FakeLifecycleEvent::ReattachPresentation,
            FakeLifecycleEvent::ResumeSubmissions,
        ]
    );
}

#[test]
fn resize_or_fullscreen_tick_conflicts_then_same_draft_can_retry() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    coordinator.set_surface_event_pending(true);
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);

    let blocked = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());
    assert_eq!(
        blocked,
        AppRouteApplyResult::RuntimeBusy {
            activity: SettingsBoundaryActivity::RendererLifecycle,
        }
    );
    assert!(lifecycle.events().is_empty());

    coordinator.set_surface_event_pending(false);
    let retried = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());
    assert_eq!(retried, AppRouteApplyResult::Applied);
}

/// Ожидаемый `ApplyFailed` с конкретным видом ошибки применения.
fn expect_apply_failed(result: &AppRouteApplyResult, kind: RendererRecreationApplyErrorKind) {
    match result {
        AppRouteApplyResult::RendererRecreationFailed {
            failure: SettingsApplyFailure::ApplyFailed { error, .. },
        } => assert_eq!(error.kind, kind, "unexpected apply error: {error:?}"),
        other => panic!("expected ApplyFailed({kind:?}), got {other:?}"),
    }
}

/// Ожидаемый `ApplyAndRollbackFailed` с конкретными видами обеих ошибок.
fn expect_apply_and_rollback_failed(
    result: &AppRouteApplyResult,
    apply_kind: RendererRecreationApplyErrorKind,
    rollback_kind: RendererRecreationRollbackErrorKind,
) {
    match result {
        AppRouteApplyResult::RendererRecreationFailed {
            failure:
                SettingsApplyFailure::ApplyAndRollbackFailed {
                    apply_error,
                    rollback_error,
                    ..
                },
        } => {
            assert_eq!(apply_error.kind, apply_kind);
            assert_eq!(rollback_error.kind, rollback_kind);
        }
        other => panic!("expected ApplyAndRollbackFailed, got {other:?}"),
    }
}

/// 02a: правка только `[render.vulkan]` — перенастройка на месте. Кандидат не создаётся,
/// окно не отпускается, GPU не ждём, аренда кадра и кэш импорта не трогаются.
#[test]
fn presentation_only_change_is_applied_in_place_without_candidate() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.active_lease = true;

    let result = coordinator.recreate(&mut lifecycle, &settings(), &presentation_only_settings());

    assert_eq!(result, AppRouteApplyResult::Applied);
    assert_eq!(
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::ReconfigurePresentation,
            FakeLifecycleEvent::ResumeSubmissions,
        ]
    );
    assert!(
        lifecycle.active_lease,
        "in-place reconfigure must not release the active frame lease"
    );
}

/// 02a: отказ перенастройки с сохранением прежнего показа — `ApplyFailed`, показ продолжается.
#[test]
fn rejected_in_place_reconfigure_keeps_previous_presentation_and_resumes() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::HostPlanar);
    lifecycle.reconfigure_error = Some(PresentationReconfigureFailure::Rejected(apply_error(
        RendererRecreationApplyErrorKind::PresentationReconfigure,
        "fake surface rejected the new presentation",
    )));

    let result = coordinator.recreate(&mut lifecycle, &settings(), &presentation_only_settings());

    expect_apply_failed(
        &result,
        RendererRecreationApplyErrorKind::PresentationReconfigure,
    );
    assert_eq!(
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::ReconfigurePresentation,
            FakeLifecycleEvent::ResumeSubmissions,
        ]
    );
}

/// 02a: перенастройка потеряла показ — `ApplyAndRollbackFailed` с `SurfaceInvalidated`,
/// показ не «возобновляется» вслепую.
#[test]
fn lost_presentation_in_place_is_apply_and_rollback_failure() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.reconfigure_error = Some(PresentationReconfigureFailure::PresentationLost {
        apply_error: apply_error(
            RendererRecreationApplyErrorKind::PresentationReconfigure,
            "fake new presentation rejected",
        ),
        rollback_error: RendererRecreationRollbackError {
            kind: RendererRecreationRollbackErrorKind::SurfaceInvalidated,
            message: "fake previous presentation rejected".into(),
        },
    });

    let result = coordinator.recreate(&mut lifecycle, &settings(), &presentation_only_settings());

    expect_apply_and_rollback_failed(
        &result,
        RendererRecreationApplyErrorKind::PresentationReconfigure,
        RendererRecreationRollbackErrorKind::SurfaceInvalidated,
    );
    assert_eq!(
        lifecycle.events(),
        vec![FakeLifecycleEvent::ReconfigurePresentation]
    );
}

/// 02a: перенастройка на месте подчиняется тому же lifecycle boundary (resize этого тика).
#[test]
fn in_place_reconfigure_is_busy_during_surface_event() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    coordinator.set_surface_event_pending(true);
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);

    let result = coordinator.recreate(&mut lifecycle, &settings(), &presentation_only_settings());

    assert_eq!(
        result,
        AppRouteApplyResult::RuntimeBusy {
            activity: SettingsBoundaryActivity::RendererLifecycle,
        }
    );
    assert!(lifecycle.events().is_empty());
}

/// Одинаковые настройки — no-op без единого шага lifecycle.
#[test]
fn unchanged_settings_are_a_no_op() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);

    let result = coordinator.recreate(&mut lifecycle, &settings(), &settings());

    assert_eq!(result, AppRouteApplyResult::Noop);
    assert!(lifecycle.events().is_empty());
}

/// 02a: отказ по одним настройкам (профиль без рендера) — до отпускания окна.
#[test]
fn unsupported_candidate_settings_are_refused_before_window_is_released() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.settings_error = Some(apply_error(
        RendererRecreationApplyErrorKind::UnsupportedProfile,
        "fake profile without renderer",
    ));

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    expect_apply_failed(
        &result,
        RendererRecreationApplyErrorKind::UnsupportedProfile,
    );
    assert_eq!(
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::RestorePrevious,
            FakeLifecycleEvent::ResumeSubmissions,
        ]
    );
}

/// 02a: провал ожидания GPU — кандидат уничтожен ДО того, как старый рендер вернёт окно.
#[test]
fn drain_failure_destroys_candidate_before_old_renderer_takes_window_back() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::HostPlanar);
    lifecycle.drain_error = Some(apply_error(
        RendererRecreationApplyErrorKind::GpuDrain,
        "fake drain timeout",
    ));

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    expect_apply_failed(&result, RendererRecreationApplyErrorKind::GpuDrain);
    assert_eq!(
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::DetachPresentation,
            FakeLifecycleEvent::Prepare(FakeMaterializerPath::HostPlanar),
            FakeLifecycleEvent::ReleaseVisualState,
            FakeLifecycleEvent::DrainGpuWork,
            FakeLifecycleEvent::CandidateDestroyed,
            FakeLifecycleEvent::RestorePrevious,
            FakeLifecycleEvent::ReattachPresentation,
            FakeLifecycleEvent::ResumeSubmissions,
        ]
    );
}

/// 02a: старый рендер не смог вернуть окно — `ApplyAndRollbackFailed` с
/// `SurfaceInvalidated`, исходная ошибка сохранена, показ не возобновляется.
#[test]
fn failed_window_reattach_is_typed_surface_invalidated_rollback() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.prepare_error = Some(apply_error(
        RendererRecreationApplyErrorKind::CandidateCreation,
        "fake creation failure",
    ));
    lifecycle.reattach_error = Some(RendererRecreationRollbackError {
        kind: RendererRecreationRollbackErrorKind::SurfaceInvalidated,
        message: "fake window surface creation failure".into(),
    });

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    expect_apply_and_rollback_failed(
        &result,
        RendererRecreationApplyErrorKind::CandidateCreation,
        RendererRecreationRollbackErrorKind::SurfaceInvalidated,
    );
    assert!(
        !lifecycle
            .events()
            .contains(&FakeLifecycleEvent::ResumeSubmissions)
    );
}

/// 02a: DeviceLost с успешным восстановлением — новый рендер прежней конфигурации стоит в
/// слоте, старый остаётся отпущенным (окно ему не возвращается), показ возобновлён.
#[test]
fn device_lost_restore_keeps_old_renderer_detached() {
    let mut coordinator = RendererLifecycleCoordinator::default();
    let mut lifecycle = FakeRendererLifecycle::ready(FakeMaterializerPath::DmaBuf);
    lifecycle.drain_error = Some(apply_error(
        RendererRecreationApplyErrorKind::DeviceLost,
        "fake device lost",
    ));

    let result = coordinator.recreate(&mut lifecycle, &settings(), &full_recreation_settings());

    expect_apply_failed(&result, RendererRecreationApplyErrorKind::DeviceLost);
    assert_eq!(
        lifecycle.events(),
        vec![
            FakeLifecycleEvent::DetachPresentation,
            FakeLifecycleEvent::Prepare(FakeMaterializerPath::DmaBuf),
            FakeLifecycleEvent::ReleaseVisualState,
            FakeLifecycleEvent::DrainGpuWork,
            FakeLifecycleEvent::CandidateDestroyed,
            FakeLifecycleEvent::RestorePrevious,
            FakeLifecycleEvent::ResumeSubmissions,
        ]
    );
}
