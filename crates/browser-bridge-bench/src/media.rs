//! Тестовые ролики стенда: генерация ffmpeg-ом в кэш и каталог `catalog.json`.
//!
//! Владелец состояния — этот модуль: структура каталога (контракт версии 1), правила проверки
//! («все файлы на месте») и безопасное разрешение URL в путь на диске. Остальные модули стенда
//! (сайт, сценарии) работают только через [`MediaCatalog`] и не знают ни про ffmpeg, ни про
//! раскладку каталогов кэша.
//!
//! Кэш: `<cache_root>/<sha256 скрипта generate.sh>/` — любое изменение скрипта даёт новый ключ,
//! поэтому устаревшие ролики не подхватываются. Генерация идёт во временный каталог рядом и
//! атомарно переименовывается, так что параллельные прогоны не увидят половину файлов.

mod catalog;
mod error;
mod generator;
pub(crate) mod safe_path;

pub use catalog::{
    Asset, CATALOG_FILE_NAME, CATALOG_VERSION, CatalogFile, Segment, Track, TrackKind,
};
pub use error::MediaError;
pub use generator::{ensure_media, generate_media_into, generate_script_path};

use std::path::{Path, PathBuf};

/// Каталог медиа, лежащий на диске: корень с файлами и разобранный `catalog.json`.
///
/// Инвариант: объект получен через [`MediaCatalog::load`] (или [`ensure_media`]), то есть
/// версия каталога поддерживается и каждый файл, упомянутый в нём, существует.
#[derive(Debug, Clone)]
pub struct MediaCatalog {
    /// Каталог, относительно которого отсчитываются все `url` из каталога.
    pub root: PathBuf,
    /// Разобранное содержимое `catalog.json`.
    pub file: CatalogFile,
}

impl MediaCatalog {
    /// Читает `<root>/catalog.json` и проверяет его (версия, наличие всех файлов).
    pub fn load(root: &Path) -> Result<Self, MediaError> {
        let catalog_path = root.join(CATALOG_FILE_NAME);
        let text = std::fs::read_to_string(&catalog_path).map_err(|source| MediaError::Io {
            action: "read the catalog",
            path: catalog_path.clone(),
            source,
        })?;
        let file: CatalogFile =
            serde_json::from_str(&text).map_err(|source| MediaError::CatalogParse {
                path: catalog_path,
                source,
            })?;
        file.validate_against(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            file,
        })
    }

    /// Ролик по идентификатору (`h264-aac`, `clearkey-h264`, …).
    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.file.assets.get(id)
    }

    /// Путь к `catalog.json` на диске (его отдаёт сервер как `/media/catalog.json`).
    pub fn catalog_path(&self) -> PathBuf {
        self.root.join(CATALOG_FILE_NAME)
    }

    /// Превращает `url` относительно `/media/` в путь к существующему файлу.
    ///
    /// Возвращает `None`, если путь пытается выйти из корня (`..`, абсолютный путь, обратная
    /// косая черта) или файла нет. Различать эти случаи вызывающему коду незачем: наружу всё
    /// равно уходит один и тот же 404.
    pub fn resolve(&self, url: &str) -> Option<PathBuf> {
        safe_path::resolve_existing_file(&self.root, url)
    }
}

#[cfg(test)]
mod tests;
