//! Модель жизненного цикла показа у offscreen-цели (только тесты; решение сессии 02a).
//!
//! Окна нет, но conformance-набор гоняет те же состояния, что у окна:
//! - «отпустить окно» — цель перестаёт выдавать буферы (`PresentationDetached`), текстура
//!   при этом сохраняется (её содержимое — последний показанный кадр);
//! - «вернуть окно» — буферы выдаются снова;
//! - перенастройка показа — у offscreen нет swapchain-а, поэтому считается, что «surface»
//!   умеет любой режим: `Auto` разрешается в FIFO (как у окна с FIFO), задержка нормализуется
//!   к единице. Отличие «применено» / «уже так» / отказ по прозрачности — как у окна.

use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachOutcome, PresentationReconfigureError,
    PresentationReconfigured, ShellPresentMode,
};

use crate::{RenderFrameDropReason, SurfacePresentSettings};

/// Действующая «конфигурация swapchain-а» offscreen-цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OffscreenEffectivePresentation {
    present_mode: ShellPresentMode,
    max_frame_latency: u32,
}

impl OffscreenEffectivePresentation {
    fn for_settings(settings: SurfacePresentSettings) -> Self {
        let present_mode = match settings.present_mode {
            ShellPresentMode::Auto => ShellPresentMode::Fifo,
            concrete => concrete,
        };
        Self {
            present_mode,
            max_frame_latency: settings.max_frame_latency.max(1),
        }
    }
}

/// Держит ли offscreen-цель «окно» и с какими настройками показа.
pub(crate) struct OffscreenPresentationState {
    /// `Detached` после «отпустить окно».
    attached: OffscreenAttachment,
    /// Запрошенные настройки (для отказа по смене прозрачности).
    requested: SurfacePresentSettings,
    /// Действующая конфигурация после разрешения `Auto` и нормализации задержки.
    effective: OffscreenEffectivePresentation,
}

/// Есть ли у offscreen-цели «окно».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OffscreenAttachment {
    Attached,
    Detached,
}

impl OffscreenPresentationState {
    pub(super) fn new(settings: SurfacePresentSettings) -> Self {
        Self {
            attached: OffscreenAttachment::Attached,
            requested: settings,
            effective: OffscreenEffectivePresentation::for_settings(settings),
        }
    }

    /// Без «окна» буфера нет.
    pub(super) fn check_acquire(&self) -> Result<(), RenderFrameDropReason> {
        match self.attached {
            OffscreenAttachment::Attached => Ok(()),
            OffscreenAttachment::Detached => Err(RenderFrameDropReason::PresentationDetached),
        }
    }

    pub(super) fn reconfigure(
        &mut self,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        if self.attached == OffscreenAttachment::Detached {
            return Err(PresentationReconfigureError::PresentationDetached);
        }
        if settings.alpha_preference != self.requested.alpha_preference {
            return Err(PresentationReconfigureError::AlphaPreferenceChangeUnsupported);
        }
        let next = OffscreenEffectivePresentation::for_settings(settings);
        self.requested = settings;
        if next == self.effective {
            return Ok(PresentationReconfigured::AlreadyActive);
        }
        self.effective = next;
        Ok(PresentationReconfigured::AsRequested)
    }

    pub(super) fn detach(&mut self) -> PresentationDetachOutcome {
        match self.attached {
            OffscreenAttachment::Attached => {
                self.attached = OffscreenAttachment::Detached;
                PresentationDetachOutcome::Detached
            }
            OffscreenAttachment::Detached => PresentationDetachOutcome::AlreadyDetached,
        }
    }

    pub(super) fn reattach(&mut self) -> PresentationReattachOutcome {
        match self.attached {
            OffscreenAttachment::Detached => {
                self.attached = OffscreenAttachment::Attached;
                PresentationReattachOutcome::Reattached
            }
            OffscreenAttachment::Attached => PresentationReattachOutcome::AlreadyAttached,
        }
    }
}
