//! Secret-safe read-only view model активной web-media конфигурации для URL sidebar.
//!
//! Доменная часть (generation fence, preference, конфигурация потока и
//! component variants) живёт в `media_source_open::web_media_stream_model` и
//! re-export-ится здесь под прежними путями. Этот модуль владеет только
//! UI-проекцией: моделью секции sidebar, её контроллером и действиями.

use std::sync::Arc;

use media_core::TimelineMode;
use player_core::{PlaybackState, PlayerSnapshot};

#[cfg(test)]
pub(crate) use media_source_open::web_media_stream_model::WebMediaContainerSummary;
pub(crate) use media_source_open::web_media_stream_model::component_variants;
pub(crate) use media_source_open::web_media_stream_model::{
    WebMediaCandidatePresentation, WebMediaSelectionPreference, WebMediaStreamConfiguration,
    WebMediaStreamGeneration,
};

use crate::media_open::ActiveMediaSource;
use crate::playlist_runtime::PlaylistViewModel;

use component_variants::WebMediaComponentVariantProjection;
mod sidebar_action;
pub(crate) use sidebar_action::{
    UrlSidebarAction, UrlSidebarPendingSelection, UrlSidebarTransitionError,
};
mod sidebar_controller;
pub(crate) use sidebar_controller::UrlSidebarController;
#[cfg(test)]
use sidebar_controller::{ItemOverrideState, SafeErrorState};

/// Контекст active queue binding, не дающий view прямого доступа к queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarItemScope {
    Detached,
    SingleItem,
    CompoundPart,
}

/// Вид потока, который sidebar может честно показать в строке «Режим».
///
/// Источник истины — `PlayerSnapshot::timeline.mode`: его публикует player после установки
/// media (live HLS/DASH/yt-dlp приходят как динамическая timeline). Sidebar сам live не
/// определяет и manifest-ы не читает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarStreamKind {
    /// Прямой эфир (динамическая live timeline).
    Live,
    /// Конечная запись.
    Vod,
    /// Media ещё не установлено (или уже сброшено): режим неизвестен, VOD не выдумываем.
    Unknown,
}

/// Возможность перемотки, как её сообщает player (а не контейнер или статический признак).
///
/// Источник истины — `PlayerSnapshot::timeline` (`seekable` + `mode`): ровно по нему
/// `PlayerSession` принимает или отклоняет команду seek (`NotSeekable` / `Expired`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarSeekAvailability {
    /// Media ещё не установлено: что-либо обещать рано.
    Unknown,
    /// Player сейчас не принимает seek (источник без seek, нет индекса или окна эфира).
    Unavailable,
    /// Конечная запись: допустима любая позиция в пределах длительности.
    Available,
    /// Live с DVR-окном: seek принимается только внутри окна, дальше player ответит
    /// «эта часть эфира уже недоступна».
    WithinLiveWindow,
}

/// Конечный playback status, который URL view может показать без worker handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UrlSidebarPlaybackStatus {
    pub(crate) stream_kind: UrlSidebarStreamKind,
    pub(crate) seek: UrlSidebarSeekAvailability,
    pub(crate) buffering: bool,
    pub(crate) refresh_on_reopen: bool,
}

/// Только bounded категории ошибок; произвольная error chain в UI-model не попадает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlSidebarSafeError {
    SourceUnavailable,
    SameItemSwitchBusy,
    SameItemSwitchStale,
    SameItemSwitchCancelled,
}

/// Модель одной секции существующего sidebar host-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlSidebarModel {
    Inactive,
    DirectMedia {
        ingress: web_media_core::WebMediaIngressKind,
        source_label: Arc<str>,
        status: UrlSidebarPlaybackStatus,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
    },
    CatalogBacked {
        generation: WebMediaStreamGeneration,
        source_label: Arc<str>,
        candidates: Arc<[WebMediaCandidatePresentation]>,
        active_candidate: WebMediaCandidatePresentation,
        pending_selection: Option<Box<UrlSidebarPendingSelection>>,
        component_variants: Box<WebMediaComponentVariantProjection>,
        preference: WebMediaSelectionPreference,
        item_scope: UrlSidebarItemScope,
        status: UrlSidebarPlaybackStatus,
        safe_error: Option<UrlSidebarSafeError>,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
        fallback_notice: bool,
    },
}

#[derive(Debug, Clone, Copy)]
struct UrlSidebarItemBinding {
    scope: UrlSidebarItemScope,
    item_id: Option<playlist_core::PlaylistItemId>,
}

enum UrlSidebarSourceProjection<'source> {
    Inactive,
    WebMedia {
        ingress: web_media_core::WebMediaIngressKind,
        source_label: &'source str,
        configuration: Option<&'source WebMediaStreamConfiguration>,
    },
}

impl UrlSidebarController {
    pub(crate) fn model_with_catalog(
        &self,
        active_source: Option<&ActiveMediaSource>,
        player_snapshot: &PlayerSnapshot,
        playlist_model: Option<&PlaylistViewModel>,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
        fallback_notice: bool,
    ) -> UrlSidebarModel {
        let source = match active_source.map(ActiveMediaSource::physical_source) {
            None | Some(ActiveMediaSource::LocalFile(_)) => UrlSidebarSourceProjection::Inactive,
            Some(ActiveMediaSource::Web(intent)) => {
                let projection = intent.read_only_projection();
                UrlSidebarSourceProjection::WebMedia {
                    ingress: projection.ingress,
                    source_label: projection.source_label,
                    configuration: projection.stream_configuration,
                }
            }
            Some(ActiveMediaSource::PlaybackWindow { .. }) => {
                unreachable!("physical_source removes playback-window wrappers")
            }
        };
        self.model_from_source_with_catalog(
            source,
            player_snapshot,
            item_binding(playlist_model),
            catalog,
            fallback_notice,
        )
    }

    fn model_from_source_with_catalog(
        &self,
        source: UrlSidebarSourceProjection<'_>,
        player_snapshot: &PlayerSnapshot,
        item_binding: UrlSidebarItemBinding,
        catalog: crate::web_media_catalog::WebMediaCatalogState,
        fallback_notice: bool,
    ) -> UrlSidebarModel {
        match source {
            UrlSidebarSourceProjection::Inactive => UrlSidebarModel::Inactive,
            UrlSidebarSourceProjection::WebMedia {
                ingress,
                source_label,
                configuration: None,
            } => UrlSidebarModel::DirectMedia {
                ingress,
                source_label: Arc::from(source_label),
                status: playback_status(player_snapshot, false),
                catalog,
            },
            UrlSidebarSourceProjection::WebMedia {
                source_label,
                configuration: Some(stream_configuration),
                ..
            } => {
                let generation = stream_configuration.generation();
                UrlSidebarModel::CatalogBacked {
                    generation,
                    source_label: Arc::from(source_label),
                    candidates: Arc::from(stream_configuration.candidates()),
                    active_candidate: stream_configuration.active_candidate().clone(),
                    pending_selection: self
                        .pending_selection
                        .as_ref()
                        .filter(|pending| pending.parent_generation() == generation)
                        .cloned()
                        .map(Box::new),
                    component_variants: Box::new(
                        stream_configuration.component_variant_projection(),
                    ),
                    preference: self
                        .item_override
                        .as_ref()
                        .filter(|item_override| {
                            item_override
                                .installed_generation
                                .has_same_source_lineage(generation)
                                && item_override.item_id == item_binding.item_id
                        })
                        .map(|item_override| {
                            WebMediaSelectionPreference::ItemOverride(
                                item_override.preferred_height,
                            )
                        })
                        .unwrap_or_else(|| stream_configuration.preference()),
                    item_scope: item_binding.scope,
                    status: playback_status(player_snapshot, true),
                    safe_error: self
                        .safe_error
                        .as_ref()
                        .filter(|error| error.generation == generation)
                        .map(|error| error.error)
                        .or_else(|| {
                            (player_snapshot.playback_state == PlaybackState::Failed)
                                .then_some(UrlSidebarSafeError::SourceUnavailable)
                        }),
                    catalog,
                    fallback_notice,
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn model(
        &self,
        active_source: Option<&ActiveMediaSource>,
        player_snapshot: &PlayerSnapshot,
        playlist_model: Option<&PlaylistViewModel>,
    ) -> UrlSidebarModel {
        self.model_with_catalog(
            active_source,
            player_snapshot,
            playlist_model,
            crate::web_media_catalog::WebMediaCatalogState::Inactive,
            false,
        )
    }

    #[cfg(test)]
    fn model_from_source(
        &self,
        source: UrlSidebarSourceProjection<'_>,
        player_snapshot: &PlayerSnapshot,
        item_binding: UrlSidebarItemBinding,
    ) -> UrlSidebarModel {
        self.model_from_source_with_catalog(
            source,
            player_snapshot,
            item_binding,
            crate::web_media_catalog::WebMediaCatalogState::Inactive,
            false,
        )
    }
}

fn playback_status(snapshot: &PlayerSnapshot, refresh_on_reopen: bool) -> UrlSidebarPlaybackStatus {
    UrlSidebarPlaybackStatus {
        stream_kind: stream_kind(snapshot),
        seek: seek_availability(snapshot),
        buffering: snapshot.playback_state == PlaybackState::Buffering,
        refresh_on_reopen,
    }
}

/// Режим потока из snapshot-а player-а. Без установленного media timeline лишь дефолтная
/// (`Static`), поэтому «VOD» по ней утверждать нельзя.
fn stream_kind(snapshot: &PlayerSnapshot) -> UrlSidebarStreamKind {
    if snapshot.media_instance_id.is_none() {
        return UrlSidebarStreamKind::Unknown;
    }
    match snapshot.timeline.mode {
        TimelineMode::Live => UrlSidebarStreamKind::Live,
        TimelineMode::Static => UrlSidebarStreamKind::Vod,
    }
}

/// Перемотка из `timeline` snapshot-а — тем же признаком, по которому player решает seek.
/// `MediaInfo::seekable` не годится: это свойство контейнера, а не текущего состояния
/// timeline (live-поток может быть «seekable» по контейнеру, но без окна эфира).
fn seek_availability(snapshot: &PlayerSnapshot) -> UrlSidebarSeekAvailability {
    if snapshot.media_instance_id.is_none() {
        return UrlSidebarSeekAvailability::Unknown;
    }
    match (snapshot.timeline.seekable, snapshot.timeline.mode) {
        (false, _) => UrlSidebarSeekAvailability::Unavailable,
        (true, TimelineMode::Static) => UrlSidebarSeekAvailability::Available,
        (true, TimelineMode::Live) => UrlSidebarSeekAvailability::WithinLiveWindow,
    }
}

fn item_binding(playlist_model: Option<&PlaylistViewModel>) -> UrlSidebarItemBinding {
    let Some(model) = playlist_model else {
        return UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::Detached,
            item_id: None,
        };
    };
    let Some(active_item_id) = model.active_item_id() else {
        return UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::Detached,
            item_id: None,
        };
    };
    let scope = match model
        .compound_snapshot()
        .structural_entry_id_for_item(active_item_id)
    {
        Some(playlist_core::PlaylistEntryId::Compound(_)) => UrlSidebarItemScope::CompoundPart,
        Some(playlist_core::PlaylistEntryId::Single(_)) => UrlSidebarItemScope::SingleItem,
        None => UrlSidebarItemScope::Detached,
    };
    UrlSidebarItemBinding {
        scope,
        item_id: Some(active_item_id),
    }
}

#[cfg(test)]
mod tests;
