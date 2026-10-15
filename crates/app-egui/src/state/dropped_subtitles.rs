//! Срок жизни файлов субтитров, ждущих видео из того же броска («видео + .srt»).
//!
//! Файлы запоминает владелец субтитров (`queue_dropped_subtitle_files`), а забирает их
//! `begin_media` в момент, когда видео установлено в плеер. Но открытие может закончиться
//! БЕЗ установки: ошибка подготовки, отмена в окне подтверждения замены очереди, отмена
//! транзакции, отказ очереди, пустой обход папки. У этих исходов нет одной общей точки
//! (подготовка, подтверждение, strong-open, обход папки и playlist-transport сообщают о
//! неудаче каждый по-своему), поэтому корневая причина «прилипания к чужому видео» —
//! срок жизни файлов не привязан к самому открытию.
//!
//! Решение: срок жизни = «пока идёт любая работа по открытию». Как только вся работа
//! закончилась ([`OpenActivity::is_idle`]), а файлы всё ещё ждут, значит видео не
//! установилось, и их надо забыть. Проверка идёт раз за кадр после опроса открытий.

use super::AppState;
use crate::playlist_runtime::PlaylistRuntime;

/// Снимок «идёт ли хоть что-то, что может закончиться установкой нового видео».
///
/// Каждое поле — отдельный владелец работы; поля названы по смыслу, чтобы место вызова
/// читалось без пояснений.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenActivity {
    /// Диалог/подготовка файла из Open или drag & drop, либо его установка.
    pub(super) local_file_open: bool,
    /// Транзакция strong-open занимает общий слот (старт, очередь, Open).
    pub(super) strong_open_slot: bool,
    /// Идут receipt-ы транспорта очереди.
    pub(super) playlist_transport: bool,
    /// На экране окно подтверждения (замена очереди и подобные): ответ ещё впереди.
    pub(super) confirmation_dialog: bool,
    /// Фоновый обход брошенных папок.
    pub(super) folder_walk: bool,
    /// Импорт файла плейлиста.
    pub(super) playlist_import: bool,
    /// Импорт ссылки («Получаем данные по ссылке…»).
    pub(super) url_import: bool,
    /// Сброс media очереди ещё не доставлен.
    pub(super) media_reset: bool,
}

impl OpenActivity {
    /// Верно, когда никакая работа по открытию не идёт и новое видео уже не появится само.
    pub(super) const fn is_idle(self) -> bool {
        !(self.local_file_open
            || self.strong_open_slot
            || self.playlist_transport
            || self.confirmation_dialog
            || self.folder_walk
            || self.playlist_import
            || self.url_import
            || self.media_reset)
    }
}

impl AppState {
    /// Забывает файлы субтитров, если открытие, которого они ждали, закончилось без
    /// установки видео. Вызывается раз за кадр из опроса открытий; без ожидающих файлов
    /// ничего не делает.
    pub(super) fn settle_dropped_subtitles(&mut self, playlist_runtime: &PlaylistRuntime) {
        if self.open_activity(playlist_runtime).is_idle() {
            self.clear_queued_subtitle_files();
        }
    }

    /// Собирает снимок активности открытия из владельцев работы.
    fn open_activity(&self, playlist_runtime: &PlaylistRuntime) -> OpenActivity {
        OpenActivity {
            local_file_open: self.has_pending_local_file_open(),
            strong_open_slot: self.has_pending_prepared_media_strong(),
            playlist_transport: self.has_pending_playlist_transport(),
            confirmation_dialog: playlist_runtime.pending_playlist_confirmation().is_some(),
            folder_walk: playlist_runtime.has_dropped_collection_walk_in_flight(),
            playlist_import: playlist_runtime.has_playlist_import_in_flight(),
            url_import: playlist_runtime.playlist_url_import_progress().is_some(),
            media_reset: playlist_runtime.has_pending_media_reset(),
        }
    }
}

#[cfg(test)]
mod tests;
