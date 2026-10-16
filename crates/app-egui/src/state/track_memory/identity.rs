//! Идентичность «того же видео» для памяти выбора: из источника media в необратимый ключ.
//!
//! Ключ строится из ИСХОДНОГО (долговечного) локатора, а не из временных адресов потока:
//! локальный файл — точный путь; web — исходная ссылка (страница, манифест, ресурс).
//! Сама ссылка никуда не пишется: `VideoKey` хранит только SHA-256.
//!
//! Часть одного файла (окно воспроизведения: трек CUE-образа, составное видео) — отдельное
//! видео: в ключ входят границы окна.

use playlist_core::{LocalLocator, PlaylistLocator, SecretUrlLocator};
use playlist_state::{VideoKey, VideoKeyError, VideoSegment};

use crate::state::ActiveMediaSource;

/// Почему для источника нельзя построить ключ (память для него просто не работает).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoIdentityError {
    /// Платформа не отдаёт точные байты пути.
    UnsupportedPathEncoding,
    /// Исходная ссылка пуста.
    EmptyUrl,
}

/// Ключ видео для источника, как его открыл пользователь.
pub(crate) fn video_key_for_source(
    source: &ActiveMediaSource,
) -> Result<VideoKey, VideoIdentityError> {
    let locator = match source.physical_source() {
        ActiveMediaSource::LocalFile(path) => {
            PlaylistLocator::Local(LocalLocator::Native(path.clone()))
        }
        ActiveMediaSource::Web(intent) => PlaylistLocator::Url(
            SecretUrlLocator::from_reopenable_url(intent.original_url_for_track_memory())
                .map_err(|_| VideoIdentityError::EmptyUrl)?,
        ),
        ActiveMediaSource::PlaybackWindow { .. } => {
            // `physical_source` снимает обёртки окна: сюда попасть нельзя. Не паникуем,
            // а честно сообщаем, что идентичность недоступна.
            return Err(VideoIdentityError::UnsupportedPathEncoding);
        }
    };
    let segment = source.playback_window().map(|window| VideoSegment {
        start_nanos: window.start().as_duration().as_nanos(),
        end_nanos: window
            .end_exclusive()
            .map(|end| end.as_duration().as_nanos()),
    });
    VideoKey::for_locator_segment(&locator, segment).map_err(
        |VideoKeyError::UnsupportedNativePathEncoding| VideoIdentityError::UnsupportedPathEncoding,
    )
}
