//! Маршрутизация плана в шаги исполнения (чистая функция, без состояния и I/O).
//!
//! Таблица решений (решение владельца: место броска решает):
//!
//! | Место   | media-файлов | Шаг                                             |
//! |---------|--------------|-------------------------------------------------|
//! | видео   | 1            | открыть как кнопкой Open                         |
//! | видео   | N >= 2       | заменить очередь этими N файлами, играть первый  |
//! | плейлист| N >= 1       | добавить в конец очереди                         |
//!
//! Если среди элементов есть папка, все файлы и папки броска собираются в ОДИН упорядоченный
//! набор ([`OpenStep::CollectDroppedItems`]): порядок броска сохраняется, папка раскрывается
//! на своём месте фоновым обходом. Файл плейлиста даёт [`OpenStep::ImportPlaylistFile`]
//! (если принесено несколько плейлистов, импортируется первый, остальные — с уведомлением).
//!
//! Ссылки (решение владельца): панель плейлиста — как «Добавить URL», видео — новая очередь
//! ([`OpenStep::OpenWebUrl`] с местом броска). За один бросок берётся первая ссылка, остальные
//! — с уведомлением; если вместе с ссылкой принесли файлы или папки, обрабатываются они, а
//! ссылки пропускаются с уведомлением.

use std::path::PathBuf;

use super::classify::{ClassifiedItem, ExternalOpenPlan};
use super::notice::DropNotice;
use super::request::{DropTarget, DroppedWebUrl};
use crate::playlist_runtime::DroppedCollectionEntry;

/// Один шаг исполнения плана.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenStep {
    /// Один файл на видео: тот же путь, что у кнопки Open.
    OpenSingleMediaFile(PathBuf),
    /// Несколько файлов на видео: новая очередь из них (с подтверждением, если очередь не пуста).
    ReplaceQueueWithMediaFiles(Vec<PathBuf>),
    /// Файлы на панель плейлиста: добавить в конец.
    AppendMediaFiles(Vec<PathBuf>),
    /// Набор из файлов и папок: фоновый обход, затем замена очереди (видео) или добавление (панель).
    CollectDroppedItems {
        /// Куда брошено: определяет, что сделать с найденными файлами.
        target: DropTarget,
        /// Файлы и папки в порядке броска.
        entries: Vec<DroppedCollectionEntry>,
    },
    /// Импорт файла плейлиста; намерение (добавить / заменить) выбирает диспетчер по настройке.
    ImportPlaylistFile {
        /// Путь к файлу плейлиста.
        path: PathBuf,
        /// Куда брошено.
        target: DropTarget,
    },
    /// Показать пользователю сообщение.
    Notify(DropNotice),
    /// Ссылка: на панель плейлиста — добавить как «Добавить URL», на видео — новая очередь.
    OpenWebUrl {
        /// Брошенная ссылка (в `Debug` скрыта).
        url: DroppedWebUrl,
        /// Куда брошено.
        target: DropTarget,
    },
}

/// Превращает план в упорядоченные шаги: сначала действие над media, затем сообщения.
pub(crate) fn route_plan(plan: ExternalOpenPlan) -> Vec<OpenStep> {
    let mut media_files = Vec::new();
    let mut collection_entries = Vec::new();
    let mut has_folder = false;
    let mut playlist_files = Vec::new();
    let mut missing_count = 0usize;
    let mut unsupported_count = 0usize;
    let mut web_urls = Vec::new();
    for item in plan.items {
        match item {
            ClassifiedItem::MediaFile(path) => {
                collection_entries.push(DroppedCollectionEntry::File(path.clone()));
                media_files.push(path);
            }
            ClassifiedItem::Missing(_) => missing_count += 1,
            ClassifiedItem::Unsupported { .. } => unsupported_count += 1,
            ClassifiedItem::Directory(path) => {
                has_folder = true;
                collection_entries.push(DroppedCollectionEntry::Folder(path));
            }
            ClassifiedItem::PlaylistFile(path) => playlist_files.push(path),
            ClassifiedItem::WebUrl(url) => web_urls.push(url),
        }
    }

    let mut steps = Vec::new();
    if has_folder {
        steps.push(OpenStep::CollectDroppedItems {
            target: plan.target,
            entries: collection_entries,
        });
    } else if let Some(step) = media_step(plan.target, media_files) {
        steps.push(step);
    }
    let extra_playlist_count = playlist_files.len().saturating_sub(1);
    if let Some(first_playlist) = playlist_files.into_iter().next() {
        steps.push(OpenStep::ImportPlaylistFile {
            path: first_playlist,
            target: plan.target,
        });
    }
    // Ссылки считаются «в смеси», только если для файлов/папок/плейлиста уже есть действие:
    // одни лишь уведомления («файл не найден») ссылке не мешают.
    let web_url_company = if steps.is_empty() {
        WebUrlCompany::Alone
    } else {
        WebUrlCompany::WithLocalItems
    };
    push_web_url_steps(&mut steps, plan.target, web_urls, web_url_company);
    if extra_playlist_count > 0 {
        steps.push(OpenStep::Notify(DropNotice::PlaylistsOneAtATime));
    }
    if plan.skipped_playlist_files > 0 {
        steps.push(OpenStep::Notify(DropNotice::PlaylistsSeparately));
    }
    if missing_count > 0 {
        steps.push(OpenStep::Notify(DropNotice::FilesNotFound {
            count: missing_count,
        }));
    }
    if unsupported_count > 0 {
        steps.push(OpenStep::Notify(DropNotice::UnsupportedLink));
    }
    if steps.is_empty() {
        steps.push(OpenStep::Notify(DropNotice::NothingToOpen));
    }
    steps
}

/// Были ли ссылки принесены вместе с файлами, папками или плейлистом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebUrlCompany {
    /// Кроме ссылок, действий над локальным содержимым нет.
    Alone,
    /// Есть действие над файлами/папками/плейлистом: ссылки пропускаются.
    WithLocalItems,
}

/// Шаги для ссылок: первая ссылка исполняется, лишние и смесь с файлами дают уведомления.
fn push_web_url_steps(
    steps: &mut Vec<OpenStep>,
    target: DropTarget,
    web_urls: Vec<DroppedWebUrl>,
    company: WebUrlCompany,
) {
    if web_urls.is_empty() {
        return;
    }
    if company == WebUrlCompany::WithLocalItems {
        steps.push(OpenStep::Notify(DropNotice::LinksSeparately));
        return;
    }
    let extra_url_count = web_urls.len() - 1;
    if let Some(first_url) = web_urls.into_iter().next() {
        steps.push(OpenStep::OpenWebUrl {
            url: first_url,
            target,
        });
    }
    if extra_url_count > 0 {
        steps.push(OpenStep::Notify(DropNotice::LinksOneAtATime));
    }
}

/// Шаг над media-файлами по таблице решений; `None`, если файлов нет.
fn media_step(target: DropTarget, mut media_files: Vec<PathBuf>) -> Option<OpenStep> {
    match (target, media_files.len()) {
        (_, 0) => None,
        (DropTarget::Playlist, _) => Some(OpenStep::AppendMediaFiles(media_files)),
        (DropTarget::Video, 1) => media_files.pop().map(OpenStep::OpenSingleMediaFile),
        (DropTarget::Video, _) => Some(OpenStep::ReplaceQueueWithMediaFiles(media_files)),
    }
}

#[cfg(test)]
mod tests;
