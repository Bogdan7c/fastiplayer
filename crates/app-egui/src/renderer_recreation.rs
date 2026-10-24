//! Controlled transactional recreation renderer/device/surface lifecycle-а.
//!
//! Модуль живёт в app composition layer-е: только здесь одновременно доступны окно,
//! слот рендера и intent-boundary `AppState`. Settings/UI не получают GPU handles.
//!
//! С сессии 02 плана `user/gles2-renderer/` рендер — `Box<dyn RenderShell>`: новый экземпляр
//! того же семейства создаёт каталог (`render_backend::create_renderer`), импорт кадров
//! переносится контрактом (`export_video_import_for_recreation` → `prepare_recreated_video_import`
//! → `install_video_import` после ожидания старого GPU).
//!
//! Сессия 02a («одно окно — одна surface», решение владельца): изменение только
//! `[render.vulkan]` применяется перенастройкой показа текущего рендера на месте
//! ([`settings_change`]); полное пересоздание сначала просит старый рендер отпустить окно,
//! а откат возвращает окно ему же. Вторая surface на одном окне запрещена оконной системой
//! (Wayland `wp_fifo_v1`, одна `EGLSurface` у EGL).

use std::sync::Arc;
use std::time::Duration;

use fastiplayer_config::RenderProfile;
use fastiplayer_settings::{
    AppRouteApplyResult, RenderCommittedSettingsUpdate, RendererRecreationApplyError,
    RendererRecreationApplyErrorKind, RendererRecreationFailure, RendererRecreationRollbackError,
    RendererRecreationRollbackErrorKind, SettingStateOwner, SettingsApplyFailure,
    SettingsBoundaryActivity,
};
use render_shell_api::{
    PendingVideoImport, RenderShell, RenderSurfaceSize, RendererGpuDrainError,
    UiTextureFullSetReason,
};
use winit::window::Window;

use crate::render_backend::{RendererCreateRequest, RendererInitialColor, create_renderer};
use crate::state::AppState;
use crate::system_capabilities::probe_system_capabilities;

mod presentation;
mod settings_change;

pub(crate) use presentation::PresentationReconfigureFailure;
use settings_change::{RendererSettingsChange, classify_renderer_settings_change};

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

    /// Применяет изменение renderer-bound настроек без hidden retry queue: только
    /// `[render.vulkan]` — перенастройкой показа на месте, иначе — транзакцией пересоздания.
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
        let result = match classify_renderer_settings_change(previous, next) {
            RendererSettingsChange::Unchanged => AppRouteApplyResult::Noop,
            RendererSettingsChange::PresentationOnly => {
                Self::run_presentation_reconfigure(lifecycle, next)
            }
            RendererSettingsChange::FullRecreation => {
                Self::run_transaction(lifecycle, previous, next)
            }
        };
        self.activity = None;
        result
    }

    /// Перенастройка показа текущего рендера на месте: без кандидата, без ожидания GPU и без
    /// освобождения кадров/кэша импорта (их рендер не теряет).
    fn run_presentation_reconfigure<L>(
        lifecycle: &mut L,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult
    where
        L: RendererRecreationLifecycle,
    {
        match lifecycle.reconfigure_presentation(next) {
            Ok(()) => {
                lifecycle.resume_submissions();
                AppRouteApplyResult::Applied
            }
            Err(PresentationReconfigureFailure::Rejected(apply_error)) => {
                // Прежняя конфигурация показа работает: показ продолжается.
                lifecycle.resume_submissions();
                renderer_failure(SettingsApplyFailure::ApplyFailed {
                    owner: SettingStateOwner::RendererLifecycle,
                    error: apply_error,
                })
            }
            Err(PresentationReconfigureFailure::PresentationLost {
                apply_error,
                rollback_error,
            }) => renderer_failure(SettingsApplyFailure::ApplyAndRollbackFailed {
                apply_owner: SettingStateOwner::RendererLifecycle,
                apply_error,
                rollback_owner: SettingStateOwner::RendererLifecycle,
                rollback_error,
            }),
        }
    }

    /// Выполняет ordered check -> detach -> prepare -> release -> drain -> commit sequence.
    ///
    /// Старый рендер отпускает окно ДО создания кандидата (у окна одна surface). При любой
    /// ошибке кандидат уничтожается ДО того, как старый рендер вернёт себе окно.
    fn run_transaction<L>(
        lifecycle: &mut L,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult
    where
        L: RendererRecreationLifecycle,
    {
        // Отказ по одним настройкам (например, профиль без рендера) — до того, как окно
        // отпущено: старый рендер не теряет surface ради заведомо невозможного кандидата.
        if let Err(apply_error) = lifecycle.check_candidate_settings(next) {
            return restore_after_failure(
                lifecycle,
                previous,
                apply_error,
                OldRendererWindow::Held,
            );
        }

        lifecycle.detach_presentation();

        // Кандидат, не созданный до конца, уничтожается внутри `prepare_candidate`.
        let candidate = match lifecycle.prepare_candidate(next) {
            Ok(candidate) => candidate,
            Err(apply_error) => {
                return restore_after_failure(
                    lifecycle,
                    previous,
                    apply_error,
                    OldRendererWindow::Detached,
                );
            }
        };

        lifecycle.release_renderer_bound_visual_state();

        if let Err(apply_error) = lifecycle.drain_old_gpu_work() {
            // Кандидат (и его surface) уничтожается до возврата окна старому рендеру.
            drop(candidate);
            return restore_after_failure(
                lifecycle,
                previous,
                apply_error,
                OldRendererWindow::Detached,
            );
        }

        // При отказе commit сам роняет кандидата до возврата Err.
        if let Err(apply_error) = lifecycle.commit_candidate(candidate) {
            return restore_after_failure(
                lifecycle,
                previous,
                apply_error,
                OldRendererWindow::Detached,
            );
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

    /// Отклоняет пересоздание по одним настройкам (без мутаций, до «отпустить окно»).
    fn check_candidate_settings(
        &self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), RendererRecreationApplyError>;

    /// Применяет новые настройки показа к текущему рендеру на месте.
    fn reconfigure_presentation(
        &mut self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), PresentationReconfigureFailure>;

    /// Старый рендер отпускает окно: его surface уничтожается до создания кандидата.
    fn detach_presentation(&mut self);

    /// Старый рендер возвращает себе окно (откат); провал — `SurfaceInvalidated`.
    fn reattach_presentation(&mut self) -> Result<(), RendererRecreationRollbackError>;

    /// Создаёт candidate resources, не меняя old runtime (окно уже отпущено старым рендером).
    /// При ошибке частично созданный кандидат уничтожается до возврата `Err`.
    fn prepare_candidate(
        &mut self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<Self::Candidate, RendererRecreationApplyError>;

    /// Освобождает renderer-bound leases/views перед ожиданием старой queue.
    fn release_renderer_bound_visual_state(&mut self);

    /// Дожидается callbacks и завершения старой GPU work.
    fn drain_old_gpu_work(&mut self) -> Result<(), RendererRecreationApplyError>;

    /// Делает готовый candidate active; partial mutation при Err запрещена, а кандидат при
    /// Err уничтожается до возврата.
    fn commit_candidate(
        &mut self,
        candidate: Self::Candidate,
    ) -> Result<(), RendererRecreationApplyError>;

    /// Пытается восстановить именно предыдущую конфигурацию.
    ///
    /// При `DeviceLost` ставит в слот НОВЫЙ рендер прежней конфигурации (старый device
    /// мёртв, старый рендер остаётся отпущенным и уничтожается заменой); при остальных ошибках
    /// активным остаётся старый рендер, и окно ему возвращает координатор.
    fn restore_previous(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        apply_error: &RendererRecreationApplyError,
    ) -> Result<(), RendererRecreationRollbackError>;

    /// Возобновляет submissions только после proven active path-а.
    fn resume_submissions(&mut self);
}

/// Держит ли старый рендер окно в момент ошибки транзакции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OldRendererWindow {
    /// Окно не отпускалось (отказ по настройкам до detach).
    Held,

    /// Окно отпущено перед созданием кандидата: откат обязан вернуть его старому рендеру.
    Detached,
}

/// Всегда вызывает concrete restore boundary и сохраняет обе typed errors при failure.
///
/// Порядок отката: restore прежней конфигурации → возврат окна старому рендеру (если он
/// остаётся активным и окно было отпущено) → возобновление показа. Кандидат к этому моменту
/// уже уничтожен вызывающим кодом.
fn restore_after_failure<L>(
    lifecycle: &mut L,
    previous: &RenderCommittedSettingsUpdate,
    apply_error: RendererRecreationApplyError,
    old_window: OldRendererWindow,
) -> AppRouteApplyResult
where
    L: RendererRecreationLifecycle,
{
    let restore_result = lifecycle.restore_previous(previous, &apply_error);
    // DeviceLost: restore заменяет старый рендер новым — возвращать окно мёртвому нельзя.
    let old_renderer_stays_active =
        apply_error.kind != RendererRecreationApplyErrorKind::DeviceLost;
    let reattach_result = (old_window == OldRendererWindow::Detached && old_renderer_stays_active)
        .then(|| lifecycle.reattach_presentation());
    let rollback_result = match (restore_result, reattach_result) {
        (Ok(()), None | Some(Ok(()))) => Ok(()),
        (Ok(()), Some(Err(reattach_error))) => Err(reattach_error),
        (Err(restore_error), Some(Err(reattach_error))) => {
            // В отчёт идёт первая причина отката; вторую не теряем молча.
            tracing::warn!(
                message = %reattach_error.message,
                "Old renderer could not take the window back after a failed restore"
            );
            Err(restore_error)
        }
        (Err(restore_error), None | Some(Ok(()))) => Err(restore_error),
    };
    match rollback_result {
        Ok(()) => {
            lifecycle.resume_submissions();
            renderer_failure(SettingsApplyFailure::ApplyFailed {
                owner: SettingStateOwner::RendererLifecycle,
                error: apply_error,
            })
        }
        Err(rollback_error) => renderer_failure(SettingsApplyFailure::ApplyAndRollbackFailed {
            apply_owner: SettingStateOwner::RendererLifecycle,
            apply_error,
            rollback_owner: SettingStateOwner::RendererLifecycle,
            rollback_error,
        }),
    }
}

/// Итог Apply с typed failure рендера.
const fn renderer_failure(failure: RendererRecreationFailure) -> AppRouteApplyResult {
    AppRouteApplyResult::RendererRecreationFailed { failure }
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
        ensure_candidate_profile_supported(settings)?;

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
        // Новый рендер не видел ни одной текстуры интерфейса (сессия 02a): ближайший свежий
        // кадр обязан нести полный набор, иначе частичная правка атласа шрифтов ему не к чему.
        // Путь отката после потери устройства ставит рендер тоже здесь.
        self.app_state
            .request_full_ui_texture_set(UiTextureFullSetReason::RendererInstalled);
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

    fn check_candidate_settings(
        &self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), RendererRecreationApplyError> {
        ensure_candidate_profile_supported(next)
    }

    fn reconfigure_presentation(
        &mut self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), PresentationReconfigureFailure> {
        self.reconfigure_current_presentation(next)
    }

    fn detach_presentation(&mut self) {
        self.detach_current_presentation();
    }

    fn reattach_presentation(&mut self) -> Result<(), RendererRecreationRollbackError> {
        self.reattach_current_presentation()
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

/// Профиль без реализованного рендера отклоняется до создания кандидата (до сессии 09).
fn ensure_candidate_profile_supported(
    settings: &RenderCommittedSettingsUpdate,
) -> Result<(), RendererRecreationApplyError> {
    if settings.profile == RenderProfile::OpenGles {
        return Err(apply_error(
            RendererRecreationApplyErrorKind::UnsupportedProfile,
            "OpenGL ES renderer is absent; the active Vulkan renderer was preserved",
        ));
    }
    Ok(())
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
mod tests;
