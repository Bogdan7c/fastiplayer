//! Bounded XSPF result model без queue IDs и service admission.

use std::fmt;
use std::num::NonZeroU32;

use media_core::{MediaDuration, TrackNumber};

/// Namespace-resolved и percent-encoded location candidate.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct XspfLocationCandidate {
    /// URL serialization хранится private, чтобы Debug не раскрыл secret-bearing URI.
    serialized_uri: String,
}

impl XspfLocationCandidate {
    /// Parser создаёт candidate только после URI/base validation.
    pub(crate) fn new(serialized_uri: String) -> Self {
        Self { serialized_uri }
    }

    /// Явно раскрывает URI только будущему app admission boundary.
    pub fn expose_uri_for_admission(&self) -> &str {
        &self.serialized_uri
    }
}

impl fmt::Debug for XspfLocationCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("XspfLocationCandidate(<redacted>)")
    }
}

/// Один parsed track сохраняет все location alternatives и metadata hints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XspfTrack {
    /// Candidate order совпадает с document order.
    location_candidates: Vec<XspfLocationCandidate>,
    /// Human-readable track title.
    title: Option<String>,
    /// XSPF creator становится одним ordered artist hint.
    creator: Option<String>,
    /// Human-readable album hint.
    album: Option<String>,
    /// Positive track ordinal hint.
    track_number: Option<TrackNumber>,
    /// Millisecond duration остаётся hint и не становится playback span.
    duration_hint: Option<MediaDuration>,
}

impl XspfTrack {
    /// Parser публикует полностью проверенный track одним commit-ом.
    pub(crate) fn new(
        location_candidates: Vec<XspfLocationCandidate>,
        title: Option<String>,
        creator: Option<String>,
        album: Option<String>,
        track_number: Option<TrackNumber>,
        duration_hint: Option<MediaDuration>,
    ) -> Self {
        Self {
            location_candidates,
            title,
            creator,
            album,
            track_number,
            duration_hint,
        }
    }

    /// Возвращает ordered candidates без выбора service-а.
    pub fn location_candidates(&self) -> &[XspfLocationCandidate] {
        &self.location_candidates
    }

    /// Возвращает optional title hint.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Возвращает optional creator hint.
    pub fn creator(&self) -> Option<&str> {
        self.creator.as_deref()
    }

    /// Возвращает optional album hint.
    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    /// Возвращает optional positive track ordinal.
    pub const fn track_number(&self) -> Option<TrackNumber> {
        self.track_number
    }

    /// Возвращает duration metadata hint, но не playback end.
    pub const fn duration_hint(&self) -> Option<MediaDuration> {
        self.duration_hint
    }
}

/// One-based flattened track index внутри XSPF document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct XspfTrackIndex(NonZeroU32);

impl XspfTrackIndex {
    /// Валидирует non-zero one-based index.
    pub(crate) fn new(value: u32) -> Option<Self> {
        NonZeroU32::new(value).map(Self)
    }

    /// Возвращает one-based document value.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Positive число flattened tracks в одном compound group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct XspfGroupTrackCount(NonZeroU32);

impl XspfGroupTrackCount {
    /// Валидирует positive group size.
    pub(crate) fn new(value: u32) -> Option<Self> {
        NonZeroU32::new(value).map(Self)
    }

    /// Возвращает exact child count.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Минимальная v1 group запись не дублирует данные каждого flattened track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XspfGroup {
    /// Первый flattened track группы.
    first_track: XspfTrackIndex,
    /// Число contiguous parts в source order.
    track_count: XspfGroupTrackCount,
    /// Durable group-root candidate проходит admission отдельно от parts.
    root_location: XspfLocationCandidate,
}

impl XspfGroup {
    /// Parser создаёт group только после range и schema validation.
    pub(crate) fn new(
        first_track: XspfTrackIndex,
        track_count: XspfGroupTrackCount,
        root_location: XspfLocationCandidate,
    ) -> Self {
        Self {
            first_track,
            track_count,
            root_location,
        }
    }

    /// Возвращает one-based начало flattened range.
    pub const fn first_track(&self) -> XspfTrackIndex {
        self.first_track
    }

    /// Возвращает positive длину flattened range.
    pub const fn track_count(&self) -> XspfGroupTrackCount {
        self.track_count
    }

    /// Возвращает root candidate без автоматического admission.
    pub const fn root_location(&self) -> &XspfLocationCandidate {
        &self.root_location
    }
}

/// Полный XSPF v1 preview без stable queue IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XspfPlaylist {
    /// Flattened XSPF tracks сохраняют document order.
    tracks: Vec<XspfTrack>,
    /// Fastiplayer compound ranges ссылаются на flattened order.
    groups: Vec<XspfGroup>,
}

impl XspfPlaylist {
    /// Parser публикует модель только после final group-range validation.
    pub(crate) fn new(tracks: Vec<XspfTrack>, groups: Vec<XspfGroup>) -> Self {
        Self { tracks, groups }
    }

    /// Возвращает flattened tracks в source order.
    pub fn tracks(&self) -> &[XspfTrack] {
        &self.tracks
    }

    /// Возвращает non-overlapping compound ranges.
    pub fn groups(&self) -> &[XspfGroup] {
        &self.groups
    }
}
