//! Показ GLES2-рендера на окне: перевод настроек и жизненный цикл «одно окно — одна surface».
//!
//! Перевод настроек (решение сессии 03, `results/03.md`):
//! - `Auto` и `Fifo` → ожидание вертикальной развёртки (`EGL swap interval 1`), `Immediate` →
//!   `swap interval 0` ([`VsyncMode`]);
//! - `Mailbox` в EGL нет → вертикальная развёртка и явный исход «режим заменён»
//!   ([`PresentationReconfigured::PresentModeFallback`]); так же — если драйвер зажал
//!   запрошенный интервал в пределы конфигурации;
//! - `max_frame_latency` EGL не управляет (задержку кадра держит драйвер): значение
//!   запоминается как действующее и больше ничего не меняет — явный no-op;
//! - прозрачность окна ([`SurfaceAlphaPreference`]) выбирается при создании контекста и на
//!   месте не меняется ([`PresentationReconfigureError::AlphaPreferenceChangeUnsupported`]).
//!
//! Рендер без окна (для тестов и снимков) моделирует те же состояния: окно можно отпустить и
//! вернуть, перенастройка различает «применено», «уже так» и отказ. Модель нужна, потому что
//! EGL-контекст без окна отпускать нечего (`PresentationError::NotAWindowTarget`).

use gles2_egui::{
    DetachOutcome, Gles2Context, PresentationError,
    PresentationReconfigured as ContextReconfigured, PresentationSettings, ReattachOutcome,
    SwapIntervalOutcome, VsyncMode, WindowAlphaRequest,
};
use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured, ShellPresentMode,
    SurfaceAlphaPreference, SurfacePresentSettings,
};

/// Совпадает ли запрошенный режим показа с тем, что умеет EGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PresentModeFidelity {
    /// EGL умеет ровно запрошенное.
    AsRequested,
    /// EGL заменил режим на ближайший (`Mailbox` → вертикальная развёртка).
    Substituted,
}

/// Режим вертикальной развёртки EGL для запрошенного режима показа.
#[must_use]
pub(super) const fn vsync_for(mode: ShellPresentMode) -> (VsyncMode, PresentModeFidelity) {
    match mode {
        ShellPresentMode::Auto | ShellPresentMode::Fifo => {
            (VsyncMode::WaitForVblank, PresentModeFidelity::AsRequested)
        }
        ShellPresentMode::Immediate => (VsyncMode::Immediate, PresentModeFidelity::AsRequested),
        // Mailbox в EGL нет: ближайшее по смыслу — ждать вертикальную развёртку.
        ShellPresentMode::Mailbox => (VsyncMode::WaitForVblank, PresentModeFidelity::Substituted),
    }
}

/// Запрос прозрачности окна для контекста.
#[must_use]
pub(super) const fn alpha_request_for(preference: SurfaceAlphaPreference) -> WindowAlphaRequest {
    match preference {
        SurfaceAlphaPreference::Opaque => WindowAlphaRequest::Opaque,
        SurfaceAlphaPreference::TransparentPreferred => WindowAlphaRequest::TransparentPreferred,
    }
}

/// Настройки показа контекста для запроса контракта (задержка кадра EGL не касается).
#[must_use]
pub(super) const fn context_settings_for(settings: SurfacePresentSettings) -> PresentationSettings {
    PresentationSettings {
        vsync: vsync_for(settings.present_mode).0,
        alpha: alpha_request_for(settings.alpha_preference),
    }
}

/// Что за цель показа: окно или рендер без окна с моделью отпуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    /// Окно приложения: правду о «отпущено» знает контекст.
    Window,
    /// Рендер без окна: «отпущено» — состояние модели.
    Offscreen { detached: bool },
}

/// Показ рендера: действующие настройки и (для рендера без окна) модель отпуска окна.
#[derive(Debug)]
pub(super) struct PresentationController {
    /// Настройки, которые приложение запросило последними и которые действуют.
    requested: SurfacePresentSettings,
    target: TargetKind,
}

impl PresentationController {
    /// Показ на окне с настройками, с которыми создан контекст.
    pub(super) const fn for_window(requested: SurfacePresentSettings) -> Self {
        Self {
            requested,
            target: TargetKind::Window,
        }
    }

    /// Показ рендера без окна (окно моделируется).
    pub(super) const fn for_offscreen(requested: SurfacePresentSettings) -> Self {
        Self {
            requested,
            target: TargetKind::Offscreen { detached: false },
        }
    }

    /// Окно сейчас отпущено: буферов под кадры нет.
    pub(super) fn is_detached(&self, context: &Gles2Context) -> bool {
        match self.target {
            TargetKind::Window => context.presentation().detached,
            TargetKind::Offscreen { detached } => detached,
        }
    }

    /// Применяет новые настройки на текущей surface (см. документацию модуля).
    pub(super) fn reconfigure(
        &mut self,
        context: &mut Gles2Context,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        if self.is_detached(context) {
            return Err(PresentationReconfigureError::PresentationDetached);
        }
        if settings.alpha_preference != self.requested.alpha_preference {
            return Err(PresentationReconfigureError::AlphaPreferenceChangeUnsupported);
        }
        if settings == self.requested {
            return Ok(PresentationReconfigured::AlreadyActive);
        }
        let (_, fidelity) = vsync_for(settings.present_mode);
        let clamped = match self.target {
            TargetKind::Window => apply_to_window(context, settings)?,
            TargetKind::Offscreen { .. } => false,
        };
        self.requested = settings;
        Ok(if fidelity == PresentModeFidelity::Substituted || clamped {
            PresentationReconfigured::PresentModeFallback {
                requested: settings.present_mode,
            }
        } else {
            PresentationReconfigured::AsRequested
        })
    }

    /// Отпускает окно: surface разрушается, конфигурация и контекст остаются.
    pub(super) fn detach(&mut self, context: &mut Gles2Context) -> PresentationDetachOutcome {
        match &mut self.target {
            TargetKind::Window => match context.detach_presentation() {
                Ok(DetachOutcome::Detached) => PresentationDetachOutcome::Detached,
                Ok(DetachOutcome::AlreadyDetached) => PresentationDetachOutcome::AlreadyDetached,
                Err(error) => {
                    // Surface снимается ДО возможного отказа отвязки контекста
                    // (`EglSession::detach_window`), поэтому окно свободно в любом случае; отказ
                    // самой отвязки — в лог, а не молчание.
                    log::error!("GLES2: отвязка контекста при отпуске окна отказала: {error}");
                    PresentationDetachOutcome::Detached
                }
            },
            TargetKind::Offscreen { detached } => {
                if std::mem::replace(detached, true) {
                    PresentationDetachOutcome::AlreadyDetached
                } else {
                    PresentationDetachOutcome::Detached
                }
            }
        }
    }

    /// Возвращает окно: новая surface на прежнем окне с прежней конфигурацией.
    pub(super) fn reattach(
        &mut self,
        context: &mut Gles2Context,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        match &mut self.target {
            TargetKind::Window => match context.reattach_presentation() {
                Ok(ReattachOutcome::Reattached) => Ok(PresentationReattachOutcome::Reattached),
                Ok(ReattachOutcome::AlreadyAttached) => {
                    Ok(PresentationReattachOutcome::AlreadyAttached)
                }
                Err(error) => Err(reattach_error(error)),
            },
            TargetKind::Offscreen { detached } => {
                if std::mem::replace(detached, false) {
                    Ok(PresentationReattachOutcome::Reattached)
                } else {
                    Ok(PresentationReattachOutcome::AlreadyAttached)
                }
            }
        }
    }
}

/// Применяет интервал показа к окну; `true` — драйвер зажал запрошенный интервал.
fn apply_to_window(
    context: &mut Gles2Context,
    settings: SurfacePresentSettings,
) -> Result<bool, PresentationReconfigureError> {
    match context.reconfigure_presentation(context_settings_for(settings)) {
        Ok(ContextReconfigured::Applied(SwapIntervalOutcome::Clamped { .. })) => Ok(true),
        // Интервал уже такой (разные только режимы с одним интервалом или задержка кадра).
        Ok(
            ContextReconfigured::Applied(SwapIntervalOutcome::AsRequested { .. })
            | ContextReconfigured::AlreadyActive,
        ) => Ok(false),
        Err(error) => Err(reconfigure_error(error)),
    }
}

/// Отказ контекста при перенастройке → отказ контракта.
#[must_use]
pub(super) fn reconfigure_error(error: PresentationError) -> PresentationReconfigureError {
    match error {
        PresentationError::Detached | PresentationError::NotAWindowTarget => {
            PresentationReconfigureError::PresentationDetached
        }
        PresentationError::AlphaChangeRequiresRecreation => {
            PresentationReconfigureError::AlphaPreferenceChangeUnsupported
        }
        // Настройки контекста меняются только после успеха, значит прежние действуют.
        PresentationError::Driver(driver) => {
            PresentationReconfigureError::RejectedPreviousRestored {
                message: driver.to_string(),
            }
        }
        PresentationError::SurfaceCreation(driver) => {
            PresentationReconfigureError::PresentationLost {
                message: driver.to_string(),
            }
        }
        PresentationError::ContextLost(loss) => PresentationReconfigureError::PresentationLost {
            message: loss.to_string(),
        },
    }
}

/// Отказ контекста при возврате окна → отказ контракта.
#[must_use]
pub(super) fn reattach_error(error: PresentationError) -> PresentationReattachError {
    let message = error.to_string();
    match error {
        // Не смогли получить surface: создание, потеря контекста, нет окна у цели.
        PresentationError::SurfaceCreation(_)
        | PresentationError::ContextLost(_)
        | PresentationError::NotAWindowTarget
        | PresentationError::Detached => PresentationReattachError::SurfaceCreation { message },
        // Surface создана, но настройки показа на ней не приняты.
        PresentationError::Driver(_) | PresentationError::AlphaChangeRequiresRecreation => {
            PresentationReattachError::ConfigureRejected { message }
        }
    }
}

#[cfg(test)]
mod tests;
