//! Транспорт субтитров HLS: rendition-ы `TYPE=SUBTITLES`, их media playlist и сегменты.
//!
//! Владелец состояния - [`HlsSubtitleTransport`]: он хранит descriptor-ы выбранной группы,
//! базу master playlist и HTTP-контекст открытого источника. Открытие видео/звука его НЕ
//! вызывает: сеть трогается только когда потребитель просит playlist или сегмент.
//!
//! Граница с потребителем (слой `media-source-open::stream_subtitles`):
//! - [`HlsSubtitleTransport::renditions`] - безопасный список дорожек;
//! - [`HlsSubtitleTransport::load_playlist`] - снимок media playlist (лёгкий профиль);
//! - [`HlsSubtitleTransport::fetch_segment`] - байты одного сегмента под лимитом вызывающего.
//!
//! Ни один тип этой границы не раскрывает URL/query в `Debug`/`Display`.

mod error;
mod playlist;
mod timeline;
mod transport;

pub use error::{HlsSubtitleFetchError, HlsSubtitleUnsupportedFeature};
pub use playlist::{HlsSubtitlePlaylistSnapshot, HlsSubtitleSegment, HlsSubtitleSegmentRef};
pub use timeline::PlaylistTimelineEpoch;
pub use transport::{
    HlsSubtitleRenditionId, HlsSubtitleRenditionInfo, HlsSubtitleSegmentByteLimit,
    HlsSubtitleTransport,
};
