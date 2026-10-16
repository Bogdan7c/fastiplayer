//! Тексты субтитров для пользователя: подписи дорожек в меню, уведомления о смене дорожки
//! и сдвиге синхронизации, итоги добавления файла.
//!
//! Модуль чистый: принимает типизированные факты владельца субтитров
//! (`state::SubtitleTrackLabel`, `SubtitleSyncOffset`, `AttachOutcome`) и возвращает
//! строки интерфейса (английские). Ни состояния, ни ввода-вывода, ни egui здесь нет, поэтому
//! формулировки проверяются обычными unit-тестами, а меню и toast-ы не расходятся в словах.

use subtitle_core::{SubtitleFormat, SubtitleSyncOffset};

use crate::state::{
    AttachOutcome, SubtitleFileOrigin, SubtitleTrackLabel, SubtitleUnavailableReason,
};

/// Уведомление, когда у видео нет ни одной дорожки субтитров (клавиши C/V/Z/X).
pub(crate) const NO_SUBTITLES_MESSAGE: &str = "No subtitles for this video";

/// Уведомление «Off» для клавиши V, когда цикл дошёл до выключения.
pub(crate) const SUBTITLES_OFF_MESSAGE: &str = "Subtitles: off";

/// Номер дорожки в меню, считая с единицы: для дорожек без языка («Track 3»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrackNumber(usize);

impl TrackNumber {
    /// Номер по позиции в списке меню (индекс с нуля).
    pub(crate) const fn from_menu_index(menu_index: usize) -> Self {
        Self(menu_index + 1)
    }
}

/// Короткое имя формата для подписи.
fn format_name(format: Option<SubtitleFormat>) -> &'static str {
    let Some(format) = format else {
        return "other";
    };
    match format {
        SubtitleFormat::Srt => "SRT",
        SubtitleFormat::WebVtt => "VTT",
        SubtitleFormat::Ass => "ASS",
        SubtitleFormat::Ssa => "SSA",
        SubtitleFormat::MovText => "MOV_TEXT",
        SubtitleFormat::Srv3 => "SRV3",
        SubtitleFormat::Ttml => "TTML",
        SubtitleFormat::MicroDvd => "MicroDVD",
        SubtitleFormat::Sami => "SAMI",
        SubtitleFormat::Pgs => "PGS",
        // Формат добавили в `subtitle-core`, а подпись для него ещё не придумали.
        _ => "other",
    }
}

/// Откуда файл: слово в скобках подписи.
const fn origin_name(origin: SubtitleFileOrigin) -> &'static str {
    match origin {
        SubtitleFileOrigin::NextToVideo => "file",
        SubtitleFileOrigin::ChosenByUser => "added",
        SubtitleFileOrigin::Embedded => "embedded",
    }
}

/// Подпись дорожки для меню и уведомления: `Russian — forced (SRT, file)`.
///
/// Название берётся по убыванию точности: английское имя языка, иначе сырой тег из имени
/// файла, иначе имя файла (две дорожки без языка так всё равно различимы), и только у
/// дорожки без файла — «Track N». Название из файла (`title`) добавляется в кавычках, если
/// оно не повторяет уже показанное.
pub(crate) fn track_text(label: &SubtitleTrackLabel, track_number: TrackNumber) -> String {
    let base_name = match (&label.language, label.file_name.as_deref()) {
        (Some(language), _) => language
            .english_display_name()
            .map_or_else(|| language.as_str().to_owned(), str::to_owned),
        (None, Some(file_name)) => file_name.to_owned(),
        (None, None) => format!("Track {}", track_number.0),
    };
    let mut text = base_name.clone();
    if let Some(title) = label.title.as_deref().map(str::trim)
        && !title.is_empty()
        && !title.eq_ignore_ascii_case(&base_name)
    {
        text.push_str(&format!(" \"{title}\""));
    }
    let flags: Vec<&str> = [
        label.is_forced.then_some("forced"),
        label.is_hearing_impaired.then_some("SDH"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !flags.is_empty() {
        text.push_str(&format!(" — {}", flags.join(", ")));
    }
    text.push_str(&format!(
        " ({}, {})",
        format_name(label.format),
        origin_name(label.origin)
    ));
    text
}

/// Уведомление после выбора дорожки клавишей V.
pub(crate) fn track_selected_toast(track_text: &str) -> String {
    format!("Subtitles: {track_text}")
}

/// Сдвиг с явным знаком и одним десятичным знаком: `+0.3 s`, `-0.2 s`, `0.0 s`.
pub(crate) fn sync_offset_text(offset: SubtitleSyncOffset) -> String {
    // Сдвиг кратен 100 мс, но округляем честно на случай других источников.
    let tenths_of_second = (offset.as_micros() as f64 / 100_000.0).round() as i64;
    match tenths_of_second {
        0 => "0.0 s".to_owned(),
        // Дефис-минус вместо настоящего минуса: он есть в любом шрифте.
        tenths if tenths < 0 => format!("-{}.{} s", tenths.abs() / 10, tenths.abs() % 10),
        tenths => format!("+{}.{} s", tenths / 10, tenths % 10),
    }
}

/// Уведомление после клавиш Z/X и сброса.
pub(crate) fn sync_offset_toast(offset: SubtitleSyncOffset) -> String {
    format!("Subtitle delay: {}", sync_offset_text(offset))
}

/// Строка «Delay» в меню.
pub(crate) fn sync_offset_menu_text(offset: SubtitleSyncOffset) -> String {
    format!("Delay: {}", sync_offset_text(offset))
}

/// Человеческая причина, почему дорожку нельзя выбрать. Технические детали в UI не нужны:
/// владелец записывает их в лог.
pub(crate) const fn unavailable_reason_text(reason: &SubtitleUnavailableReason) -> &'static str {
    match reason {
        SubtitleUnavailableReason::FormatNotSupportedYet => {
            "This subtitle format is not supported yet."
        }
        SubtitleUnavailableReason::LoadFailed(_) => "This subtitle file could not be read.",
    }
}

/// Уведомление об итоге «Load file…»; `None` — сообщать нечего (файл добавлен и виден).
pub(crate) const fn attach_outcome_message(outcome: AttachOutcome) -> Option<&'static str> {
    match outcome {
        AttachOutcome::Attached(_) => None,
        AttachOutcome::AddedButUnavailable(_) => Some("This subtitle format is not supported yet."),
        AttachOutcome::NotASubtitleFile => Some("This file is not a subtitle file."),
        AttachOutcome::NoMediaToAttachTo => Some("Open a video first, then add subtitles."),
    }
}

/// Одно уведомление на пачку файлов (бросок нескольких субтитров): первое, что есть что сказать.
/// Если все файлы добавлены и видны, сообщать нечего.
pub(crate) fn attach_batch_message(outcomes: &[AttachOutcome]) -> Option<&'static str> {
    outcomes.iter().copied().find_map(attach_outcome_message)
}

#[cfg(test)]
mod tests {
    use subtitle_core::{SubtitleLanguage, SubtitleTrackId};

    use super::*;

    fn label(language: Option<&str>) -> SubtitleTrackLabel {
        SubtitleTrackLabel {
            language: language.map(|tag| SubtitleLanguage::parse(tag).expect("valid test tag")),
            format: Some(SubtitleFormat::Srt),
            origin: SubtitleFileOrigin::NextToVideo,
            is_forced: false,
            is_hearing_impaired: false,
            title: None,
            file_name: None,
        }
    }

    fn text_of(label: &SubtitleTrackLabel) -> String {
        track_text(label, TrackNumber::from_menu_index(2))
    }

    #[test]
    fn language_name_comes_from_the_table_for_two_and_three_letter_codes() {
        assert_eq!(text_of(&label(Some("ru"))), "Russian (SRT, file)");
        assert_eq!(text_of(&label(Some("rus"))), "Russian (SRT, file)");
    }

    #[test]
    fn unknown_language_shows_the_raw_tag_and_missing_language_shows_track_number() {
        assert_eq!(text_of(&label(Some("tlh"))), "tlh (SRT, file)");
        assert_eq!(text_of(&label(None)), "Track 3 (SRT, file)");
    }

    #[test]
    fn file_without_language_is_named_by_its_file_name_not_by_number() {
        // Две брошенные дорожки без языка в имени должны различаться в меню.
        let mut dropped = label(None);
        dropped.origin = SubtitleFileOrigin::ChosenByUser;
        dropped.file_name = Some("commentary.srt".to_owned());
        assert_eq!(text_of(&dropped), "commentary.srt (SRT, added)");
    }

    #[test]
    fn flags_title_format_and_origin_are_appended_in_a_stable_order() {
        let mut forced = label(Some("uk"));
        forced.is_forced = true;
        assert_eq!(text_of(&forced), "Ukrainian — forced (SRT, file)");

        let mut sdh = label(Some("en"));
        sdh.is_hearing_impaired = true;
        sdh.format = Some(SubtitleFormat::WebVtt);
        sdh.origin = SubtitleFileOrigin::ChosenByUser;
        assert_eq!(text_of(&sdh), "English — SDH (VTT, added)");

        let mut both = label(Some("en"));
        both.is_forced = true;
        both.is_hearing_impaired = true;
        assert_eq!(text_of(&both), "English — forced, SDH (SRT, file)");

        let mut titled = label(Some("ru"));
        titled.title = Some("Director's commentary".to_owned());
        assert_eq!(
            text_of(&titled),
            "Russian \"Director's commentary\" (SRT, file)"
        );
    }

    #[test]
    fn title_that_repeats_the_shown_name_or_is_blank_is_not_added() {
        let mut repeated = label(Some("ru"));
        repeated.title = Some("russian".to_owned());
        assert_eq!(text_of(&repeated), "Russian (SRT, file)");
        let mut blank = label(Some("ru"));
        blank.title = Some("  ".to_owned());
        assert_eq!(text_of(&blank), "Russian (SRT, file)");
    }

    #[test]
    fn ass_and_ssa_have_their_own_format_names() {
        let mut ass = label(Some("en"));
        ass.format = Some(SubtitleFormat::Ass);
        assert_eq!(text_of(&ass), "English (ASS, file)");
        ass.format = Some(SubtitleFormat::Ssa);
        assert_eq!(text_of(&ass), "English (SSA, file)");
    }

    #[test]
    fn embedded_tracks_say_embedded_and_unnamed_formats_say_other() {
        let mut embedded = label(Some("ja"));
        embedded.origin = SubtitleFileOrigin::Embedded;
        embedded.format = Some(SubtitleFormat::Pgs);
        assert_eq!(text_of(&embedded), "Japanese (PGS, embedded)");
        embedded.format = None;
        assert_eq!(text_of(&embedded), "Japanese (other, embedded)");
    }

    #[test]
    fn sync_offset_has_explicit_sign_and_one_decimal() {
        let text = |millis| sync_offset_text(SubtitleSyncOffset::from_millis(millis));
        assert_eq!(text(300), "+0.3 s");
        assert_eq!(text(-200), "-0.2 s");
        assert_eq!(text(0), "0.0 s");
        assert_eq!(text(12_500), "+12.5 s");
        assert_eq!(text(-1_000), "-1.0 s");
        assert_eq!(
            sync_offset_toast(SubtitleSyncOffset::from_millis(300)),
            "Subtitle delay: +0.3 s"
        );
        assert_eq!(
            sync_offset_menu_text(SubtitleSyncOffset::from_millis(-100)),
            "Delay: -0.1 s"
        );
    }

    #[test]
    fn unavailable_reasons_hide_technical_details() {
        assert_eq!(
            unavailable_reason_text(&SubtitleUnavailableReason::FormatNotSupportedYet),
            "This subtitle format is not supported yet."
        );
        assert_eq!(
            unavailable_reason_text(&SubtitleUnavailableReason::LoadFailed(
                "io error: /secret/path".to_owned()
            )),
            "This subtitle file could not be read."
        );
    }

    #[test]
    fn batch_message_is_the_first_outcome_that_has_something_to_say() {
        let id = SubtitleTrackId::new(1);
        assert_eq!(
            attach_batch_message(&[AttachOutcome::Attached(id), AttachOutcome::Attached(id)]),
            None
        );
        assert_eq!(
            attach_batch_message(&[
                AttachOutcome::Attached(id),
                AttachOutcome::AddedButUnavailable(id),
                AttachOutcome::NoMediaToAttachTo,
            ]),
            Some("This subtitle format is not supported yet.")
        );
        assert_eq!(attach_batch_message(&[]), None);
    }

    #[test]
    fn attach_outcomes_map_to_the_documented_messages() {
        let id = SubtitleTrackId::new(1);
        assert_eq!(attach_outcome_message(AttachOutcome::Attached(id)), None);
        assert_eq!(
            attach_outcome_message(AttachOutcome::AddedButUnavailable(id)),
            Some("This subtitle format is not supported yet.")
        );
        assert_eq!(
            attach_outcome_message(AttachOutcome::NotASubtitleFile),
            Some("This file is not a subtitle file.")
        );
        assert_eq!(
            attach_outcome_message(AttachOutcome::NoMediaToAttachTo),
            Some("Open a video first, then add subtitles.")
        );
    }
}
