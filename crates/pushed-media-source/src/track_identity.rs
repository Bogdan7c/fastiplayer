//! Стабильные ID дорожек полосы между эпизодами.
//!
//! Каждый эпизод — свежий демуксер со своими номерами дорожек. Плееру нужны
//! постоянные: выбранная звуковая дорожка, сборщик раздельных звука и видео
//! (`CompositeAvDemuxer`) и декодеры опираются на ID. Поэтому дорожка эпизода
//! получает публичный ID по «месту»: вид (видео, звук, субтитры) и порядковый
//! номер среди дорожек этого вида.
//!
//! Плеер получает `TracksChanged` только при настоящей смене описания (другой
//! кодек, разрешение, codec private): каждая такая смена пересобирает декодеры.

use std::collections::BTreeMap;

use media_core::{TrackId, TrackInfo, TrackKind};

/// Место дорожки в полосе: вид и порядковый номер среди дорожек этого вида.
type TrackSlot = (TrackKindKey, usize);

/// Ключ вида дорожки для упорядоченного словаря.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TrackKindKey {
    /// Видео.
    Video,
    /// Звук.
    Audio,
    /// Субтитры.
    Subtitle,
}

impl From<TrackKind> for TrackKindKey {
    fn from(kind: TrackKind) -> Self {
        match kind {
            TrackKind::Video => Self::Video,
            TrackKind::Audio => Self::Audio,
            TrackKind::Subtitle => Self::Subtitle,
        }
    }
}

/// Изменился ли опубликованный список дорожек.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackListChange {
    /// Описание то же: плееру сообщать нечего.
    Unchanged,
    /// Описание другое: плеер должен получить `TracksChanged`.
    Changed,
}

/// Публичные ID и опубликованный список дорожек полосы.
#[derive(Debug, Default)]
pub(crate) struct TrackIdentity {
    /// Публичный ID каждого места.
    public_ids: BTreeMap<TrackSlot, TrackId>,
    /// ID дорожек текущего эпизода → публичный ID.
    episode_mapping: BTreeMap<TrackId, TrackId>,
    /// Последний опубликованный список (с публичными ID).
    published: Option<Vec<TrackInfo>>,
}

impl TrackIdentity {
    /// Принимает список дорожек нового эпизода (или его `TracksChanged`).
    pub(crate) fn adopt(&mut self, episode_tracks: &[TrackInfo]) -> TrackListChange {
        let mut ordinals: BTreeMap<TrackKindKey, usize> = BTreeMap::new();
        let mut mapping = BTreeMap::new();
        let mut public_tracks = Vec::with_capacity(episode_tracks.len());
        for track in episode_tracks {
            let kind = TrackKindKey::from(track.kind);
            let ordinal = ordinals.entry(kind).or_default();
            let slot = (kind, *ordinal);
            *ordinal += 1;
            let public_id = self.public_id_for(slot, track.id);
            mapping.insert(track.id, public_id);
            let mut public_track = track.clone();
            public_track.id = public_id;
            // Длительность потока знает источник байтов, а не эпизод: поле дорожки
            // у разных init может отличаться и не должно пересоздавать декодеры.
            public_track.duration = None;
            public_tracks.push(public_track);
        }
        self.episode_mapping = mapping;
        if self.published.as_ref() == Some(&public_tracks) {
            return TrackListChange::Unchanged;
        }
        self.published = Some(public_tracks);
        TrackListChange::Changed
    }

    /// Публичный ID дорожки текущего эпизода.
    pub(crate) fn public_id(&self, episode_track: TrackId) -> Option<TrackId> {
        self.episode_mapping.get(&episode_track).copied()
    }

    /// Опубликованный список дорожек (пустой до первого эпизода).
    pub(crate) fn published(&self) -> &[TrackInfo] {
        self.published.as_deref().unwrap_or_default()
    }

    /// Был ли уже опубликован список.
    pub(crate) fn has_published(&self) -> bool {
        self.published.is_some()
    }

    /// ID места: уже выданный или новый. Первый эпизод сохраняет свои номера —
    /// так ID совпадают с тем, что показал бы обычный файл.
    fn public_id_for(&mut self, slot: TrackSlot, episode_id: TrackId) -> TrackId {
        if let Some(existing) = self.public_ids.get(&slot) {
            return *existing;
        }
        let taken = |candidate: TrackId| self.public_ids.values().any(|id| *id == candidate);
        let public_id = if taken(episode_id) {
            let highest = self
                .public_ids
                .values()
                .map(|id| id.get())
                .max()
                .unwrap_or(0);
            TrackId::new(highest.saturating_add(1))
        } else {
            episode_id
        };
        self.public_ids.insert(slot, public_id);
        public_id
    }
}

#[cfg(test)]
mod tests {
    use media_core::{TrackId, TrackInfo, TrackKind};

    use super::{TrackIdentity, TrackListChange};

    fn track(id: u32, kind: TrackKind, codec: &str) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(id),
            kind,
            codec_id: codec.to_owned(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: None,
            channels: None,
            video: None,
            labels: Default::default(),
        }
    }

    #[test]
    fn same_description_in_new_episode_is_unchanged_and_keeps_ids() {
        let mut identity = TrackIdentity::default();
        let first = [
            track(1, TrackKind::Video, "V_VP9"),
            track(2, TrackKind::Audio, "A_OPUS"),
        ];
        assert_eq!(identity.adopt(&first), TrackListChange::Changed);
        assert_eq!(identity.adopt(&first), TrackListChange::Unchanged);
        assert_eq!(identity.public_id(TrackId::new(2)), Some(TrackId::new(2)));
        assert_eq!(identity.published().len(), 2);
    }

    #[test]
    fn renumbered_episode_maps_back_to_first_ids_by_kind_and_order() {
        let mut identity = TrackIdentity::default();
        identity.adopt(&[
            track(1, TrackKind::Video, "V_VP9"),
            track(2, TrackKind::Audio, "A_OPUS"),
        ]);
        // Новый эпизод отдал звук первым и с другими номерами.
        let change = identity.adopt(&[
            track(7, TrackKind::Audio, "A_OPUS"),
            track(9, TrackKind::Video, "V_VP9"),
        ]);
        assert_eq!(identity.public_id(TrackId::new(9)), Some(TrackId::new(1)));
        assert_eq!(identity.public_id(TrackId::new(7)), Some(TrackId::new(2)));
        // Порядок в списке другой — это честная смена для плеера.
        assert_eq!(change, TrackListChange::Changed);
    }

    #[test]
    fn codec_change_is_changed_with_the_same_public_id() {
        let mut identity = TrackIdentity::default();
        identity.adopt(&[track(1, TrackKind::Video, "V_MPEG4/ISO/AVC")]);
        assert_eq!(
            identity.adopt(&[track(1, TrackKind::Video, "V_VP9")]),
            TrackListChange::Changed
        );
        assert_eq!(identity.published()[0].id, TrackId::new(1));
        assert_eq!(identity.published()[0].codec_id, "V_VP9");
    }

    #[test]
    fn colliding_new_slot_gets_a_fresh_id() {
        let mut identity = TrackIdentity::default();
        identity.adopt(&[track(1, TrackKind::Video, "V_VP9")]);
        identity.adopt(&[track(1, TrackKind::Audio, "A_OPUS")]);
        assert_eq!(identity.public_id(TrackId::new(1)), Some(TrackId::new(2)));
        assert!(identity.has_published());
    }
}
