//! Exact process-lifetime projection активной media для MPRIS и других внешних controls.

use desktop_integration::DesktopMetadata;
use player_core::{MediaInstanceId, PlayerSnapshot};
use playlist_core::{PlaylistEntry, PlaylistEntryId, PlaylistItemId, QueueRevisionSnapshot};

use super::controller::PlaylistController;
use super::{PlaylistBindingGeneration, PlaylistRuntime, PlaylistRuntimeLifecycle};

/// Ограничивает один внешний title, чтобы metadata update не раздувал D-Bus payload.
const MAX_EXTERNAL_TITLE_CHARS: usize = 512;
/// Ограничивает compound context независимо от размера service metadata.
const MAX_COMPOUND_CONTEXT_CHARS: usize = 192;

/// Exact app-owned binding без D-Bus типов и без раскрытия queue storage.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ExternalPlaybackBinding {
    queue_revision: QueueRevisionSnapshot,
    active_part_item_id: Option<PlaylistItemId>,
    media_instance_id: MediaInstanceId,
    player_binding_generation: PlaylistBindingGeneration,
}

/// Полный read model с owned metadata; production строит metadata лениво через
/// [`ExternalPlaybackIdentity::build_metadata`], а целиком projection нужна focused-тестам.
#[cfg(test)]
pub(super) struct ExternalPlaybackProjection {
    pub(super) binding: ExternalPlaybackBinding,
    pub(super) active_part_item_id: Option<PlaylistItemId>,
    pub(super) metadata: DesktopMetadata,
}

/// Из чего строится metadata: только заимствованные срезы, ни одной аллокации.
///
/// Нужен, чтобы горячий путь публикации (каждый кадр) мог проверить binding и
/// сравнить входы metadata, не строя `String`-и: строки собираются только когда
/// входы реально изменились ([`ExternalPlaybackIdentity::build_metadata`]).
enum MetadataPlan<'a> {
    /// Одиночный media: title и label приходят из player snapshot-а.
    Single {
        player_title: Option<&'a str>,
        source_label: Option<&'a str>,
    },
    /// Exact part compound group: title части + bounded контекст группы.
    Compound {
        part_title: Option<&'a str>,
        group_title: Option<&'a str>,
        part_position: usize,
        part_count: usize,
    },
}

/// Дешёвая часть projection: exact binding + план metadata без строк.
pub(super) struct ExternalPlaybackIdentity<'a> {
    pub(super) binding: ExternalPlaybackBinding,
    pub(super) active_part_item_id: Option<PlaylistItemId>,
    pub(super) active_lineage: u64,
    metadata_plan: MetadataPlan<'a>,
}

impl<'a> ExternalPlaybackIdentity<'a> {
    /// Захватывает identity только для exact bound player и committed active part.
    pub(super) fn capture(
        runtime: &'a PlaylistRuntime,
        player_snapshot: &'a PlayerSnapshot,
    ) -> Option<Self> {
        let binding_generation = match runtime.lifecycle {
            PlaylistRuntimeLifecycle::Bound(binding) => binding.binding_generation(),
            PlaylistRuntimeLifecycle::Suspended
            | PlaylistRuntimeLifecycle::ShuttingDown
            | PlaylistRuntimeLifecycle::Shutdown => return None,
        };
        let controller = runtime.playlist_controller()?;
        capture_identity_for_controller(controller, binding_generation, player_snapshot)
    }

    /// Собирает bounded `DesktopMetadata` (здесь и только здесь выделяются строки).
    pub(super) fn build_metadata(&self) -> DesktopMetadata {
        match self.metadata_plan {
            MetadataPlan::Single {
                player_title,
                source_label,
            } => DesktopMetadata {
                track_key: None,
                title: bounded_non_empty(player_title, MAX_EXTERNAL_TITLE_CHARS),
                collection_context: None,
                source_label: source_label.map(str::to_owned),
                duration: None,
            },
            MetadataPlan::Compound {
                part_title,
                group_title,
                part_position,
                part_count,
            } => DesktopMetadata {
                track_key: None,
                title: bounded_non_empty(part_title, MAX_EXTERNAL_TITLE_CHARS),
                collection_context: Some(compound_context(group_title, part_position, part_count)),
                // Compound projection не публикует source locator/label: только owner metadata.
                source_label: None,
                duration: None,
            },
        }
    }

    /// Полная projection с owned metadata.
    #[cfg(test)]
    pub(super) fn into_projection(self) -> ExternalPlaybackProjection {
        let metadata = self.build_metadata();
        ExternalPlaybackProjection {
            binding: self.binding,
            active_part_item_id: self.active_part_item_id,
            metadata,
        }
    }
}

/// Общая pure projection после того, как process owner разрешил current binding generation.
#[cfg(test)]
fn capture_for_controller(
    controller: &PlaylistController,
    binding_generation: PlaylistBindingGeneration,
    player_snapshot: &PlayerSnapshot,
) -> Option<ExternalPlaybackProjection> {
    capture_identity_for_controller(controller, binding_generation, player_snapshot)
        .map(ExternalPlaybackIdentity::into_projection)
}

/// Проверяет binding и выбирает план metadata без аллокаций.
fn capture_identity_for_controller<'a>(
    controller: &'a PlaylistController,
    binding_generation: PlaylistBindingGeneration,
    player_snapshot: &'a PlayerSnapshot,
) -> Option<ExternalPlaybackIdentity<'a>> {
    let active_media = controller.active_media()?;
    if active_media.player_binding_generation() != binding_generation
        || player_snapshot.media_instance_id != Some(active_media.media_instance_id())
    {
        return None;
    }

    let active_part_item_id = active_media.item_id();
    let metadata_plan = match active_part_item_id {
        Some(item_id) => plan_playlist_metadata(controller, item_id, player_snapshot)?,
        None => MetadataPlan::Single {
            player_title: player_snapshot.media_title.as_deref(),
            source_label: player_snapshot.source_label.as_deref(),
        },
    };
    Some(ExternalPlaybackIdentity {
        binding: ExternalPlaybackBinding {
            queue_revision: controller.queue().revision_snapshot(),
            active_part_item_id,
            media_instance_id: active_media.media_instance_id(),
            player_binding_generation: binding_generation,
        },
        active_part_item_id,
        active_lineage: active_media.lineage_id().expose_value_for_correlation(),
        metadata_plan,
    })
}

/// Проверяет, что command всё ещё относится к тому же queue/current/player binding.
pub(super) fn capture_binding(
    runtime: &PlaylistRuntime,
    player_snapshot: &PlayerSnapshot,
) -> Option<ExternalPlaybackBinding> {
    ExternalPlaybackIdentity::capture(runtime, player_snapshot).map(|identity| identity.binding)
}

/// Выбирает план для exact playlist part и запрещает header-у стать fake track.
fn plan_playlist_metadata<'a>(
    controller: &'a PlaylistController,
    active_part_item_id: PlaylistItemId,
    player_snapshot: &'a PlayerSnapshot,
) -> Option<MetadataPlan<'a>> {
    let queue = controller.queue();
    if queue.traversal_current().map(|current| current.item_id()) != Some(active_part_item_id) {
        return None;
    }
    let active_item = queue.item(active_part_item_id)?;
    let entry_id = queue.structural_entry_id_for_item(active_part_item_id)?;
    match entry_id {
        PlaylistEntryId::Single(_) => Some(MetadataPlan::Single {
            player_title: player_snapshot.media_title.as_deref(),
            source_label: player_snapshot.source_label.as_deref(),
        }),
        PlaylistEntryId::Compound(_) => {
            let PlaylistEntry::Compound(group) = queue.top_level_entry(entry_id)? else {
                return None;
            };
            let part_count = group.retained_part_count();
            let part_position = group
                .parts()
                .position(|part| part.item().item_id() == active_part_item_id)?
                .saturating_add(1);
            Some(MetadataPlan::Compound {
                part_title: active_item
                    .cached_metadata()
                    .title()
                    .or(player_snapshot.media_title.as_deref()),
                group_title: group.cached_summary().title(),
                part_position,
                part_count,
            })
        }
    }
}

/// Формирует bounded group context без fallback locator-а.
fn compound_context(group_title: Option<&str>, part_position: usize, part_count: usize) -> String {
    let position = format!("Part {part_position}/{part_count}");
    let full_context = group_title
        .filter(|title| !title.trim().is_empty())
        .map_or(position.clone(), |title| {
            format!("{} · {position}", title.trim())
        });
    bound_chars(&full_context, MAX_COMPOUND_CONTEXT_CHARS)
}

/// Отбрасывает пустую metadata и копирует не больше заданного числа Unicode scalar values.
fn bounded_non_empty(value: Option<&str>, max_chars: usize) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| bound_chars(value, max_chars))
}

/// Обрезает строку только на char boundary и помечает фактическое усечение многоточием.
fn bound_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let retained_chars = max_chars.saturating_sub(1);
    let mut bounded = value.chars().take(retained_chars).collect::<String>();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests;
