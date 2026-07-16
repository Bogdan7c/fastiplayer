//! Immutable transport/status model для bottom host.
//!
//! Здесь controller/runtime переводят queue/discovery/Undo state в intent-oriented UI model.
//! UI не читает queue, shuffle history или pending install fields напрямую.

use std::time::{Duration, Instant};

use playlist_core::ManualNavigationDirection;

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

/// Read-only prototype Undo model без controller snapshot-а в UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemovalUndoUiModel {
    pub(crate) kind_label: &'static str,
    pub(crate) seconds_remaining: u64,
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

/// Один immutable snapshot для prototype buttons и global recovery actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistTransportUiModel {
    /// Controller view revision делает snapshot явно correlated с owner state.
    pub(crate) playlist_view_revision: u64,
    pub(crate) previous: NavigationControlAvailability,
    pub(crate) next: NavigationControlAvailability,
    pub(crate) global_status: Option<PlaylistGlobalTransportStatus>,
    pub(crate) undo: Option<RemovalUndoUiModel>,
    /// Только ближайшая видимая смена countdown/expiry, не animation loop.
    pub(crate) next_wake_deadline: Option<Instant>,
}

impl PlaylistRuntime {
    /// Собирает UI snapshot и exactly-once очищает expired Undo slot.
    pub(crate) fn playlist_transport_ui_model(
        &mut self,
        current_position: Duration,
        now: Instant,
    ) -> PlaylistTransportUiModel {
        let wait_availability = self.discovery.manual_wait_availability();
        let restart_threshold = self.previous_restart_threshold();
        let (playlist_view_revision, previous, next) = self.controller.as_ref().map_or(
            (
                0,
                NavigationControlAvailability::Disabled,
                NavigationControlAvailability::Disabled,
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
        let undo_status = self.removal_undo_status(now);
        let undo = undo_status.map(|status| RemovalUndoUiModel {
            kind_label: match status.kind {
                ControllerRemovalKind::Remove => "удаление элемента",
                ControllerRemovalKind::Clear => "очистку плейлиста",
                ControllerRemovalKind::RemoveOthers => "удаление остальных элементов",
            },
            seconds_remaining: status.seconds_remaining,
        });
        let next_wake_deadline = undo_status.map(|status| status.next_wake_deadline);
        PlaylistTransportUiModel {
            playlist_view_revision,
            previous,
            next,
            global_status,
            undo,
            next_wake_deadline,
        }
    }
}
