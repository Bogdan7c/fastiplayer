//! Playlist item records и ID-less mutation drafts.

use std::fmt;

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
            locator: self.locator,
            local_fingerprint: self.local_fingerprint,
            cached_metadata: self.cached_metadata,
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

/// Committed строка canonical очереди.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistItem {
    item_id: PlaylistItemId,
    locator: PlaylistLocator,
    local_fingerprint: Option<LocalSourceFingerprint>,
    cached_metadata: CachedPlaylistMetadata,
}

impl PlaylistItem {
    /// Возвращает stable row identity.
    pub const fn item_id(&self) -> PlaylistItemId {
        self.item_id
    }

    /// Возвращает persisted/open locator read-only.
    pub fn locator(&self) -> &PlaylistLocator {
        &self.locator
    }

    /// Возвращает optional local source fingerprint.
    pub const fn local_fingerprint(&self) -> Option<LocalSourceFingerprint> {
        self.local_fingerprint
    }

    /// Возвращает immutable cached metadata.
    pub fn cached_metadata(&self) -> &CachedPlaylistMetadata {
        &self.cached_metadata
    }

    /// Меняет только metadata cache после полного batch preflight.
    pub(crate) fn replace_cached_metadata(&mut self, metadata: CachedPlaylistMetadata) {
        self.cached_metadata = metadata;
    }
}

impl fmt::Debug for PlaylistItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistItem")
            .field("item_id", &self.item_id)
            .field("locator", &self.locator)
            .field("local_fingerprint", &self.local_fingerprint)
            .field("cached_metadata", &self.cached_metadata)
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
