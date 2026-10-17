//! Заголовки субтитровых дорожек (`codec_private`), которые уходят в приложение.
//!
//! Для ASS/SSA в MKV заголовок (`[Script Info]`, стили) лежит в `CodecPrivate` дорожки.
//! Он НЕ кладётся в `TrackSummarySnapshot`: снимок клонируется очень часто, а заголовок
//! может весить сотни КБ. Вместо этого он один раз едет в `MediaStarted`.
//!
//! Ограничение размера: заголовок уже ограничен demuxer-ом (он читает `CodecPrivate`
//! как обычный элемент контейнера), а приложение дополнительно отбрасывает слишком
//! большой заголовок по `subtitles.max_ass_header_kb` (дорожка тогда идёт как простой
//! текст). Поэтому здесь отдельного лимита нет — труба не решает политику парсинга.
//!
//! Известное ограничение: дорожки, появившиеся позже через `TracksChanged`, заголовка
//! не получают и показываются как простой текст.

use bytes::Bytes;
use media_core::{TrackId, TrackInfo, TrackKind};

/// Заголовок одной субтитровой дорожки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrackHeader {
    /// Дорожка-владелец заголовка (тот же id, что у пакетов и снимка).
    pub track_id: TrackId,

    /// Сырые байты `codec_private` (для ASS — текст заголовка).
    pub codec_private: Bytes,
}

/// Собирает заголовки всех субтитровых дорожек, у которых `codec_private` не пуст.
///
/// Дорожки других видов и дорожки без заголовка пропускаются (SRT/WebVTT его не имеют).
/// `Bytes::clone` не копирует данные, поэтому вызов дёшев.
#[must_use]
pub(crate) fn collect_subtitle_track_headers(tracks: &[TrackInfo]) -> Vec<SubtitleTrackHeader> {
    tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Subtitle)
        .filter_map(|track| {
            let codec_private = track.codec_private.as_ref()?;
            if codec_private.is_empty() {
                return None;
            }
            Some(SubtitleTrackHeader {
                track_id: track.id,
                codec_private: codec_private.clone(),
            })
        })
        .collect()
}
