//! Типизированная форма `catalog.json` (контракт версии 1) и её проверка.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::MediaError;
use super::safe_path::resolve_existing_file;

/// Имя файла каталога внутри корня медиа.
pub const CATALOG_FILE_NAME: &str = "catalog.json";
/// Единственная поддерживаемая версия формата.
pub const CATALOG_VERSION: u32 = 1;

/// Содержимое `catalog.json`: версия и ролики по идентификаторам.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogFile {
    pub version: u32,
    /// `BTreeMap`, чтобы порядок при выводе и в сообщениях об ошибках был стабильным.
    pub assets: BTreeMap<String, Asset>,
}

/// Один ролик. Вид задаётся полем `kind` прямо в JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Asset {
    /// Сегментированный fMP4/WebM для MSE: init-сегмент + сегменты на каждую дорожку.
    Segmented { duration_s: f64, tracks: Vec<Track> },
    /// Цельный файл для `<video src>`.
    File {
        url: String,
        mime: String,
        duration_s: f64,
    },
    /// HLS: главный плейлист (медиаплейлисты и сегменты лежат рядом).
    Hls { url: String, duration_s: f64 },
    /// Текст, например субтитры WebVTT.
    Text { url: String, mime: String },
    /// Зашифрованный (ClearKey) сегментированный ролик: ключ открыт, это тестовый ключ.
    Clearkey {
        duration_s: f64,
        key_id_hex: String,
        key_hex: String,
        tracks: Vec<Track>,
    },
}

/// Вид дорожки: `muxed` — видео и звук в одном SourceBuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Muxed,
}

/// Дорожка сегментированного ролика (один SourceBuffer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    /// MIME с codecs — ровно то, что страница передаёт в `addSourceBuffer`.
    pub mime: String,
    /// Размеры картинки; у чисто звуковых дорожек их нет.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// URL init-сегмента (относительно `/media/`).
    pub init: String,
    pub segments: Vec<Segment>,
}

/// Медиа-сегмент: URL и положение на шкале времени ролика.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub url: String,
    pub start_s: f64,
    pub duration_s: f64,
}

impl Asset {
    /// Дорожки сегментированного (или ClearKey) ролика; у остальных видов их нет.
    pub fn tracks(&self) -> &[Track] {
        match self {
            Asset::Segmented { tracks, .. } | Asset::Clearkey { tracks, .. } => tracks,
            Asset::File { .. } | Asset::Hls { .. } | Asset::Text { .. } => &[],
        }
    }

    /// Все URL, которые ролик обещает отдать: по ним проверяется наличие файлов.
    fn referenced_urls(&self) -> Vec<&str> {
        match self {
            Asset::File { url, .. } | Asset::Hls { url, .. } | Asset::Text { url, .. } => {
                vec![url.as_str()]
            }
            Asset::Segmented { tracks, .. } | Asset::Clearkey { tracks, .. } => tracks
                .iter()
                .flat_map(|track| {
                    std::iter::once(track.init.as_str())
                        .chain(track.segments.iter().map(|segment| segment.url.as_str()))
                })
                .collect(),
        }
    }
}

impl CatalogFile {
    /// Проверяет версию и что каждый упомянутый файл существует, обычный и непустой.
    pub(super) fn validate_against(&self, root: &Path) -> Result<(), MediaError> {
        if self.version != CATALOG_VERSION {
            return Err(MediaError::UnsupportedVersion {
                found: self.version,
                expected: CATALOG_VERSION,
            });
        }
        for (asset_id, asset) in &self.assets {
            for url in asset.referenced_urls() {
                let Some(path) = resolve_existing_file(root, url) else {
                    // Различаем «небезопасный url» и «файла нет», чтобы сообщение было точным.
                    return Err(if super::safe_path::join_inside(root, url).is_none() {
                        MediaError::UnsafeUrl {
                            asset: asset_id.clone(),
                            url: url.to_owned(),
                        }
                    } else {
                        MediaError::MissingFile {
                            asset: asset_id.clone(),
                            url: url.to_owned(),
                        }
                    });
                };
                let is_empty = std::fs::metadata(&path)
                    .map(|meta| meta.len() == 0)
                    .unwrap_or(true);
                if is_empty {
                    return Err(MediaError::MissingFile {
                        asset: asset_id.clone(),
                        url: url.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}
