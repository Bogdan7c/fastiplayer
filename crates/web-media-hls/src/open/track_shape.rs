//! Политика формы главных дорожек HLS: сколько video/audio дорожек обязан дать demux.
//!
//! Модуль владеет единственным правилом сопоставления `HlsMainTrackLayoutIntent` с фактическим
//! набором дорожек открытого demuxer-а и типом ошибки несовпадения. Инварианты:
//! - проверяется полная кардинальность video/audio, а не просто наличие нужного kind;
//! - `VideoWithOptionalAudio*` допускает видео без звука (A13-2), а вариант
//!   `...OrAudioOnly` ещё и радио без видео (A13-3): форму доказывает содержимое сегмента.

use media_core::TrackKind;

use crate::HlsMainTrackLayoutIntent;

/// Actual demux tracks нарушили explicit selected topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "HLS {component} track shape mismatch: expected {expected:?}, video={video_tracks}, audio={audio_tracks}"
)]
pub(crate) struct HlsTrackShapeError {
    component: &'static str,
    expected: HlsMainTrackLayoutIntent,
    video_tracks: usize,
    audio_tracks: usize,
}

/// Проверяет полную video/audio cardinality, а не только наличие требуемого kind.
pub(crate) fn validate_track_shape(
    tracks: &[media_core::TrackInfo],
    expected: HlsMainTrackLayoutIntent,
    component: &'static str,
) -> Result<(), HlsTrackShapeError> {
    let video_tracks = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .count();
    let audio_tracks = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .count();
    let matches = match expected {
        HlsMainTrackLayoutIntent::MuxedAv => video_tracks == 1 && audio_tracks == 1,
        HlsMainTrackLayoutIntent::VideoOnly => video_tracks == 1 && audio_tracks == 0,
        HlsMainTrackLayoutIntent::AudioOnly => video_tracks == 0 && audio_tracks == 1,
        HlsMainTrackLayoutIntent::VideoWithOptionalAudio => video_tracks == 1 && audio_tracks <= 1,
        HlsMainTrackLayoutIntent::VideoWithOptionalAudioOrAudioOnly => {
            (video_tracks == 1 && audio_tracks <= 1) || (video_tracks == 0 && audio_tracks == 1)
        }
    };
    if matches {
        Ok(())
    } else {
        Err(HlsTrackShapeError {
            component,
            expected,
            video_tracks,
            audio_tracks,
        })
    }
}
