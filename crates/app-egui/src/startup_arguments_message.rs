//! Тексты для пользователя о нескольких media-аргументах командной строки (сессия 13).
//!
//! Классификатор (`startup_media::initial_arguments`) сообщает типизированный факт: кто
//! победил по правилам drag & drop и сколько аргументов какого вида пропущено. Здесь —
//! только формулировки. Показ — информационная плашка (8 с) через `AppState::notify_info`.
//!
//! Правила текста (общие правила плана): по-английски, без путей к папкам, без имён
//! Rust-типов; пропущенное называется числом и видом («2 плейлиста и 1 ссылка»).

/// Кого открыл startup из нескольких аргументов (решение владельца 6 сессии 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeveralArgumentsWinnerKind {
    /// Открыты локальные видео-/аудиофайлы, плейлисты и ссылки пропущены.
    LocalFiles,
    /// Локальных файлов нет: открыт первый плейлист.
    FirstPlaylist,
    /// Ни файлов, ни плейлистов: открыта первая ссылка.
    FirstLink,
}

/// Сколько аргументов каждого вида не открыто.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SkippedStartupArguments {
    /// Пропущенные файлы плейлистов (`.m3u`, `.xspf`, …).
    pub(crate) playlists: usize,
    /// Пропущенные ссылки (включая ссылки, которые плеер открыть не умеет).
    pub(crate) links: usize,
    /// Локальные файлы сверх вместимости очереди.
    pub(crate) files_over_queue_limit: usize,
}

/// Текст уведомления о пропущенных аргументах; `None`, если ничего не пропущено.
pub(crate) fn several_arguments_skipped_notice(
    winner: SeveralArgumentsWinnerKind,
    skipped: SkippedStartupArguments,
    queue_capacity: usize,
) -> Option<String> {
    let mut sentences = Vec::new();
    if let Some(skipped_kinds) = skipped_kinds_text(skipped) {
        let opened = match winner {
            SeveralArgumentsWinnerKind::LocalFiles => "Only video and audio files were opened",
            SeveralArgumentsWinnerKind::FirstPlaylist => "Only the first Playlist was opened",
            SeveralArgumentsWinnerKind::FirstLink => "Only the first link was opened",
        };
        sentences.push(format!("{opened}; skipped: {skipped_kinds}."));
    }
    if skipped.files_over_queue_limit > 0 {
        sentences.push(format!(
            "The queue holds at most {queue_capacity} {}; the remaining {} were not opened.",
            files_noun(queue_capacity),
            skipped.files_over_queue_limit,
        ));
    }
    (!sentences.is_empty()).then(|| sentences.join(" "))
}

/// «2 плейлиста», «1 ссылка», «2 плейлиста и 1 ссылка»; `None`, если пропусков нет.
fn skipped_kinds_text(skipped: SkippedStartupArguments) -> Option<String> {
    let mut parts = Vec::new();
    if skipped.playlists > 0 {
        parts.push(format!(
            "{} {}",
            skipped.playlists,
            playlists_noun(skipped.playlists)
        ));
    }
    if skipped.links > 0 {
        parts.push(format!("{} {}", skipped.links, links_noun(skipped.links)));
    }
    (!parts.is_empty()).then(|| parts.join(" and "))
}

/// Английское существительное: 21 и 101 тоже требуют множественного числа.
const fn playlists_noun(count: usize) -> &'static str {
    if count == 1 { "Playlist" } else { "Playlists" }
}

/// Единственное число используется только для ровно одной ссылки.
const fn links_noun(count: usize) -> &'static str {
    if count == 1 { "link" } else { "links" }
}

/// В английском форма существительного не зависит от падежа.
const fn files_noun(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

/// Первый файл открыт, а остальные не удалось поставить в очередь за ним.
pub(crate) const FOLLOW_UP_FILES_NOT_QUEUED_MESSAGE: &str =
    "The first file was opened, but the remaining files could not be added to the queue";

#[cfg(test)]
mod tests {
    use super::*;

    const CAPACITY: usize = 50_000;

    fn skipped(playlists: usize, links: usize) -> SkippedStartupArguments {
        SkippedStartupArguments {
            playlists,
            links,
            files_over_queue_limit: 0,
        }
    }

    #[test]
    fn local_files_winner_names_skipped_kinds_with_counts() {
        let notice = |playlists, links| {
            several_arguments_skipped_notice(
                SeveralArgumentsWinnerKind::LocalFiles,
                skipped(playlists, links),
                CAPACITY,
            )
        };
        assert_eq!(
            notice(2, 1).as_deref(),
            Some("Only video and audio files were opened; skipped: 2 Playlists and 1 link.")
        );
        assert_eq!(
            notice(0, 5).as_deref(),
            Some("Only video and audio files were opened; skipped: 5 links.")
        );
        assert_eq!(
            notice(1, 0).as_deref(),
            Some("Only video and audio files were opened; skipped: 1 Playlist.")
        );
    }

    #[test]
    fn playlist_and_link_winners_name_what_was_opened() {
        assert_eq!(
            several_arguments_skipped_notice(
                SeveralArgumentsWinnerKind::FirstPlaylist,
                skipped(1, 2),
                CAPACITY,
            )
            .as_deref(),
            Some("Only the first Playlist was opened; skipped: 1 Playlist and 2 links.")
        );
        assert_eq!(
            several_arguments_skipped_notice(
                SeveralArgumentsWinnerKind::FirstLink,
                skipped(0, 21),
                CAPACITY,
            )
            .as_deref(),
            Some("Only the first link was opened; skipped: 21 links.")
        );
    }

    #[test]
    fn nothing_skipped_gives_no_notice() {
        assert_eq!(
            several_arguments_skipped_notice(
                SeveralArgumentsWinnerKind::LocalFiles,
                SkippedStartupArguments::default(),
                CAPACITY,
            ),
            None
        );
    }

    #[test]
    fn queue_overflow_is_reported_with_capacity_and_count() {
        let notice = several_arguments_skipped_notice(
            SeveralArgumentsWinnerKind::LocalFiles,
            SkippedStartupArguments {
                playlists: 1,
                links: 0,
                files_over_queue_limit: 7,
            },
            CAPACITY,
        );
        assert_eq!(
            notice.as_deref(),
            Some(
                "Only video and audio files were opened; skipped: 1 Playlist. The queue holds at most 50000 files; the remaining 7 were not opened."
            )
        );
    }
}
