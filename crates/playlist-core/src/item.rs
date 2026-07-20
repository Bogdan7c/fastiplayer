//! Playlist item records и ID-less mutation drafts.

use std::fmt;
use std::sync::Arc;

use crate::{
    CachedPlaylistMetadata, LocalLocator, LocalSourceFingerprint, PlaylistItemId, PlaylistLocator,
    SecretUrlLocator,
};

/// ID-less вход обычного add/replace domain boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistItemDraft {
    locator: PlaylistLocator,
    local_fingerprint: Option<LocalSourceFingerprint>,
    cached_metadata: CachedPlaylistMetadata,
}

impl PlaylistItemDraft {
    /// Создаёт local draft с optional best-effort fingerprint.
    pub fn local(
        locator: LocalLocator,
        local_fingerprint: Option<LocalSourceFingerprint>,
        cached_metadata: CachedPlaylistMetadata,
    ) -> Self {
        Self {
            locator: PlaylistLocator::Local(locator),
            local_fingerprint,
            cached_metadata,
        }
    }

    /// Создаёт URL draft без выдуманного filesystem fingerprint.
    pub fn url(locator: SecretUrlLocator, cached_metadata: CachedPlaylistMetadata) -> Self {
        Self {
            locator: PlaylistLocator::Url(locator),
            local_fingerprint: None,
            cached_metadata,
        }
    }

    /// Возвращает locator read-only и не раскрывает URL secret форматированием.
    pub fn locator(&self) -> &PlaylistLocator {
        &self.locator
    }

    /// Возвращает local fingerprint, если I/O boundary смог его получить.
    pub const fn local_fingerprint(&self) -> Option<LocalSourceFingerprint> {
        self.local_fingerprint
    }

    /// Возвращает полный display/sort cache.
    pub fn cached_metadata(&self) -> &CachedPlaylistMetadata {
        &self.cached_metadata
    }

    /// Публикует draft вместе с allocator-owned ID только внутри queue commit.
    pub(crate) fn into_item(self, item_id: PlaylistItemId) -> PlaylistItem {
        PlaylistItem {
            item_id,
            local_fingerprint: self.local_fingerprint,
            payload: Arc::new(PlaylistItemPayload {
                locator: self.locator,
                cached_metadata: self.cached_metadata,
            }),
        }
    }
}

impl fmt::Debug for PlaylistItemDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistItemDraft")
            .field("locator", &self.locator)
            .field("local_fingerprint", &self.local_fingerprint)
            .field("cached_metadata", &self.cached_metadata)
            .finish()
    }
}

/// Committed playable identity: top-level Single либо subordinate compound part.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistItem {
    item_id: PlaylistItemId,
    local_fingerprint: Option<LocalSourceFingerprint>,
    /// Тяжёлый locator/metadata payload разделяется с runtime-only Undo snapshot.
    payload: Arc<PlaylistItemPayload>,
}

/// Неизменяемая часть строки, которую removal snapshot не клонирует глубоко.
#[derive(Clone, PartialEq, Eq)]
struct PlaylistItemPayload {
    locator: PlaylistLocator,
    cached_metadata: CachedPlaylistMetadata,
}

impl PlaylistItem {
    /// Возвращает stable row identity.
    pub const fn item_id(&self) -> PlaylistItemId {
        self.item_id
    }

    /// Возвращает persisted/open locator read-only.
    pub fn locator(&self) -> &PlaylistLocator {
        &self.payload.locator
    }

    /// Возвращает optional local source fingerprint.
    pub const fn local_fingerprint(&self) -> Option<LocalSourceFingerprint> {
        self.local_fingerprint
    }

    /// Возвращает immutable cached metadata.
    pub fn cached_metadata(&self) -> &CachedPlaylistMetadata {
        &self.payload.cached_metadata
    }

    /// Атомарно заменяет local freshness fingerprint и связанный metadata cache.
    pub(crate) fn replace_local_cache(
        &mut self,
        local_fingerprint: Option<LocalSourceFingerprint>,
        metadata: CachedPlaylistMetadata,
    ) {
        self.local_fingerprint = local_fingerprint;
        Arc::make_mut(&mut self.payload).cached_metadata = metadata;
    }

    /// Проверяет shared payload без раскрытия locator/metadata наружу.
    #[cfg(test)]
    pub(crate) fn shares_payload_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.payload, &other.payload)
    }
}

impl fmt::Debug for PlaylistItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistItem")
            .field("item_id", &self.item_id)
            .field("locator", &self.payload.locator)
            .field("local_fingerprint", &self.local_fingerprint)
            .field("cached_metadata", &self.payload.cached_metadata)
            .finish()
    }
}

/// Persistence-facing restored record с уже сохранённым stable Item ID.
#[derive(Clone, PartialEq, Eq)]
pub struct RestoredPlaylistItem {
    item_id: PlaylistItemId,
    draft: PlaylistItemDraft,
}

impl RestoredPlaylistItem {
    /// Связывает validated persisted ID с ID-less item payload.
    pub fn new(item_id: PlaylistItemId, draft: PlaylistItemDraft) -> Self {
        Self { item_id, draft }
    }

    /// Возвращает restored stable identity для queue restore validation.
    pub const fn item_id(&self) -> PlaylistItemId {
        self.item_id
    }

    /// Превращает validated restore record в committed item.
    pub(crate) fn into_item(self) -> PlaylistItem {
        self.draft.into_item(self.item_id)
    }
}

impl fmt::Debug for RestoredPlaylistItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestoredPlaylistItem")
            .field("item_id", &self.item_id)
            .field("draft", &self.draft)
            .finish()
    }
}
