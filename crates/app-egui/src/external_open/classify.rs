//! Классификация запроса открытия: что именно принесли, по метаданным файловой системы.
//!
//! Здесь нет демукса и чтения содержимого: только `std::fs::metadata` (файл / папка /
//! нет такого) и расширение плейлиста. Тип media определит уже обычный путь открытия, он же
//! покажет понятную ошибку для «файла, который оказался не media».

use std::io;
use std::path::PathBuf;

use super::request::{DropTarget, DroppedWebUrl, ExternalOpenItem, ExternalOpenRequest};
use crate::startup_media::is_recognized_startup_playlist_path;

/// Элемент запроса после классификации.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClassifiedItem {
    /// Обычный файл, не плейлист: кандидат на воспроизведение или добавление.
    MediaFile(PathBuf),
    /// Папка.
    Directory(PathBuf),
    /// Файл плейлиста (`.m3u`, `.m3u8`, `.xspf`, `.cue`).
    PlaylistFile(PathBuf),
    /// Пути уже нет на диске (удалили между броском и разбором).
    Missing(PathBuf),
    /// Ссылка http(s).
    WebUrl(DroppedWebUrl),
    /// Неподдерживаемая ссылка или чужой `file://`.
    Unsupported {
        /// Схема в нижнем регистре.
        scheme: String,
    },
}

/// План открытия: классифицированные элементы и правила смеси уже применены.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExternalOpenPlan {
    /// Куда брошено.
    pub(crate) target: DropTarget,
    /// Элементы в порядке источника (плейлисты из смеси уже убраны).
    pub(crate) items: Vec<ClassifiedItem>,
    /// Сколько файлов плейлистов пропущено, потому что их принесли вместе с другим.
    pub(crate) skipped_playlist_files: usize,
}

/// Классифицирует запрос и применяет правило смеси.
///
/// Правило смеси (решение владельца): плейлист вместе с media/папками/ссылками не
/// импортируется — media и папки обрабатываются, плейлисты пропускаются с уведомлением.
/// Если принесли только плейлисты (в том числе рядом с пропавшим файлом или неподдерживаемой
/// ссылкой), они остаются в плане (их обработает отдельный этап).
pub(crate) fn classify_request(request: ExternalOpenRequest) -> ExternalOpenPlan {
    let classified: Vec<ClassifiedItem> = request.items.into_iter().map(classify_item).collect();
    // Смесью считается только настоящее содержимое: media, папка или ссылка. «Файл не найден»
    // и неподдерживаемая ссылка ничего не открывают, поэтому плейлист рядом с ними не
    // пропускается (они лишь добавят своё уведомление).
    let has_non_playlist_item = classified.iter().any(|item| {
        matches!(
            item,
            ClassifiedItem::MediaFile(_) | ClassifiedItem::Directory(_) | ClassifiedItem::WebUrl(_)
        )
    });
    let has_playlist_item = classified
        .iter()
        .any(|item| matches!(item, ClassifiedItem::PlaylistFile(_)));
    if !(has_non_playlist_item && has_playlist_item) {
        return ExternalOpenPlan {
            target: request.target,
            items: classified,
            skipped_playlist_files: 0,
        };
    }
    let total = classified.len();
    let kept: Vec<ClassifiedItem> = classified
        .into_iter()
        .filter(|item| !matches!(item, ClassifiedItem::PlaylistFile(_)))
        .collect();
    ExternalOpenPlan {
        target: request.target,
        skipped_playlist_files: total - kept.len(),
        items: kept,
    }
}

/// Классифицирует один элемент.
fn classify_item(item: ExternalOpenItem) -> ClassifiedItem {
    match item {
        ExternalOpenItem::LocalPath(path) => classify_local_path(path),
        ExternalOpenItem::WebUrl(url) => ClassifiedItem::WebUrl(url),
        ExternalOpenItem::Unsupported { scheme } => ClassifiedItem::Unsupported { scheme },
    }
}

/// Локальный путь → файл / папка / плейлист / отсутствует.
fn classify_local_path(path: PathBuf) -> ClassifiedItem {
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => ClassifiedItem::Directory(path),
        Ok(_) if is_recognized_startup_playlist_path(&path) => ClassifiedItem::PlaylistFile(path),
        Ok(_) => ClassifiedItem::MediaFile(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => ClassifiedItem::Missing(path),
        // Нет прав и прочие сбои: файл существует, пусть обычный путь открытия назовёт
        // точную причину («нет доступа к файлу»), а не мы гадаем здесь.
        Err(error) => {
            tracing::debug!(kind = ?error.kind(), "Метаданные брошенного файла недоступны");
            ClassifiedItem::MediaFile(path)
        }
    }
}

#[cfg(test)]
mod tests;
