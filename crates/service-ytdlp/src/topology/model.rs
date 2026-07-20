//! Public value model bounded `yt-dlp` topology extraction.

use std::fmt;
use std::time::Duration;

use crate::YtDlpMediaLocator;

/// Service-owned bounded topology одного URL.
#[derive(Clone, PartialEq, Eq)]
pub enum YtDlpTopology {
    /// Обычный single video result.
    Video(YtDlpTopologyVideo),
    /// Ordered collection без compound ownership.
    Playlist(YtDlpTopologyCollection),
    /// Один compound root с ordered parts.
    MultiVideo(YtDlpTopologyMultiVideo),
    /// Сериализуемая URL delegation.
    Delegation(YtDlpTopologyDelegation),
}

impl YtDlpTopology {
    /// Возвращает стабильный result kind без раскрытия metadata.
    #[must_use]
    pub const fn kind(&self) -> YtDlpTopologyKind {
        match self {
            Self::Video(_) => YtDlpTopologyKind::Video,
            Self::Playlist(_) => YtDlpTopologyKind::Playlist,
            Self::MultiVideo(_) => YtDlpTopologyKind::MultiVideo,
            Self::Delegation(_) => YtDlpTopologyKind::Delegation,
        }
    }

    /// Возвращает video payload, когда root действительно является video.
    #[must_use]
    pub const fn as_video(&self) -> Option<&YtDlpTopologyVideo> {
        match self {
            Self::Video(video) => Some(video),
            _ => None,
        }
    }

    /// Возвращает playlist payload без flatten/copy.
    #[must_use]
    pub const fn as_playlist(&self) -> Option<&YtDlpTopologyCollection> {
        match self {
            Self::Playlist(collection) => Some(collection),
            _ => None,
        }
    }

    /// Возвращает compound payload без flatten/copy.
    #[must_use]
    pub const fn as_multi_video(&self) -> Option<&YtDlpTopologyMultiVideo> {
        match self {
            Self::MultiVideo(multi_video) => Some(multi_video),
            _ => None,
        }
    }

    /// Возвращает delegation payload без раскрытия target locator.
    #[must_use]
    pub const fn as_delegation(&self) -> Option<&YtDlpTopologyDelegation> {
        match self {
            Self::Delegation(delegation) => Some(delegation),
            _ => None,
        }
    }
}

impl fmt::Debug for YtDlpTopology {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopology")
            .field("kind", &self.kind())
            .field("entry_count", &topology_entry_count(self))
            .finish()
    }
}

/// Root topology discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpTopologyKind {
    /// Missing `_type` либо explicit `video`.
    Video,
    /// `_type = "playlist"`.
    Playlist,
    /// `_type = "multi_video"`.
    MultiVideo,
    /// `_type = "url"` либо `"url_transparent"`.
    Delegation,
}

/// Video node без ephemeral formats/direct endpoints.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpTopologyVideo {
    identity: YtDlpTopologyIdentity,
    metadata: YtDlpTopologyMetadata,
}

impl YtDlpTopologyVideo {
    /// Возвращает extractor identity/provenance.
    #[must_use]
    pub const fn identity(&self) -> &YtDlpTopologyIdentity {
        &self.identity
    }

    /// Возвращает bounded display metadata.
    #[must_use]
    pub const fn metadata(&self) -> &YtDlpTopologyMetadata {
        &self.metadata
    }

    pub(crate) fn new(identity: YtDlpTopologyIdentity, metadata: YtDlpTopologyMetadata) -> Self {
        Self { identity, metadata }
    }
}

impl fmt::Debug for YtDlpTopologyVideo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyVideo")
            .field("has_identity", &!self.identity.is_missing())
            .field("has_title", &self.metadata.title.is_some())
            .finish()
    }
}

/// Ordered playlist topology.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpTopologyCollection {
    identity: YtDlpTopologyIdentity,
    metadata: YtDlpTopologyMetadata,
    entries: Vec<YtDlpTopologyEntry>,
}

impl YtDlpTopologyCollection {
    /// Возвращает collection identity/provenance.
    #[must_use]
    pub const fn identity(&self) -> &YtDlpTopologyIdentity {
        &self.identity
    }

    /// Возвращает root summary metadata.
    #[must_use]
    pub const fn metadata(&self) -> &YtDlpTopologyMetadata {
        &self.metadata
    }

    /// Итерирует source-order entries без обещания другого storage API.
    pub fn iter_entries(&self) -> impl ExactSizeIterator<Item = &YtDlpTopologyEntry> {
        self.entries.iter()
    }

    /// Возвращает source-order entry count.
    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn new(
        identity: YtDlpTopologyIdentity,
        metadata: YtDlpTopologyMetadata,
        entries: Vec<YtDlpTopologyEntry>,
    ) -> Self {
        Self {
            identity,
            metadata,
            entries,
        }
    }
}

impl fmt::Debug for YtDlpTopologyCollection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyCollection")
            .field("has_identity", &!self.identity.is_missing())
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

/// Compound `_type = "multi_video"` topology.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpTopologyMultiVideo {
    root_video: YtDlpTopologyVideo,
    entries: Vec<YtDlpTopologyEntry>,
}

impl YtDlpTopologyMultiVideo {
    /// Возвращает validated root video summary/provenance.
    #[must_use]
    pub const fn root_video(&self) -> &YtDlpTopologyVideo {
        &self.root_video
    }

    /// Итерирует ordered compound parts.
    pub fn iter_entries(&self) -> impl ExactSizeIterator<Item = &YtDlpTopologyEntry> {
        self.entries.iter()
    }

    /// Возвращает source-order part count.
    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn new(root_video: YtDlpTopologyVideo, entries: Vec<YtDlpTopologyEntry>) -> Self {
        Self {
            root_video,
            entries,
        }
    }
}

impl fmt::Debug for YtDlpTopologyMultiVideo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyMultiVideo")
            .field("root_video", &self.root_video)
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

/// Один retained child entry.
#[derive(Clone, PartialEq, Eq)]
pub enum YtDlpTopologyEntry {
    /// Playable video descriptor.
    Video(YtDlpTopologyVideo),
    /// Nested collection.
    Playlist(YtDlpTopologyCollection),
    /// Nested compound media.
    MultiVideo(YtDlpTopologyMultiVideo),
    /// URL delegation с explicit merge policy.
    Delegation(YtDlpTopologyDelegation),
    /// Retained unavailable/missing child.
    Unavailable(YtDlpUnavailableTopologyEntry),
}

impl YtDlpTopologyEntry {
    /// Возвращает entry kind без раскрытия metadata.
    #[must_use]
    pub const fn kind(&self) -> YtDlpTopologyEntryKind {
        match self {
            Self::Video(_) => YtDlpTopologyEntryKind::Video,
            Self::Playlist(_) => YtDlpTopologyEntryKind::Playlist,
            Self::MultiVideo(_) => YtDlpTopologyEntryKind::MultiVideo,
            Self::Delegation(_) => YtDlpTopologyEntryKind::Delegation,
            Self::Unavailable(_) => YtDlpTopologyEntryKind::Unavailable,
        }
    }
}

impl fmt::Debug for YtDlpTopologyEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyEntry")
            .field("kind", &self.kind())
            .finish()
    }
}

/// Child entry discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpTopologyEntryKind {
    /// Video.
    Video,
    /// Nested playlist.
    Playlist,
    /// Nested compound.
    MultiVideo,
    /// Delegation.
    Delegation,
    /// Retained unavailable row.
    Unavailable,
}

/// Stable extractor identity и portable provenance без format endpoints.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpTopologyIdentity {
    extractor_id: Option<String>,
    extractor_key: Option<String>,
    webpage_locator: Option<YtDlpMediaLocator>,
    original_locator: Option<YtDlpMediaLocator>,
}

impl YtDlpTopologyIdentity {
    /// Возвращает bounded extractor-local ID.
    #[must_use]
    pub fn extractor_id(&self) -> Option<&str> {
        self.extractor_id.as_deref()
    }

    /// Возвращает bounded extractor key.
    #[must_use]
    pub fn extractor_key(&self) -> Option<&str> {
        self.extractor_key.as_deref()
    }

    /// Возвращает exact webpage locator через secret-safe typed boundary.
    #[must_use]
    pub const fn webpage_locator(&self) -> Option<&YtDlpMediaLocator> {
        self.webpage_locator.as_ref()
    }

    /// Возвращает exact original locator через secret-safe typed boundary.
    #[must_use]
    pub const fn original_locator(&self) -> Option<&YtDlpMediaLocator> {
        self.original_locator.as_ref()
    }

    /// Сообщает, что extractor не дал ни ID, ни durable locator.
    #[must_use]
    pub const fn is_missing(&self) -> bool {
        self.extractor_id.is_none()
            && self.webpage_locator.is_none()
            && self.original_locator.is_none()
    }

    pub(crate) fn new(
        extractor_id: Option<String>,
        extractor_key: Option<String>,
        webpage_locator: Option<YtDlpMediaLocator>,
        original_locator: Option<YtDlpMediaLocator>,
    ) -> Self {
        Self {
            extractor_id,
            extractor_key,
            webpage_locator,
            original_locator,
        }
    }
}

impl fmt::Debug for YtDlpTopologyIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyIdentity")
            .field("has_extractor_id", &self.extractor_id.is_some())
            .field("has_extractor_key", &self.extractor_key.is_some())
            .field("has_webpage_locator", &self.webpage_locator.is_some())
            .field("has_original_locator", &self.original_locator.is_some())
            .finish()
    }
}

/// Bounded metadata, не содержащая transport/request material.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpTopologyMetadata {
    title: Option<String>,
    description: Option<String>,
    duration: Option<Duration>,
}

impl YtDlpTopologyMetadata {
    /// Возвращает bounded title.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Возвращает bounded description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Возвращает finite non-negative duration hint.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub(crate) fn new(
        title: Option<String>,
        description: Option<String>,
        duration: Option<Duration>,
    ) -> Self {
        Self {
            title,
            description,
            duration,
        }
    }
}

impl fmt::Debug for YtDlpTopologyMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyMetadata")
            .field("has_title", &self.title.is_some())
            .field("has_description", &self.description.is_some())
            .field("duration", &self.duration)
            .finish()
    }
}

/// Сериализуемая URL delegation.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpTopologyDelegation {
    target: YtDlpMediaLocator,
    wrapper_metadata: YtDlpTopologyMetadata,
    merge_policy: YtDlpDelegationMetadataPolicy,
}

impl YtDlpTopologyDelegation {
    /// Возвращает exact target только через typed secret-safe locator.
    #[must_use]
    pub const fn target(&self) -> &YtDlpMediaLocator {
        &self.target
    }

    /// Возвращает wrapper metadata до merge.
    #[must_use]
    pub const fn wrapper_metadata(&self) -> &YtDlpTopologyMetadata {
        &self.wrapper_metadata
    }

    /// Возвращает upstream-defined merge policy.
    #[must_use]
    pub const fn merge_policy(&self) -> YtDlpDelegationMetadataPolicy {
        self.merge_policy
    }

    /// Применяет upstream distinction к уже разрешённой metadata.
    #[must_use]
    pub fn merge_resolved_metadata(
        &self,
        resolved_metadata: &YtDlpTopologyMetadata,
    ) -> YtDlpTopologyMetadata {
        match self.merge_policy {
            YtDlpDelegationMetadataPolicy::ResolvedResultOnly => resolved_metadata.clone(),
            YtDlpDelegationMetadataPolicy::TransparentWrapperPriority => {
                YtDlpTopologyMetadata::new(
                    self.wrapper_metadata
                        .title
                        .clone()
                        .or_else(|| resolved_metadata.title.clone()),
                    self.wrapper_metadata
                        .description
                        .clone()
                        .or_else(|| resolved_metadata.description.clone()),
                    self.wrapper_metadata
                        .duration
                        .or(resolved_metadata.duration),
                )
            }
        }
    }

    pub(crate) fn new(
        target: YtDlpMediaLocator,
        wrapper_metadata: YtDlpTopologyMetadata,
        merge_policy: YtDlpDelegationMetadataPolicy,
    ) -> Self {
        Self {
            target,
            wrapper_metadata,
            merge_policy,
        }
    }
}

impl fmt::Debug for YtDlpTopologyDelegation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpTopologyDelegation")
            .field("target", &self.target)
            .field("merge_policy", &self.merge_policy)
            .field("wrapper_metadata", &self.wrapper_metadata)
            .finish()
    }
}

/// Metadata merge semantics для двух official delegation result types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpDelegationMetadataPolicy {
    /// Обычный `url`: wrapper metadata не переопределяет resolved result.
    ResolvedResultOnly,
    /// `url_transparent`: non-null wrapper metadata имеет приоритет.
    TransparentWrapperPriority,
}

/// Retained unavailable child без fake playable identity.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpUnavailableTopologyEntry {
    identity: YtDlpTopologyIdentity,
    metadata: YtDlpTopologyMetadata,
    reason: YtDlpUnavailableTopologyReason,
}

impl YtDlpUnavailableTopologyEntry {
    /// Возвращает доступную stable identity, если extractor её сохранил.
    #[must_use]
    pub const fn identity(&self) -> &YtDlpTopologyIdentity {
        &self.identity
    }

    /// Возвращает доступную display metadata.
    #[must_use]
    pub const fn metadata(&self) -> &YtDlpTopologyMetadata {
        &self.metadata
    }

    /// Возвращает typed причину retention.
    #[must_use]
    pub const fn reason(&self) -> YtDlpUnavailableTopologyReason {
        self.reason
    }

    pub(crate) fn new(
        identity: YtDlpTopologyIdentity,
        metadata: YtDlpTopologyMetadata,
        reason: YtDlpUnavailableTopologyReason,
    ) -> Self {
        Self {
            identity,
            metadata,
            reason,
        }
    }
}

impl fmt::Debug for YtDlpUnavailableTopologyEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpUnavailableTopologyEntry")
            .field("has_identity", &!self.identity.is_missing())
            .field("reason", &self.reason)
            .finish()
    }
}

/// Причина retained unavailable entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpUnavailableTopologyReason {
    /// Upstream `entries` содержит `null`.
    NullEntry,
    /// Delegation сохранила identity, но не дала target URL.
    MissingDelegationTarget,
    /// `availability` явно не public.
    RestrictedAvailability,
    /// Entry не содержит stable identity.
    MissingIdentity,
}

fn topology_entry_count(topology: &YtDlpTopology) -> usize {
    match topology {
        YtDlpTopology::Video(_) | YtDlpTopology::Delegation(_) => 0,
        YtDlpTopology::Playlist(collection) => collection.entry_count(),
        YtDlpTopology::MultiVideo(multi_video) => multi_video.entry_count(),
    }
}
