//! Отпечаток описания дорожек показанного медиа: признак «смена прошла в том же поколении».
//!
//! YouTube меняет озвучку (и качество в одном `SourceBuffer`) новым init-сегментом внутри
//! ТОГО ЖЕ поколения потока: нового `MediaSource` нет, позиция и так та же, а плеер
//! переходит на новое описание дорожек «эстафетой» 03d на границе показа. Снимок плеера в
//! этот момент сразу показывает новый список дорожек (`replace_track_list_keeping_decoders`).
//! Возврат на ту же секунду после смены варианта (решение 280) по этому признаку понимает,
//! что ждать нового поколения не нужно (`session_media::position_carry`).
//!
//! Отпечаток — хеш того, что описывает дорожку для человека и декодера: вид, кодек,
//! частота и каналы звука, размер кадра, язык и название. Длительность не входит: она
//! уточняется по ходу и сменой дорожки не является. Отпечаток живёт только в памяти процесса
//! (хешер стандартной библиотеки), на диск и в журнал не попадает.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use player_core::{PlayerSnapshot, TrackSummarySnapshot};

use super::MediaLoadTicket;

/// Хеш описания дорожек (сравнивается только с хешем того же процесса).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TrackLayoutStamp(u64);

impl TrackLayoutStamp {
    /// Отпечаток списка дорожек снимка.
    pub(crate) fn of_tracks(tracks: &[TrackSummarySnapshot]) -> Self {
        let mut hasher = DefaultHasher::new();
        tracks.len().hash(&mut hasher);
        for track in tracks {
            hash_track(track, &mut hasher);
        }
        Self(hasher.finish())
    }

    /// Отпечаток с заданным числом (тестам правил возврата: им нужны «разные» описания).
    #[cfg(test)]
    pub(crate) const fn for_test(raw: u64) -> Self {
        Self(raw)
    }
}

/// Описание дорожек медиа, которое плеер сейчас показывает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ShownTrackLayout {
    /// Неизвестно: плеер ничего не показывает или показывает чужое (прежнее) медиа.
    #[default]
    Unknown,
    /// Медиа загрузки `load` показано с таким описанием дорожек.
    Shown {
        /// Чья загрузка показана.
        load: MediaLoadTicket,
        /// Отпечаток её дорожек.
        stamp: TrackLayoutStamp,
    },
}

impl ShownTrackLayout {
    /// Описание дорожек снимка, если его медиа принадлежит загрузке `shown_load`.
    pub(crate) fn from_snapshot(
        shown_load: Option<MediaLoadTicket>,
        snapshot: &PlayerSnapshot,
    ) -> Self {
        match shown_load {
            Some(load) if !snapshot.tracks.is_empty() => Self::Shown {
                load,
                stamp: TrackLayoutStamp::of_tracks(&snapshot.tracks),
            },
            _ => Self::Unknown,
        }
    }
}

fn hash_track(track: &TrackSummarySnapshot, hasher: &mut DefaultHasher) {
    track.kind.hash(hasher);
    track.codec_id.hash(hasher);
    track.sample_rate.hash(hasher);
    track.channels.hash(hasher);
    let picture = track
        .video
        .as_ref()
        .map(|video| (video.coded_width, video.coded_height));
    picture.hash(hasher);
    track.labels.language.hash(hasher);
    track.labels.title.hash(hasher);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{TrackId, TrackKind, TrackLabels, TrackLanguage};

    use super::*;

    fn audio(language: &str) -> TrackSummarySnapshot {
        TrackSummarySnapshot {
            id: TrackId::new(1),
            kind: TrackKind::Audio,
            codec_id: "A_AAC".to_owned(),
            sample_rate: Some(48_000),
            channels: Some(2),
            duration: Some(Duration::from_secs(60)),
            video: None,
            video_color_summary: None,
            labels: TrackLabels {
                language: Some(TrackLanguage::new(language)),
                ..TrackLabels::default()
            },
        }
    }

    #[test]
    fn another_dub_changes_the_stamp_and_a_longer_duration_does_not() {
        let original = TrackLayoutStamp::of_tracks(&[audio("eng")]);
        assert_ne!(original, TrackLayoutStamp::of_tracks(&[audio("ukr")]));
        let mut longer = audio("eng");
        longer.duration = Some(Duration::from_secs(61));
        assert_eq!(original, TrackLayoutStamp::of_tracks(&[longer]));
    }
}
