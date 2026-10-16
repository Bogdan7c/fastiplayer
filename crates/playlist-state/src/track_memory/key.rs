//! Ключ видео в памяти дорожек: необратимый отпечаток точного локатора.

use std::fmt;

use playlist_core::PlaylistLocator;
use sha2::{Digest, Sha256};

use crate::locator_fingerprint::{
    UnsupportedNativePathEncoding, decode_lower_hex_32, encode_lower_hex,
    locator_fingerprint_with_domain,
};

/// Отдельный domain separator: отпечаток видео здесь не совпадёт с resume-отпечатком того же
/// локатора, и файлы нельзя «склеить» друг с другом по хэшу.
const TRACK_MEMORY_LOCATOR_DOMAIN: &[u8] = b"fastiplayer/track-memory/locator/v1";

/// Отдельный domain separator для ключа фрагмента файла: ключ окна не совпадёт ни с ключом
/// целого файла, ни с ключом окна другого домена.
const TRACK_MEMORY_SEGMENT_DOMAIN: &[u8] = b"fastiplayer/track-memory/segment/v1";

/// Фрагмент одного физического файла (например, часть CUE-образа или составного видео).
///
/// Нейтральный тип: crate ничего не знает о player-core. Границы — наносекунды абсолютного
/// времени файла; `end_nanos = None` — «до конца файла». Разные фрагменты одного файла — разные
/// видео для памяти дорожек.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSegment {
    /// Абсолютное начало фрагмента, нс.
    pub start_nanos: u128,
    /// Абсолютный эксклюзивный конец фрагмента, нс; `None` — до конца файла.
    pub end_nanos: Option<u128>,
}

/// Почему ключ построить нельзя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoKeyError {
    /// Платформа не отдаёт точные байты локального пути.
    UnsupportedNativePathEncoding,
}

impl fmt::Display for VideoKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("exact native path encoding is not available for video key")
    }
}

impl std::error::Error for VideoKeyError {}

/// SHA-256 точного локатора видео (локальный файл или URL).
///
/// Сам путь/URL не хранится, не попадает в `Debug`, ошибки и файл: URL может содержать токены.
/// Тот же файл по другому пути — другое видео (осознанное решение: точный локатор).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoKey([u8; 32]);

impl VideoKey {
    /// Строит ключ из точных байт локатора.
    pub fn for_locator(locator: &PlaylistLocator) -> Result<Self, VideoKeyError> {
        locator_fingerprint_with_domain(TRACK_MEMORY_LOCATOR_DOMAIN, locator)
            .map(Self)
            .map_err(|UnsupportedNativePathEncoding| VideoKeyError::UnsupportedNativePathEncoding)
    }

    /// Строит ключ видео, которое является фрагментом физического файла.
    ///
    /// Без фрагмента (`None`) результат равен [`Self::for_locator`]: обычный файл и «окно на
    /// весь файл без границ» не должны иметь двух разных ключей. Фрагмент добавляется в хэш
    /// поверх ключа локатора, поэтому разные окна одного файла различаются.
    pub fn for_locator_segment(
        locator: &PlaylistLocator,
        segment: Option<VideoSegment>,
    ) -> Result<Self, VideoKeyError> {
        let base_key = Self::for_locator(locator)?;
        let Some(segment) = segment else {
            return Ok(base_key);
        };
        let mut hasher = Sha256::new();
        hasher.update((TRACK_MEMORY_SEGMENT_DOMAIN.len() as u64).to_le_bytes());
        hasher.update(TRACK_MEMORY_SEGMENT_DOMAIN);
        hasher.update(base_key.0);
        hasher.update(segment.start_nanos.to_le_bytes());
        match segment.end_nanos {
            // Маркер присутствия отделяет «конец = 0» от «конца нет».
            Some(end_nanos) => {
                hasher.update([0x01]);
                hasher.update(end_nanos.to_le_bytes());
            }
            None => hasher.update([0x00]),
        }
        Ok(Self(hasher.finalize().into()))
    }

    /// Hex для файла (64 символа нижнего регистра).
    pub(super) fn to_hex(self) -> String {
        encode_lower_hex(&self.0)
    }

    /// Разбор hex из файла; `None` — формат нарушен.
    pub(super) fn from_hex(encoded: &str) -> Option<Self> {
        decode_lower_hex_32(encoded).map(Self)
    }

    /// Ключ из готовых байт — только для тестов других модулей.
    #[cfg(test)]
    pub(super) const fn from_bytes_for_tests(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for VideoKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VideoKey(<sha256>)")
    }
}
