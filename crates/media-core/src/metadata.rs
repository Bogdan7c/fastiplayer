//! Нейтральный snapshot статических и обновляемых metadata медиа.

/// Описание контейнера без зависимости от конкретного demux backend-а.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaContainerMetadata {
    /// Короткое user-facing имя формата, когда backend может назвать его точно.
    pub format_name: Option<String>,
}

/// Нормализованные теги, которые имеют практический смысл в UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaTagMetadata {
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
}

impl MediaTagMetadata {
    /// Upsert не стирает уже известные значения отсутствующими полями revision-а.
    pub fn upsert(&mut self, revision: Self) {
        if revision.title.is_some() {
            self.title = revision.title;
        }
        if !revision.artists.is_empty() {
            self.artists = revision.artists;
        }
        if revision.album.is_some() {
            self.album = revision.album;
        }
    }
}

/// Полный immutable snapshot metadata на текущий момент чтения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaMetadata {
    pub container: Option<MediaContainerMetadata>,
    pub tags: MediaTagMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_revision_is_upsert_instead_of_replacement() {
        let mut tags = MediaTagMetadata {
            title: Some("Title".into()),
            artists: vec!["Artist".into()],
            album: None,
        };
        tags.upsert(MediaTagMetadata {
            album: Some("Album".into()),
            ..Default::default()
        });
        assert_eq!(tags.title.as_deref(), Some("Title"));
        assert_eq!(tags.artists, ["Artist"]);
        assert_eq!(tags.album.as_deref(), Some("Album"));
    }
}
