//! Immutable transport/status и отдельная Undo-модель для UI.
//!
//! Здесь controller/runtime переводят queue/discovery/removal state в intent-oriented
//! UI snapshots. UI не читает queue, shuffle history или pending install fields напрямую.

use std::time::{Duration, Instant};

use playlist_core::{ManualNavigationDirection, RepeatMode};

use super::PlaylistRuntime;
use super::controller::{ControllerManualNavigationAvailability, ControllerRemovalKind};
use super::discovery::PlaylistDiscoveryNavigationStatus;

/// Navigation button state сохраняет отличие definite no-item от возможного ожидания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigationControlAvailability {
    Ready,
    PotentialWait,
    Pending,
    Disabled,
}

impl NavigationControlAvailability {
    pub(crate) const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    pub(crate) const fn explanation(self) -> &'static str {
        match self {
            Self::Ready => "Переход готов",
            Self::PotentialWait => "Можно дождаться подходящего файла из текущего поиска",
            Self::Pending => "Повтор изменит только последний ожидающий переход",
            Self::Disabled => "Подходящего элемента нет",
        }
    }
}

impl From<ControllerManualNavigationAvailability> for NavigationControlAvailability {
    fn from(value: ControllerManualNavigationAvailability) -> Self {
        match value {
            ControllerManualNavigationAvailability::Ready => Self::Ready,
            ControllerManualNavigationAvailability::PotentialWait => Self::PotentialWait,
            ControllerManualNavigationAvailability::Pending => Self::Pending,
            ControllerManualNavigationAvailability::Disabled => Self::Disabled,
        }
    }
}

/// Read-only представление одного authoritative Undo без controller snapshot-а в UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemovalUndoUiModel {
    /// Грамматически готовое название отменяемой операции для русской подписи.
    pub(crate) kind_label: &'static str,
    /// Целое число секунд, округлённое вверх runtime owner-ом.
    pub(crate) seconds_remaining: u64,
}

impl RemovalUndoUiModel {
    /// Формирует единое имя для tooltip и AccessKit без расхождения строк.
    pub(crate) fn action_label(self) -> String {
        format!(
            "Отменить {} ({} с)",
            self.kind_label, self.seconds_remaining
        )
    }
}

/// Отдельный read-only snapshot visibility и runtime wake для Undo toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistUndoUiSnapshot {
    /// Authoritative Undo отсутствует сразу после expiry/invalidation/activation.
    pub(crate) undo: Option<RemovalUndoUiModel>,
    /// Следующая смена countdown либо expiry; animation repaint сюда не входит.
    pub(crate) next_wake_deadline: Option<Instant>,
}

/// Global D41/D50 status остаётся доступен даже при скрытом sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistGlobalTransportStatus {
    WaitingManual {
        direction: ManualNavigationDirection,
    },
    WaitingAutomatic,
    TargetReady,
    Exhausted,
    Cancelled,
    Fatal,
}

impl PlaylistGlobalTransportStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::WaitingManual {
                direction: ManualNavigationDirection::Previous,
            } => "Ожидание предыдущего элемента…",
            Self::WaitingManual {
                direction: ManualNavigationDirection::Next,
            } => "Ожидание следующего элемента…",
            Self::WaitingAutomatic => "Ожидание следующего элемента для автоперехода…",
            Self::TargetReady => "Следующий элемент готовится…",
            Self::Exhausted => "Подходящих элементов больше нет",
            Self::Cancelled => "Ожидание перехода отменено",
            Self::Fatal => "Поиск следующего элемента завершился ошибкой",
        }
    }

    pub(crate) const fn can_cancel(self) -> bool {
        matches!(self, Self::WaitingManual { .. } | Self::WaitingAutomatic)
    }
}

/// Один immutable snapshot для transport buttons и global navigation recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistTransportUiModel {
    /// Controller view revision делает snapshot явно correlated с owner state.
    pub(crate) playlist_view_revision: u64,
    pub(crate) previous: NavigationControlAvailability,
    pub(crate) next: NavigationControlAvailability,
    /// Подтверждённый owner-ом режим повтора для persistent transport control.
    pub(crate) repeat_mode: RepeatMode,
    /// Подтверждённый owner-ом shuffle flag без optimistic UI state.
    pub(crate) shuffle_enabled: bool,
    /// Единая доступность mode mutations на текущей controller revision.
    pub(crate) queue_modes_enabled: bool,
    pub(crate) global_status: Option<PlaylistGlobalTransportStatus>,
}

impl PlaylistRuntime {
    /// Собирает transport-only UI snapshot без mutation и Undo lifecycle.
    pub(crate) fn playlist_transport_ui_model(
        &self,
        current_position: Duration,
    ) -> PlaylistTransportUiModel {
        let wait_availability = self.discovery.manual_wait_availability();
        let restart_threshold = self.previous_restart_threshold();
        let (
            playlist_view_revision,
            previous,
            next,
            repeat_mode,
            shuffle_enabled,
            queue_modes_enabled,
        ) = self.controller.as_ref().map_or(
            (
                0,
                NavigationControlAvailability::Disabled,
                NavigationControlAvailability::Disabled,
                RepeatMode::StopAtEnd,
                false,
                false,
            ),
            |controller| {
                (
                    controller.view_snapshot().revision().get(),
                    controller
                        .manual_navigation_availability(
                            ManualNavigationDirection::Previous,
                            current_position,
                            restart_threshold,
                            wait_availability,
                        )
                        .into(),
                    controller
                        .manual_navigation_availability(
                            ManualNavigationDirection::Next,
                            current_position,
                            restart_threshold,
                            wait_availability,
                        )
                        .into(),
                    controller.repeat_mode(),
                    controller.queue().shuffle_enabled(),
                    controller.view_snapshot().structural_actions_enabled(),
                )
            },
        );
        let global_status = match self.playlist_discovery_navigation_status() {
            PlaylistDiscoveryNavigationStatus::Idle => None,
            PlaylistDiscoveryNavigationStatus::WaitingManual { direction, .. } => {
                Some(PlaylistGlobalTransportStatus::WaitingManual { direction })
            }
            PlaylistDiscoveryNavigationStatus::WaitingAutomatic { .. } => {
                Some(PlaylistGlobalTransportStatus::WaitingAutomatic)
            }
            PlaylistDiscoveryNavigationStatus::TargetReady { .. } => {
                Some(PlaylistGlobalTransportStatus::TargetReady)
            }
            PlaylistDiscoveryNavigationStatus::Exhausted { .. } => {
                Some(PlaylistGlobalTransportStatus::Exhausted)
            }
            PlaylistDiscoveryNavigationStatus::Cancelled { .. } => {
                Some(PlaylistGlobalTransportStatus::Cancelled)
            }
            PlaylistDiscoveryNavigationStatus::Fatal { .. } => {
                Some(PlaylistGlobalTransportStatus::Fatal)
            }
        };
        PlaylistTransportUiModel {
            playlist_view_revision,
            previous,
            next,
            repeat_mode,
            shuffle_enabled,
            queue_modes_enabled,
            global_status,
        }
    }

    /// Собирает отдельный Undo snapshot и exactly-once очищает stale runtime slot.
    pub(crate) fn playlist_undo_ui_snapshot(&mut self, now: Instant) -> PlaylistUndoUiSnapshot {
        // Runtime status остаётся единственной точкой expiry/invalidation проверки.
        let undo_status = self.removal_undo_status(now);
        // UI получает только грамматическую подпись и countdown, а не snapshot очереди.
        let undo = undo_status.map(|status| RemovalUndoUiModel {
            kind_label: match status.kind {
                ControllerRemovalKind::Remove => "удаление элемента",
                ControllerRemovalKind::Clear => "очистку плейлиста",
                ControllerRemovalKind::RemoveOthers => "удаление остальных элементов",
                ControllerRemovalKind::RemoveSelected => "удаление выбранных элементов",
                ControllerRemovalKind::RemoveUnselected => "удаление невыбранных элементов",
            },
            seconds_remaining: status.seconds_remaining,
        });
        // Deadline сохраняется отдельно от transport, но не теряет runtime источник.
        let next_wake_deadline = undo_status.map(|status| status.next_wake_deadline);

        PlaylistUndoUiSnapshot {
            undo,
            next_wake_deadline,
        }
    }
}
