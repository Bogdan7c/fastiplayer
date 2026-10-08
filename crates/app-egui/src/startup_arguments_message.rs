//! Тексты для пользователя о нескольких media-аргументах командной строки (сессия 13).
//!
//! Классификатор (`startup_media::initial_arguments`) сообщает типизированный факт: кто
//! победил по правилам drag & drop и сколько аргументов какого вида пропущено. Здесь —
//! только формулировки. Показ — информационная плашка (8 с) через `AppState::notify_info`.
//!
//! Правила текста (общие правила плана): по-русски, без путей к папкам, без имён
//! Rust-типов; пропущенное называется числом и видом («2 плейлиста и 1 ссылка»).

use crate::playlist_skip_message::{RussianPluralForm, russian_plural_form};

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
            SeveralArgumentsWinnerKind::LocalFiles => "Открыты только видео- и аудиофайлы",
            SeveralArgumentsWinnerKind::FirstPlaylist => "Открыт только первый плейлист",
            SeveralArgumentsWinnerKind::FirstLink => "Открыта только первая ссылка",
        };
        sentences.push(format!("{opened}, пропущено: {skipped_kinds}."));
    }
    if skipped.files_over_queue_limit > 0 {
        sentences.push(format!(
            "В очередь помещается не больше {queue_capacity} {}, остальные {} не открыты.",
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
    (!parts.is_empty()).then(|| parts.join(" и "))
}

/// «плейлист» / «плейлиста» / «плейлистов».
const fn playlists_noun(count: usize) -> &'static str {
    match russian_plural_form(count) {
        RussianPluralForm::One => "плейлист",
        RussianPluralForm::Few => "плейлиста",
        RussianPluralForm::Many => "плейлистов",
    }
}

/// «ссылка» / «ссылки» / «ссылок».
const fn links_noun(count: usize) -> &'static str {
    match russian_plural_form(count) {
        RussianPluralForm::One => "ссылка",
        RussianPluralForm::Few => "ссылки",
        RussianPluralForm::Many => "ссылок",
    }
}

/// «файла» / «файлов» после «не больше N» (родительный падеж).
const fn files_noun(count: usize) -> &'static str {
    match russian_plural_form(count) {
        RussianPluralForm::One => "файла",
        RussianPluralForm::Few | RussianPluralForm::Many => "файлов",
    }
}

/// Первый файл открыт, а остальные не удалось поставить в очередь за ним.
pub(crate) const FOLLOW_UP_FILES_NOT_QUEUED_MESSAGE: &str =
    "Первый файл открыт, но остальные файлы не удалось добавить в очередь";

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
            Some("Открыты только видео- и аудиофайлы, пропущено: 2 плейлиста и 1 ссылка.")
        );
        assert_eq!(
            notice(0, 5).as_deref(),
            Some("Открыты только видео- и аудиофайлы, пропущено: 5 ссылок.")
        );
        assert_eq!(
            notice(1, 0).as_deref(),
            Some("Открыты только видео- и аудиофайлы, пропущено: 1 плейлист.")
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
            Some("Открыт только первый плейлист, пропущено: 1 плейлист и 2 ссылки.")
        );
        assert_eq!(
            several_arguments_skipped_notice(
                SeveralArgumentsWinnerKind::FirstLink,
                skipped(0, 21),
                CAPACITY,
            )
            .as_deref(),
            Some("Открыта только первая ссылка, пропущено: 21 ссылка.")
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
                "Открыты только видео- и аудиофайлы, пропущено: 1 плейлист. \
                 В очередь помещается не больше 50000 файлов, остальные 7 не открыты."
            )
        );
    }
}
