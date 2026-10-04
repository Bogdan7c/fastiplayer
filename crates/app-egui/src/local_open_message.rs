//! Тексты для пользователя об открытии локального файла.
//!
//! Единственный владелец формулировок: кнопка Open, замена очереди, CLI-старт и строка
//! плейлиста берут текст только отсюда, поэтому одна и та же причина везде выглядит
//! одинаково. Причину определяет `media_open::LocalOpenFailureReason` (классификация
//! живёт рядом с ошибкой), а здесь — только превращение причины в русский текст.
//!
//! Политика приватности (решение владельца 1а): показываем имя файла, но не путь к
//! папке. Имя строит `safe_local_open_label`.

use std::path::Path;

use crate::media_open::LocalOpenFailureReason;
use crate::playlist_runtime::safe_local_open_label;

/// Сообщение об ошибке открытия: «Не удалось открыть «clip.mkv»: файл не найден».
pub(crate) fn local_open_failure_message(path: &Path, reason: LocalOpenFailureReason) -> String {
    let file_label = safe_local_open_label(path);
    format!(
        "Не удалось открыть «{file_label}»: {}",
        failure_reason_phrase(reason)
    )
}

/// Сообщение, пока файл готовится к воспроизведению: «Открываем «clip.mkv»…».
pub(crate) fn local_open_preparing_message(path: &Path) -> String {
    let file_label = safe_local_open_label(path);
    format!("Открываем «{file_label}»…")
}

/// Короткая причина для бейджа строки плейлиста: «Файл не найден».
///
/// Имя файла не добавляется — строка очереди уже показывает его рядом.
pub(crate) fn local_open_failure_row_summary(reason: LocalOpenFailureReason) -> String {
    capitalize_first_letter(failure_reason_phrase(reason))
}

/// Человеческая формулировка причины, со строчной буквы (идёт после двоеточия).
const fn failure_reason_phrase(reason: LocalOpenFailureReason) -> &'static str {
    match reason {
        LocalOpenFailureReason::FileNotFound => "файл не найден",
        LocalOpenFailureReason::AccessDenied => "нет доступа к файлу",
        LocalOpenFailureReason::IsDirectory => "это папка, а не файл",
        LocalOpenFailureReason::EmptyFile => "файл пустой",
        LocalOpenFailureReason::UnrecognizedFormat => {
            "формат файла не распознан или не поддерживается"
        }
        LocalOpenFailureReason::DamagedOrTruncated => "файл повреждён или обрезан",
        LocalOpenFailureReason::ReadFailed => "не удалось прочитать файл",
        LocalOpenFailureReason::ReadTimedOut => "файл читается слишком долго",
        LocalOpenFailureReason::NoAudioOrVideo => "в файле нет ни видео, ни звука",
        LocalOpenFailureReason::ChangedDuringOpen => {
            "файл изменился во время открытия, попробуйте ещё раз"
        }
        LocalOpenFailureReason::InternalError => "внутренняя ошибка плеера",
    }
}

/// Делает первую букву заглавной с учётом Unicode (кириллица занимает 2 байта в UTF-8).
fn capitalize_first_letter(phrase: &str) -> String {
    let mut characters = phrase.chars();
    match characters.next() {
        Some(first_letter) => first_letter.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests;
