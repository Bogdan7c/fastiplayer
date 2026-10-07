//! Применение результата фонового обхода брошенных папок к очереди и UI.
//!
//! Обход делает `PlaylistRuntime` (см. `playlist_runtime::dropped_collection`); здесь —
//! решение «что показать и какой существующий путь вызвать»:
//!
//! - ничего не найдено → уведомление, очередь не тронута;
//! - бросок на панель → Manual Add в конец очереди (без подтверждения);
//! - бросок на видео → замена очереди через ТОТ ЖЕ admission, что и у файлов: пустая очередь
//!   заменяется сразу, непустая требует подтверждения. Подтверждение показывается уже ПОСЛЕ
//!   обхода (осознанное исключение S14A по решению владельца: в вопросе видно число файлов;
//!   до Confirm выполнено только перечисление каталога). Уведомление об усечении лимитом
//!   показывается только когда файлы реально добавлены: для замены — в момент commit.
//!
//! Защита от устаревшего результата: admission проверяет очередь в момент применения, а не
//! броска; если за время обхода началось другое открытие или импорт, результат отбрасывается
//! с сообщением «Файл ещё открывается»; shutdown отменяет обход токеном.

use tracing::{debug, info, warn};

use super::super::AppState;
use crate::external_open::DropNotice;
use crate::playlist_runtime::{
    CollectedDroppedFiles, DroppedCollectionDestination, DroppedCollectionTruncation,
    DroppedCollectionWalkCompletion, InAppQueueReplacementIntent, PlaylistRuntime,
};

impl AppState {
    /// Применяет завершённый обход к очереди; вызывается UI-потоком по wake владельца.
    pub(crate) fn apply_dropped_collection_walk_completion(
        &mut self,
        completion: DroppedCollectionWalkCompletion,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        match completion {
            DroppedCollectionWalkCompletion::Cancelled => {
                debug!("Обход брошенных папок отменён: результат отброшен");
            }
            DroppedCollectionWalkCompletion::WorkerFailed => {
                warn!("Поток обхода брошенных папок завершился аварийно");
                self.notify_info(DropNotice::FolderUnreadable.text());
            }
            DroppedCollectionWalkCompletion::Collected(collected) => {
                self.apply_collected_dropped_files(collected, playlist_runtime, renderer);
            }
        }
    }

    /// Решает судьбу собранных файлов: пустой результат, устаревание, добавление или замена.
    fn apply_collected_dropped_files(
        &mut self,
        collected: CollectedDroppedFiles,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        // В логе только числа: пути и имена папок приватны.
        info!(
            file_count = collected.files.len(),
            unreadable = collected.unreadable_count,
            truncation = ?collected.truncation,
            "Обход брошенных папок завершён"
        );
        if collected.files.is_empty() {
            self.notify_info(empty_result_notice(collected.unreadable_count).text());
            return;
        }
        // Устаревание: пока шёл обход, пользователь мог начать другое открытие.
        if self.has_pending_local_file_open() || playlist_runtime.has_playlist_import_in_flight() {
            self.notify_open_still_in_progress();
            return;
        }
        if let Some(notice) = truncation_notice_before_commit(
            collected.destination,
            collected.truncation,
            collected.files.len(),
        ) {
            self.notify_info(notice.text());
        }
        match collected.destination {
            DroppedCollectionDestination::AppendToQueue => {
                self.append_dropped_media_files(collected.files, playlist_runtime);
            }
            DroppedCollectionDestination::ReplaceQueue => {
                // Усечение едет внутри intent-а и показывается в момент фактической замены
                // очереди (`replace_queue_with_admitted_local_files`), а не здесь.
                let intent = match collected.source.as_ref() {
                    Some(source) => {
                        InAppQueueReplacementIntent::local_files_from_dropped_collection(
                            collected.files,
                            source,
                            collected.truncation,
                        )
                    }
                    None => InAppQueueReplacementIntent::local_files(collected.files),
                };
                self.request_queue_replacement_with_intent(intent, playlist_runtime, renderer);
            }
        }
    }
}

/// Сообщение для пустого результата: «нечего открывать» или «не удалось прочитать».
fn empty_result_notice(unreadable_count: usize) -> DropNotice {
    if unreadable_count > 0 {
        DropNotice::FolderUnreadable
    } else {
        DropNotice::FolderHasNoMedia
    }
}

/// Уведомление об усечении, которое можно показать ДО фактического добавления файлов.
///
/// Append добавляет файлы сразу, без подтверждения — сообщаем сейчас. Замена очереди может
/// ждать Confirm (и быть отменена), поэтому для неё до commit уведомления нет: усечение
/// показывается в момент замены.
fn truncation_notice_before_commit(
    destination: DroppedCollectionDestination,
    truncation: Option<DroppedCollectionTruncation>,
    taken_files: usize,
) -> Option<DropNotice> {
    match destination {
        DroppedCollectionDestination::AppendToQueue => truncation_notice(truncation, taken_files),
        DroppedCollectionDestination::ReplaceQueue => None,
    }
}

/// Сообщение об усечении результата лимитом; `None`, если обход дошёл до конца.
pub(super) fn truncation_notice(
    truncation: Option<DroppedCollectionTruncation>,
    taken_files: usize,
) -> Option<DropNotice> {
    match truncation? {
        DroppedCollectionTruncation::FileLimit => {
            Some(DropNotice::FolderFileLimitReached { taken: taken_files })
        }
        DroppedCollectionTruncation::DepthLimit => Some(DropNotice::FolderDepthLimitReached),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_result_says_no_media_or_unreadable_depending_on_errors() {
        assert_eq!(empty_result_notice(0).text(), "В папке нет медиафайлов");
        assert_eq!(empty_result_notice(2).text(), "Не удалось прочитать папку");
    }

    #[test]
    fn replace_destination_defers_truncation_notice_until_commit() {
        let limit = Some(DroppedCollectionTruncation::FileLimit);

        assert_eq!(
            truncation_notice_before_commit(
                DroppedCollectionDestination::ReplaceQueue,
                limit,
                2000
            ),
            None,
            "замена может быть отменена в Confirm: уведомлять рано"
        );
        assert_eq!(
            truncation_notice_before_commit(
                DroppedCollectionDestination::AppendToQueue,
                limit,
                2000
            )
            .map(DropNotice::text),
            Some("Добавлены первые 2000 файлов из папки".to_owned())
        );
    }

    #[test]
    fn truncation_notices_name_the_taken_count_or_depth() {
        assert_eq!(truncation_notice(None, 5), None);
        assert_eq!(
            truncation_notice(Some(DroppedCollectionTruncation::FileLimit), 2000)
                .map(DropNotice::text),
            Some("Добавлены первые 2000 файлов из папки".to_owned())
        );
        assert_eq!(
            truncation_notice(Some(DroppedCollectionTruncation::DepthLimit), 5)
                .map(DropNotice::text),
            Some("Часть вложенных папок пропущена: они расположены слишком глубоко".to_owned())
        );
    }
}
