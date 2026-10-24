//! Показ fake-рендера: честная модель «одно окно — одна surface» без GPU (сессия 02a).
//!
//! «Surface» умеет FIFO и Mailbox; Immediate недоступен (проверка запасного режима). Окно
//! можно отпустить и вернуть; пока оно отпущено, буферов нет. Размера у fake нет вовсе.

use crate::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured, RenderFrameDropReason,
    ShellPresentMode, SurfacePresentSettings,
};

/// Держит ли fake-рендер окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeWindowAttachment {
    /// Surface есть, буферы выдаются.
    Attached,
    /// Окно отпущено: surface нет.
    Detached,
}

/// Действующая конфигурация «swapchain-а» после выбора режима.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeEffectivePresentation {
    present_mode: ShellPresentMode,
    max_frame_latency: u32,
}

/// Состояние показа fake-рендера.
pub(super) struct FakePresentation {
    attachment: FakeWindowAttachment,
    requested: SurfacePresentSettings,
    effective: FakeEffectivePresentation,
}

/// Режим, который «surface» fake-а реально даст на запрос (как `choose_present_mode`).
fn resolve_present_mode(requested: ShellPresentMode) -> ShellPresentMode {
    match requested {
        ShellPresentMode::Auto | ShellPresentMode::Immediate => ShellPresentMode::Fifo,
        supported @ (ShellPresentMode::Fifo | ShellPresentMode::Mailbox) => supported,
    }
}

fn effective_for(settings: SurfacePresentSettings) -> FakeEffectivePresentation {
    FakeEffectivePresentation {
        present_mode: resolve_present_mode(settings.present_mode),
        max_frame_latency: settings.max_frame_latency.max(1),
    }
}

impl FakePresentation {
    /// Показ сразу после создания рендера с настройками `settings`.
    pub(super) fn new(settings: SurfacePresentSettings) -> Self {
        Self {
            attachment: FakeWindowAttachment::Attached,
            requested: settings,
            effective: effective_for(settings),
        }
    }

    /// Можно ли выдать буфер (дефект `ignore_detach` выдаёт его и без окна).
    pub(super) fn check_acquire(&self, ignore_detach: bool) -> Result<(), RenderFrameDropReason> {
        match self.attachment {
            FakeWindowAttachment::Detached if !ignore_detach => {
                Err(RenderFrameDropReason::PresentationDetached)
            }
            FakeWindowAttachment::Attached | FakeWindowAttachment::Detached => Ok(()),
        }
    }

    /// Перенастройка на месте; дефект `never_no_op` сообщает «применено» даже без изменений.
    pub(super) fn reconfigure(
        &mut self,
        settings: SurfacePresentSettings,
        never_no_op: bool,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        if self.attachment == FakeWindowAttachment::Detached {
            return Err(PresentationReconfigureError::PresentationDetached);
        }
        if settings.alpha_preference != self.requested.alpha_preference {
            return Err(PresentationReconfigureError::AlphaPreferenceChangeUnsupported);
        }
        let next = effective_for(settings);
        self.requested = settings;
        if next == self.effective && !never_no_op {
            return Ok(PresentationReconfigured::AlreadyActive);
        }
        self.effective = next;
        if settings.present_mode != ShellPresentMode::Auto
            && next.present_mode != settings.present_mode
        {
            return Ok(PresentationReconfigured::PresentModeFallback {
                requested: settings.present_mode,
            });
        }
        Ok(PresentationReconfigured::AsRequested)
    }

    /// Отпускает окно.
    pub(super) fn detach(&mut self) -> PresentationDetachOutcome {
        match self.attachment {
            FakeWindowAttachment::Attached => {
                self.attachment = FakeWindowAttachment::Detached;
                PresentationDetachOutcome::Detached
            }
            FakeWindowAttachment::Detached => PresentationDetachOutcome::AlreadyDetached,
        }
    }

    /// Возвращает окно (у fake возврат не отказывает).
    pub(super) fn reattach(
        &mut self,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        match self.attachment {
            FakeWindowAttachment::Detached => {
                self.attachment = FakeWindowAttachment::Attached;
                Ok(PresentationReattachOutcome::Reattached)
            }
            FakeWindowAttachment::Attached => Ok(PresentationReattachOutcome::AlreadyAttached),
        }
    }
}
