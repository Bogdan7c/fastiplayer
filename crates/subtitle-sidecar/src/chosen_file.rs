//! Описание файла субтитров, который выбрал пользователь (бросок на окно, «Загрузить файл…»).
//!
//! В отличие от поиска рядом с видео, здесь нет основы видео, с которой можно сверять имя,
//! поэтому подсказки (язык, forced, SDH) берутся только из «хвоста» имени после первой части:
//! `Movie.ru.forced.srt` -> `ru` + forced, `sintel_ru.srt` -> `ru`. Имя из одной части
//! (`Movie.srt`) подсказок не даёт: иначе само слово `Movie` могло бы сойти за код языка.
//!
//! Функции чисто именные: файловую систему не трогают и содержимое не читают.

use std::path::Path;

use crate::language_hint::{NameHints, parse_name_hints};
use crate::outcome::{SidecarLocation, SidecarSubtitleCandidate, SidecarSubtitleFormat};

/// Сколько последних точечных частей основы имени разбираются как подсказки.
const MAX_HINT_PARTS: usize = 3;

/// Трёхбуквенные теги релиза, которые выглядят как код языка (`Show.1080p.WEB.srt`). Код языка
/// из такого слова — ложное срабатывание, поэтому часть с ним подсказкой не считается.
const RELEASE_TAGS_LOOKING_LIKE_LANGUAGE_CODES: [&str; 6] =
    ["web", "dvd", "hdr", "rip", "cam", "mux"];

/// Расширение (без точки, без учёта регистра) относится к известному формату субтитров?
///
/// Для классификатора броска: файл с таким расширением не является медиа.
pub fn is_subtitle_file_extension(extension: &str) -> bool {
    SidecarSubtitleFormat::from_extension(extension).is_some()
}

/// Описывает выбранный пользователем файл или `None`, если это не файл субтитров по
/// расширению. Результат — обычный кандидат с `SidecarLocation::ChosenByUser`.
pub fn describe_subtitle_file(path: &Path) -> Option<SidecarSubtitleCandidate> {
    let format = SidecarSubtitleFormat::from_path(path)?;
    let stem = path.file_stem()?.to_string_lossy();
    let hints = hints_from_trailing_parts(&stem);
    Some(SidecarSubtitleCandidate {
        path: path.to_path_buf(),
        format,
        location: SidecarLocation::ChosenByUser,
        language: hints.language,
        disposition: hints.disposition,
        is_hearing_impaired: hints.is_hearing_impaired,
        // Название по имени файла не угадываем: имя файла и так показывается целиком.
        title: None,
    })
}

/// Разделители частей имени — те же, что у поиска рядом с видео (кроме дефиса: он живёт
/// внутри тегов вроде `pt-BR`, и такой токен целиком разбирает `parse_name_hints`).
const NAME_PART_SEPARATORS: [char; 7] = ['.', '_', ' ', '[', ']', '(', ')'];

/// Собирает подсказки из последних частей основы имени.
///
/// Части разбираются С КОНЦА и только пока они являются подсказками (язык, forced, SDH):
/// `Show.S01E02.ru.forced` -> `forced`, `ru`, дальше `S01E02` не подсказка — остановка.
/// Так же читаются `sintel_ru` и `Movie [Forced]`. Первая часть основы — «название» и
/// подсказкой не бывает.
///
/// Язык принимается только из таблицы известных языков: без видео, с которым можно сверить
/// имя, случайное слово из трёх букв (`The_Big_Lie`) иначе стало бы «языком».
fn hints_from_trailing_parts(stem: &str) -> NameHints {
    let parts: Vec<&str> = stem
        .split(NAME_PART_SEPARATORS)
        .filter(|part| !part.is_empty())
        .skip(1)
        .collect();
    let mut combined = NameHints::default();
    for part in parts.iter().rev().take(MAX_HINT_PARTS) {
        let part_hints = parse_name_hints(part);
        let is_release_tag = RELEASE_TAGS_LOOKING_LIKE_LANGUAGE_CODES
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case(part));
        let is_known_language = part_hints
            .language
            .as_ref()
            .is_some_and(|language| language.is_common_sidecar_language());
        let is_hint = !is_release_tag
            && part_hints.title.is_none()
            && (is_known_language
                || part_hints.disposition.is_forced
                || part_hints.is_hearing_impaired);
        if !is_hint {
            break;
        }
        // Идём с конца: язык ближе к концу имени главнее более раннего.
        if is_known_language {
            combined.language = combined.language.or(part_hints.language);
        }
        combined.disposition.is_forced |= part_hints.disposition.is_forced;
        combined.is_hearing_impaired |= part_hints.is_hearing_impaired;
    }
    combined
}
