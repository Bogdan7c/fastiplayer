//! Настройки реплики WebVTT после конечного времени: `vertical`, `line`, `position`,
//! `size`, `align`, `region`.
//!
//! Некорректная настройка не ломает реплику: она отбрасывается, а её имя возвращается
//! вызывающему, чтобы тот записал проблему в отчёт. Повторная настройка с тем же именем
//! перекрывает предыдущую (как в браузерах).

use subtitle_core::{
    CueLine, CueLineAlign, CuePlacement, CuePosition, CuePositionAlign, CueTextAlign,
    CueWritingDirection, PercentOfVideo,
};

use super::timestamp::is_webvtt_space;
use crate::report::CueSettingName;

/// Итог разбора строки настроек.
#[derive(Debug, PartialEq)]
pub(super) struct ParsedCueSettings {
    /// Размещение; `region_id` здесь ещё «сырой»: существование региона проверяет вызывающий.
    pub(super) placement: CuePlacement,
    /// Настройки, которые пришлось отбросить (по одной записи на каждую).
    pub(super) rejected: Vec<CueSettingName>,
}

/// Разбирает хвост строки времени. Пустая строка даёт размещение по умолчанию.
pub(super) fn parse_cue_settings(settings: &str) -> ParsedCueSettings {
    let mut placement = CuePlacement::default();
    let mut rejected = Vec::new();
    for token in settings
        .split(is_webvtt_space)
        .filter(|token| !token.is_empty())
    {
        let Some((name, value)) = split_setting(token) else {
            rejected.push(CueSettingName::Unknown);
            continue;
        };
        let applied = match name {
            "vertical" => apply_vertical(&mut placement, value).ok_or(CueSettingName::Vertical),
            "line" => apply_line(&mut placement, value).ok_or(CueSettingName::Line),
            "position" => apply_position(&mut placement, value).ok_or(CueSettingName::Position),
            "size" => apply_size(&mut placement, value).ok_or(CueSettingName::Size),
            "align" => apply_align(&mut placement, value).ok_or(CueSettingName::Align),
            "region" => apply_region(&mut placement, value).ok_or(CueSettingName::Region),
            _ => Err(CueSettingName::Unknown),
        };
        if let Err(setting_name) = applied {
            rejected.push(setting_name);
        }
    }
    ParsedCueSettings {
        placement,
        rejected,
    }
}

/// Процент вида `12`, `12.5` + `%`. Знак, пустая целая часть и экспонента недопустимы;
/// значение вне 0..=100 отвергается типом [`PercentOfVideo`].
pub(super) fn parse_percent(text: &str) -> Option<PercentOfVideo> {
    let digits = text.strip_suffix('%')?;
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let is_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    // Дробная часть, если есть точка, обязана содержать цифры: `5.%` невалиден.
    let has_dot = digits.contains('.');
    if whole.is_empty()
        || !is_digits(whole)
        || !is_digits(fraction)
        || (has_dot && fraction.is_empty())
    {
        return None;
    }
    PercentOfVideo::new(digits.parse().ok()?).ok()
}

/// `имя:значение`; двоеточие не может быть первым или последним символом токена.
fn split_setting(token: &str) -> Option<(&str, &str)> {
    let (name, value) = token.split_once(':')?;
    (!name.is_empty() && !value.is_empty()).then_some((name, value))
}

fn apply_vertical(placement: &mut CuePlacement, value: &str) -> Option<()> {
    placement.writing_direction = match value {
        "rl" => CueWritingDirection::VerticalRightToLeft,
        "lr" => CueWritingDirection::VerticalLeftToRight,
        _ => return None,
    };
    Some(())
}

/// `line:<число>|<процент>[,start|center|end]`. Выравнивание применяется только
/// если вся настройка корректна, иначе настройка не меняет размещение вовсе.
fn apply_line(placement: &mut CuePlacement, value: &str) -> Option<()> {
    let (position_text, align_text) = split_optional_align(value);
    let line = if position_text.ends_with('%') {
        CueLine::Percent(parse_percent(position_text)?)
    } else {
        CueLine::LineNumber(parse_signed_integer(position_text)?)
    };
    let line_align = match align_text {
        None => CueLineAlign::Start,
        Some("start") => CueLineAlign::Start,
        Some("center") => CueLineAlign::Center,
        Some("end") => CueLineAlign::End,
        Some(_) => return None,
    };
    placement.line = line;
    placement.line_align = line_align;
    Some(())
}

/// `position:<процент>[,line-left|center|line-right]`.
fn apply_position(placement: &mut CuePlacement, value: &str) -> Option<()> {
    let (percent_text, align_text) = split_optional_align(value);
    let position = CuePosition::Percent(parse_percent(percent_text)?);
    let position_align = match align_text {
        None => CuePositionAlign::Auto,
        Some("line-left") => CuePositionAlign::LineLeft,
        Some("center") => CuePositionAlign::Center,
        Some("line-right") => CuePositionAlign::LineRight,
        Some(_) => return None,
    };
    placement.position = position;
    placement.position_align = position_align;
    Some(())
}

fn apply_size(placement: &mut CuePlacement, value: &str) -> Option<()> {
    placement.size = parse_percent(value)?;
    Some(())
}

fn apply_align(placement: &mut CuePlacement, value: &str) -> Option<()> {
    placement.text_align = match value {
        "start" => CueTextAlign::Start,
        "center" => CueTextAlign::Center,
        "end" => CueTextAlign::End,
        "left" => CueTextAlign::Left,
        "right" => CueTextAlign::Right,
        _ => return None,
    };
    Some(())
}

/// Идентификатор региона не должен содержать `-->`, иначе строка не была бы настройкой.
fn apply_region(placement: &mut CuePlacement, value: &str) -> Option<()> {
    if value.contains("-->") {
        return None;
    }
    placement.region_id = Some(value.to_owned());
    Some(())
}

/// `значение[,выравнивание]` -> (значение, выравнивание).
fn split_optional_align(value: &str) -> (&str, Option<&str>) {
    match value.split_once(',') {
        Some((main, align)) => (main, Some(align)),
        None => (value, None),
    }
}

/// Целое со знаком (`-2`, `3`); дробные и пустые значения недопустимы.
fn parse_signed_integer(text: &str) -> Option<i32> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn percent(value: f32) -> PercentOfVideo {
        PercentOfVideo::new(value).unwrap()
    }

    #[test]
    fn empty_settings_give_webvtt_defaults() {
        let parsed = parse_cue_settings("");
        assert_eq!(parsed.placement, CuePlacement::default());
        assert!(parsed.rejected.is_empty());
    }

    #[test]
    fn all_settings_are_applied() {
        let parsed = parse_cue_settings(
            "vertical:rl line:10%,end position:30%,line-right size:40% align:left region:r1",
        );
        let expected = CuePlacement {
            line: CueLine::Percent(percent(10.0)),
            line_align: CueLineAlign::End,
            position: CuePosition::Percent(percent(30.0)),
            position_align: CuePositionAlign::LineRight,
            size: percent(40.0),
            text_align: CueTextAlign::Left,
            writing_direction: CueWritingDirection::VerticalRightToLeft,
            region_id: Some("r1".to_owned()),
            anchor: None,
        };
        assert_eq!(parsed.placement, expected);
        assert!(parsed.rejected.is_empty());
    }

    #[test]
    fn line_number_can_be_negative_and_vertical_lr_is_recognized() {
        let parsed = parse_cue_settings("line:-2 vertical:lr");
        assert_eq!(parsed.placement.line, CueLine::LineNumber(-2));
        assert_eq!(
            parsed.placement.writing_direction,
            CueWritingDirection::VerticalLeftToRight
        );
    }

    #[test]
    fn invalid_settings_are_rejected_by_name_and_keep_defaults() {
        let parsed = parse_cue_settings(
            "vertical:up line:abc position:101% size:-5% align:justify region:a-->b bogus:1 novalue: :x lonely",
        );
        assert_eq!(parsed.placement, CuePlacement::default());
        assert_eq!(
            parsed.rejected,
            [
                CueSettingName::Vertical,
                CueSettingName::Line,
                CueSettingName::Position,
                CueSettingName::Size,
                CueSettingName::Align,
                CueSettingName::Region,
                CueSettingName::Unknown,
                CueSettingName::Unknown,
                CueSettingName::Unknown,
                CueSettingName::Unknown,
            ]
        );
    }

    #[test]
    fn bad_alignment_part_rejects_whole_setting_without_partial_change() {
        let parsed = parse_cue_settings("line:5,middle position:50%,left");
        assert_eq!(parsed.placement.line, CueLine::Auto);
        assert_eq!(parsed.placement.position, CuePosition::Auto);
        assert_eq!(
            parsed.rejected,
            [CueSettingName::Line, CueSettingName::Position]
        );
    }

    #[test]
    fn later_duplicate_overrides_earlier_and_good_neighbours_survive() {
        let parsed = parse_cue_settings("align:start size:oops align:end size:50.5%");
        assert_eq!(parsed.placement.text_align, CueTextAlign::End);
        assert_eq!(parsed.placement.size, percent(50.5));
        assert_eq!(parsed.rejected, [CueSettingName::Size]);
    }

    #[test]
    fn percent_grammar_is_strict() {
        assert_eq!(parse_percent("0%"), Some(percent(0.0)));
        assert_eq!(parse_percent("100%"), Some(percent(100.0)));
        assert_eq!(parse_percent("12.5%"), Some(percent(12.5)));
        for bad in [
            "50", "%", "5.%", ".5%", "+5%", "1e1%", "100.1%", "5 %", "-0%",
        ] {
            assert_eq!(parse_percent(bad), None, "{bad:?}");
        }
    }
}
