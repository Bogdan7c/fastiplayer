//! Runtime-identities playlist controller-а, независимые от canonical queue cursor-а.

use std::num::NonZeroU64;
use std::sync::Arc;

use player_core::{MediaInstanceId, PlaybackIntentRevision};
use playlist_core::PlaylistItemId;

use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::PlaylistBindingGeneration;
#[cfg(test)]
use crate::playlist_runtime::{PlaylistLifecycleGeneration, PlaylistRuntimeBinding};

#[cfg(test)]
impl PlaylistRuntimeBinding {
    pub(crate) const fn for_test(lifecycle_generation: u64, binding_generation: u64) -> Self {
        Self {
            lifecycle_generation: PlaylistLifecycleGeneration(lifecycle_generation),
            binding_generation: PlaylistBindingGeneration(binding_generation),
        }
    }
}

/// Runtime badge не удерживает неограниченный service/backend text.
const MAX_RUNTIME_ERROR_SUMMARY_CHARS: usize = 240;

/// Process-lifetime identity одного логического active media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ActiveMediaLineageId(NonZeroU64);

impl ActiveMediaLineageId {
    /// Создаёт identity только из controller-owned checked allocator-а.
    pub(super) const fn from_non_zero(identity: NonZeroU64) -> Self {
        Self(identity)
    }

    /// Возвращает число только для diagnostics и focused tests.
    pub(crate) const fn expose_value_for_correlation(self) -> u64 {
        self.0.get()
    }

    #[cfg(test)]
    pub(crate) const fn get(self) -> u64 {
        self.expose_value_for_correlation()
    }
}

/// Active identity не выводится ни из selection, ни из traversal current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveMediaIdentity {
    item_id: Option<PlaylistItemId>,
    lineage_id: ActiveMediaLineageId,
    media_instance_id: MediaInstanceId,
    player_binding_generation: PlaylistBindingGeneration,
}

impl ActiveMediaIdentity {
    /// Публикует exact identity после matching `Installed` и domain commit-а.
    pub(super) const fn installed(
        item_id: Option<PlaylistItemId>,
        lineage_id: ActiveMediaLineageId,
        media_instance_id: MediaInstanceId,
        player_binding_generation: PlaylistBindingGeneration,
    ) -> Self {
        Self {
            item_id,
            lineage_id,
            media_instance_id,
            player_binding_generation,
        }
    }

    pub(crate) const fn item_id(self) -> Option<PlaylistItemId> {
        self.item_id
    }

    pub(crate) const fn lineage_id(self) -> ActiveMediaLineageId {
        self.lineage_id
    }

    pub(crate) const fn media_instance_id(self) -> MediaInstanceId {
        self.media_instance_id
    }

    pub(crate) const fn player_binding_generation(self) -> PlaylistBindingGeneration {
        self.player_binding_generation
    }

    /// Отсоединяет active media от committed row, сохраняя exact lineage/instance/binding.
    pub(super) const fn detached(self) -> Self {
        Self {
            item_id: None,
            lineage_id: self.lineage_id,
            media_instance_id: self.media_instance_id,
            player_binding_generation: self.player_binding_generation,
        }
    }

    /// Возвращает tombstone той же lineage к восстановленной committed row без reopen.
    pub(super) const fn reattached(self, item_id: PlaylistItemId) -> Self {
        Self {
            item_id: Some(item_id),
            lineage_id: self.lineage_id,
            media_instance_id: self.media_instance_id,
            player_binding_generation: self.player_binding_generation,
        }
    }

    /// D72 меняет exact player instance/binding, но не app-owned lineage.
    pub(super) const fn rebound(
        self,
        media_instance_id: MediaInstanceId,
        player_binding_generation: PlaylistBindingGeneration,
    ) -> Self {
        Self {
            item_id: self.item_id,
            lineage_id: self.lineage_id,
            media_instance_id,
            player_binding_generation,
        }
    }
}

/// Источник intent-а нужен для diagnostics, но не сообщает coordinator-у priority policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingTargetOrigin {
    ExplicitRowPlay,
    ManualNavigation {
        origin: TransportActionOrigin,
    },
    ExplicitOpen,
    RestoredCurrent,
    ControlledResume,
    /// Automatic clean Ended/error traversal всегда стартует Playing.
    AutomaticAdvance,
}

/// Источник transport action остаётся typed до будущего UI/MPRIS wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportActionOrigin {
    /// Основной UI, keyboard shortcut или hardware media key.
    Ui,
    /// Process-lifetime MPRIS adapter; Stopped disposition сохраняется отдельно.
    Mpris,
    /// Trusted CLI/desktop startup intent до первого committed queue open.
    Startup,
}

/// Pending target остаётся отдельным от active/current до exact install commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingTarget {
    request_id: MediaOpenRequestId,
    item_id: Option<PlaylistItemId>,
    origin: PendingTargetOrigin,
    intent_revision: PlaybackIntentRevision,
}

impl PendingTarget {
    pub(super) const fn new(
        request_id: MediaOpenRequestId,
        item_id: Option<PlaylistItemId>,
        origin: PendingTargetOrigin,
        intent_revision: PlaybackIntentRevision,
    ) -> Self {
        Self {
            request_id,
            item_id,
            origin,
            intent_revision,
        }
    }

    pub(crate) const fn request_id(self) -> MediaOpenRequestId {
        self.request_id
    }

    pub(crate) const fn item_id(self) -> Option<PlaylistItemId> {
        self.item_id
    }

    pub(crate) const fn origin(self) -> PendingTargetOrigin {
        self.origin
    }

    pub(crate) const fn intent_revision(self) -> PlaybackIntentRevision {
        self.intent_revision
    }
}

/// Этап runtime-ошибки строки остаётся app-owned и не попадает в persistence DTO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistItemErrorPhase {
    Preparation,
    Install,
    Playback,
    SourceUnavailable,
}

/// Bounded категория ошибки без concrete backend/service details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistItemErrorCategory {
    Unavailable,
    Unsupported,
    Rejected,
    Runtime,
}

/// Одна latest runtime error запись на stable Item ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistItemRuntimeError {
    phase: PlaylistItemErrorPhase,
    category: PlaylistItemErrorCategory,
    safe_summary: Arc<str>,
    request_id: Option<MediaOpenRequestId>,
    media_instance_id: Option<MediaInstanceId>,
    occurrence_count: u32,
}

impl PlaylistItemRuntimeError {
    pub(super) fn first(
        phase: PlaylistItemErrorPhase,
        category: PlaylistItemErrorCategory,
        safe_summary: Arc<str>,
        request_id: Option<MediaOpenRequestId>,
        media_instance_id: Option<MediaInstanceId>,
    ) -> Self {
        Self {
            phase,
            category,
            safe_summary: bounded_safe_summary(safe_summary),
            request_id,
            media_instance_id,
            occurrence_count: 1,
        }
    }

    pub(super) fn replace_with_latest(
        &mut self,
        phase: PlaylistItemErrorPhase,
        category: PlaylistItemErrorCategory,
        safe_summary: Arc<str>,
        request_id: Option<MediaOpenRequestId>,
        media_instance_id: Option<MediaInstanceId>,
    ) {
        self.phase = phase;
        self.category = category;
        self.safe_summary = bounded_safe_summary(safe_summary);
        self.request_id = request_id;
        self.media_instance_id = media_instance_id;
        self.occurrence_count = self.occurrence_count.saturating_add(1);
    }

    pub(crate) const fn phase(&self) -> PlaylistItemErrorPhase {
        self.phase
    }

    pub(crate) const fn category(&self) -> PlaylistItemErrorCategory {
        self.category
    }

    pub(crate) fn safe_summary(&self) -> &str {
        &self.safe_summary
    }

    pub(crate) const fn request_id(&self) -> Option<MediaOpenRequestId> {
        self.request_id
    }

    pub(crate) const fn media_instance_id(&self) -> Option<MediaInstanceId> {
        self.media_instance_id
    }

    pub(crate) const fn occurrence_count(&self) -> u32 {
        self.occurrence_count
    }
}

fn bounded_safe_summary(summary: Arc<str>) -> Arc<str> {
    if summary
        .chars()
        .nth(MAX_RUNTIME_ERROR_SUMMARY_CHARS)
        .is_none()
    {
        return summary;
    }
    Arc::from(
        summary
            .chars()
            .take(MAX_RUNTIME_ERROR_SUMMARY_CHARS)
            .collect::<String>(),
    )
}
