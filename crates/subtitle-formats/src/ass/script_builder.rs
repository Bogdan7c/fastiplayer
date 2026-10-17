// Порт поведения libass 0.17.5 (ISC): ass.c (process_line, process_info_line, process_styles_line, process_events_line, parse_script_type, parse_ycbcr_matrix, format_line_compare, custom_format_line_compatibility, detect_legacy_conv_subs, event_format_fallback).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
// Copyright (C) 2009 Grigori Goronzy <greg@geekmind.org>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Построчный сборщик сценария ASS: `[Script Info]`, `[V4(+) Styles]`, `[Events]` Format
//! (порт `process_line`, `process_info_line`, `process_styles_line`, `parse_script_type`,
//! `custom_format_line_compatibility`, `detect_legacy_conv_subs`, `event_format_fallback`).
//!
//! Один и тот же автомат читает и файл целиком, и заголовок `codec_private`: различие только
//! в том, что делать со строками `Dialogue:` ([`LineScope`]).

use subtitle_core::{
    AssEventFormat, AssKerning, AssLayoutRes, AssPlayRes, AssScaledBorderAndShadow, AssScript,
    AssScriptInfo, AssScriptKind, AssStyle, AssWrapStyle, AssYCbCrMatrix,
};

use super::event_fields::{
    BrokenEventLine, EventFormatFlavor, RawEventFields, default_event_format, parse_event_format,
    parse_event_tail,
};
use super::limits::AssParseLimits;
use super::scan::{
    FieldCursor, TrailingSpaces, parse_bool, parse_int_header, rskip_spaces, skip_spaces,
};
use super::style_line::{
    StyleField, StyleFlavor, default_style_format, parse_style, parse_style_format,
};

/// Стандартные строки `Format:` из libass — с ними сравниваются форматы файла.
pub(super) const ASS_STYLE_FORMAT_TEXT: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, \
OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding";
pub(super) const ASS_EVENT_FORMAT_TEXT: &str =
    "Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";
pub(super) const SSA_STYLE_FORMAT_TEXT: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, \
TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, \
MarginR, MarginV, AlphaLevel, Encoding";
pub(super) const SSA_EVENT_FORMAT_TEXT: &str =
    "Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";

/// Какие заголовки уже встречались (`header_flags` libass).
mod header_flag {
    pub(super) const PLAY_RES_X: u16 = 1 << 0;
    pub(super) const PLAY_RES_Y: u16 = 1 << 1;
    pub(super) const LAYOUT_RES_X: u16 = 1 << 2;
    pub(super) const LAYOUT_RES_Y: u16 = 1 << 3;
    pub(super) const TIMER: u16 = 1 << 4;
    pub(super) const WRAP_STYLE: u16 = 1 << 5;
    pub(super) const SCALED_BORDER: u16 = 1 << 6;
    pub(super) const KERNING: u16 = 1 << 7;
    pub(super) const COLOUR_MATRIX: u16 = 1 << 8;
    pub(super) const LANGUAGE: u16 = 1 << 9;
    pub(super) const SCRIPT_TYPE: u16 = 1 << 10;
    /// Сценарий сгенерирован FFmpeg/Lavc (по строке-подписи).
    pub(super) const GENERATED_BY_FFMPEG: u16 = 1 << 11;
}

/// Секция, в которой находится разбор.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// До первого заголовка секции строки игнорируются.
    None,
    Info,
    Styles,
    Events,
    /// `[Fonts]` (встроенные шрифты) и прочее — игнорируется.
    Ignored,
}

/// Версия сценария по ходу разбора (в libass есть ещё «неизвестно» до первого признака).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptType {
    Unknown,
    Ssa,
    Ass,
}

/// Что делать со строками `Dialogue:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineScope {
    /// Только заголовок (`codec_private`): события в нём libass тоже разбирает, но у нас
    /// события приходят пакетами, поэтому строки `Dialogue:` здесь игнорируются.
    HeaderOnly,
    /// Файл целиком: `Dialogue:` превращается в [`LineOutcome::Dialogue`].
    FullFile,
}

/// Результат обработки одной строки.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum LineOutcome {
    /// Строка учтена в заголовке (или проигнорирована).
    Handled,
    Dialogue(RawEventFields),
    /// Строка `Dialogue:` не соответствует раскладке; пропущена.
    BrokenDialogue,
}

pub(super) struct ScriptBuilder<'limits> {
    limits: &'limits AssParseLimits,
    scope: LineScope,
    section: Section,
    script_type: ScriptType,
    header_flags: u16,
    play_res_x: i32,
    play_res_y: i32,
    layout_res_x: i32,
    layout_res_y: i32,
    scaled_border_and_shadow: bool,
    wrap_style: i32,
    kerning: bool,
    ycbcr_matrix: AssYCbCrMatrix,
    language: Option<String>,
    style_format: Option<Vec<StyleField>>,
    event_format: Option<AssEventFormat>,
    styles: Vec<AssStyle>,
    skipped_styles: usize,
}

impl<'limits> ScriptBuilder<'limits> {
    pub(super) fn new(limits: &'limits AssParseLimits, scope: LineScope) -> Self {
        Self {
            limits,
            scope,
            section: Section::None,
            script_type: ScriptType::Unknown,
            header_flags: 0,
            play_res_x: 0,
            play_res_y: 0,
            layout_res_x: 0,
            layout_res_y: 0,
            scaled_border_and_shadow: false,
            wrap_style: 0,
            kerning: false,
            ycbcr_matrix: AssYCbCrMatrix::Default,
            language: None,
            style_format: None,
            event_format: None,
            styles: Vec::new(),
            skipped_styles: 0,
        }
    }

    /// Обрабатывает одну строку (без перевода строки и BOM). `process_line`.
    pub(super) fn feed_line(&mut self, line: &str) -> LineOutcome {
        let line = skip_spaces(line);
        if let Some(section) = section_header(line) {
            self.enter_section(section);
            return LineOutcome::Handled;
        }
        match self.section {
            Section::Info => self.process_info_line(line),
            Section::Styles => self.process_styles_line(line),
            Section::Events => return self.process_events_line(line),
            Section::None | Section::Ignored => {}
        }
        LineOutcome::Handled
    }

    /// Сколько стилей пропущено из-за `max_styles`.
    pub(super) fn skipped_styles(&self) -> usize {
        self.skipped_styles
    }

    /// Завершает разбор: разрешает умолчания (`ass_lazy_track_init`, `event_format_fallback`).
    pub(super) fn finish(self) -> AssScript {
        let flavor = self.event_flavor_for_fallback();
        let event_format = self
            .event_format
            .unwrap_or_else(|| default_event_format(flavor));
        let layout_res = (self.layout_res_x > 0 && self.layout_res_y > 0).then_some(AssLayoutRes {
            width: self.layout_res_x,
            height: self.layout_res_y,
        });
        let info = AssScriptInfo {
            play_res: AssPlayRes::resolve(self.play_res_x, self.play_res_y),
            layout_res,
            scaled_border_and_shadow: if self.scaled_border_and_shadow {
                AssScaledBorderAndShadow::Enabled
            } else {
                AssScaledBorderAndShadow::Disabled
            },
            wrap_style: AssWrapStyle::from_libass_value(self.wrap_style),
            kerning: if self.kerning {
                AssKerning::Enabled
            } else {
                AssKerning::Disabled
            },
            ycbcr_matrix: self.ycbcr_matrix,
            language: self.language,
        };
        // Сценарий без признаков версии трактуется как ASS (libass тоже берёт ASS-умолчания).
        let kind = match self.script_type {
            ScriptType::Ssa => AssScriptKind::Ssa,
            ScriptType::Ass | ScriptType::Unknown => AssScriptKind::Ass,
        };
        AssScript::new(kind, info, event_format, self.styles)
    }

    fn enter_section(&mut self, section: SectionHeader) {
        match section {
            SectionHeader::Info => self.section = Section::Info,
            SectionHeader::StylesSsa => {
                self.section = Section::Styles;
                self.script_type = ScriptType::Ssa;
            }
            SectionHeader::StylesAss => {
                self.section = Section::Styles;
                self.script_type = ScriptType::Ass;
            }
            SectionHeader::Events => self.section = Section::Events,
            SectionHeader::Other => self.section = Section::Ignored,
        }
    }

    /// Умолчания для `Format:`-less событий: SSA -> раскладка SSA, иначе ASS.
    fn event_flavor_for_fallback(&self) -> EventFormatFlavor {
        if self.script_type == ScriptType::Ssa {
            EventFormatFlavor::Ssa
        } else {
            EventFormatFlavor::Ass
        }
    }

    fn style_flavor(&self) -> StyleFlavor {
        if self.script_type == ScriptType::Ssa {
            StyleFlavor::Ssa
        } else {
            StyleFlavor::Ass
        }
    }

    fn has_flag(&self, flag: u16) -> bool {
        self.header_flags & flag != 0
    }

    fn set_flag(&mut self, flag: u16) {
        self.header_flags |= flag;
    }

    /// `process_info_line`. Сравнение префиксов чувствительно к регистру (как в libass).
    /// Повторные заголовки молча перезаписывают предыдущие (libass только предупреждает).
    fn process_info_line(&mut self, line: &str) {
        if let Some(value) = line.strip_prefix("PlayResX:") {
            self.set_flag(header_flag::PLAY_RES_X);
            self.play_res_x = parse_int_header(value);
        } else if let Some(value) = line.strip_prefix("PlayResY:") {
            self.set_flag(header_flag::PLAY_RES_Y);
            self.play_res_y = parse_int_header(value);
        } else if let Some(value) = line.strip_prefix("LayoutResX:") {
            self.set_flag(header_flag::LAYOUT_RES_X);
            self.layout_res_x = parse_int_header(value);
        } else if let Some(value) = line.strip_prefix("LayoutResY:") {
            self.set_flag(header_flag::LAYOUT_RES_Y);
            self.layout_res_y = parse_int_header(value);
        } else if line.starts_with("Timer:") {
            // Скорость таймера libass хранит, но не использует; нам нужен только признак
            // «заголовок был» для `detect_legacy_conv_subs`.
            self.set_flag(header_flag::TIMER);
        } else if let Some(value) = line.strip_prefix("WrapStyle:") {
            self.set_flag(header_flag::WRAP_STYLE);
            self.wrap_style = parse_int_header(value);
        } else if let Some(value) = line.strip_prefix("ScaledBorderAndShadow:") {
            self.set_flag(header_flag::SCALED_BORDER);
            self.scaled_border_and_shadow = parse_bool(value);
        } else if let Some(value) = line.strip_prefix("Kerning:") {
            self.set_flag(header_flag::KERNING);
            self.kerning = parse_bool(value);
        } else if let Some(value) = line.strip_prefix("YCbCr Matrix:") {
            self.set_flag(header_flag::COLOUR_MATRIX);
            self.ycbcr_matrix = parse_ycbcr_matrix(value);
        } else if let Some(value) = line.strip_prefix("Language:") {
            self.set_flag(header_flag::LANGUAGE);
            let language: String = value.trim_start().chars().take(2).collect();
            self.language = Some(language);
        } else if let Some(value) = line.strip_prefix("ScriptType:") {
            self.set_flag(header_flag::SCRIPT_TYPE);
            self.apply_script_type(value);
        } else if line
            .strip_prefix("; Script generated by ")
            .is_some_and(|generator| generator.starts_with("FFmpeg/Lavc"))
        {
            self.set_flag(header_flag::GENERATED_BY_FFMPEG);
        }
    }

    /// `parse_script_type`: версия читается с КОНЦА строки (`4.00`, `4.00+`), как у VSFilter.
    fn apply_script_type(&mut self, value: &str) {
        let trimmed = rskip_spaces(value);
        if trimmed.len() < 4 {
            return;
        }
        let (version_text, detected) = match trimmed.strip_suffix('+') {
            Some(without_plus) => (without_plus, ScriptType::Ass),
            None => (trimmed, ScriptType::Ssa),
        };
        if version_text.ends_with("4.00") {
            self.script_type = detected;
        }
    }

    /// `process_styles_line`.
    fn process_styles_line(&mut self, line: &str) {
        if let Some(format_text) = line.strip_prefix("Format:") {
            let format_text = skip_spaces(format_text);
            self.style_format = Some(parse_style_format(format_text));
            let standard = if self.script_type == ScriptType::Ass {
                ASS_STYLE_FORMAT_TEXT
            } else {
                SSA_STYLE_FORMAT_TEXT
            };
            self.apply_custom_format_compatibility(format_text, standard);
        } else if let Some(values) = line.strip_prefix("Style:") {
            if self.styles.len() >= self.limits.max_styles {
                self.skipped_styles = self.skipped_styles.saturating_add(1);
                return;
            }
            let flavor = self.style_flavor();
            let format = self
                .style_format
                .get_or_insert_with(|| default_style_format(flavor));
            let style = parse_style(format, flavor, skip_spaces(values));
            self.styles.push(style);
        }
    }

    /// `process_events_line`.
    fn process_events_line(&mut self, line: &str) -> LineOutcome {
        if let Some(format_text) = line.strip_prefix("Format:") {
            let format_text = skip_spaces(format_text);
            self.event_format = Some(parse_event_format(format_text));
            let standard = if self.script_type == ScriptType::Ass {
                ASS_EVENT_FORMAT_TEXT
            } else {
                SSA_EVENT_FORMAT_TEXT
            };
            self.apply_custom_format_compatibility(format_text, standard);
            if self.is_legacy_ffmpeg_script() {
                self.scaled_border_and_shadow = true;
            }
            return LineOutcome::Handled;
        }
        let Some(tail) = line.strip_prefix("Dialogue:") else {
            // `Comment:` и всё непонятное игнорируется.
            return LineOutcome::Handled;
        };
        if self.scope == LineScope::HeaderOnly {
            return LineOutcome::Handled;
        }
        let flavor = self.event_flavor_for_fallback();
        let format = self
            .event_format
            .get_or_insert_with(|| default_event_format(flavor));
        match parse_event_tail(format.fields(), skip_spaces(tail)) {
            Ok(fields) => LineOutcome::Dialogue(fields),
            Err(BrokenEventLine) => LineOutcome::BrokenDialogue,
        }
    }

    /// `custom_format_line_compatibility`: у файлов с нестандартной строкой `Format:` и без
    /// явного `ScaledBorderAndShadow` libass исторически включает масштабирование контура.
    fn apply_custom_format_compatibility(&mut self, file_format: &str, standard_format: &str) {
        if !self.has_flag(header_flag::SCALED_BORDER)
            && !format_lines_equivalent(file_format, standard_format)
        {
            self.scaled_border_and_shadow = true;
        }
    }

    /// `detect_legacy_conv_subs`: сценарий старого FFmpeg (после подписи, до явного
    /// `ScaledBorderAndShadow`): ровно набор заголовков `ScriptType` + `PlayResX/Y`
    /// + подпись, и единственный стиль `Default*`.
    fn is_legacy_ffmpeg_script(&self) -> bool {
        let expected = header_flag::SCRIPT_TYPE
            | header_flag::PLAY_RES_X
            | header_flag::PLAY_RES_Y
            | header_flag::GENERATED_BY_FFMPEG;
        if self.header_flags != expected {
            return false;
        }
        matches!(self.styles.as_slice(), [only] if only.name.starts_with("Default"))
    }
}

/// Заголовок секции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionHeader {
    Info,
    StylesSsa,
    StylesAss,
    Events,
    Other,
}

/// Распознаёт строку-заголовок секции (префикс, без учёта регистра, как `ass_strncasecmp`).
/// Остальные `[...]` секции (кроме перечисленных) не меняют состояние в libass, но
/// `[Fonts]` переключает его на игнорирование — для нас это одно и то же.
fn section_header(line: &str) -> Option<SectionHeader> {
    let starts = |prefix: &str| {
        line.as_bytes()
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    };
    if starts("[Script Info]") {
        Some(SectionHeader::Info)
    } else if starts("[V4 Styles]") {
        Some(SectionHeader::StylesSsa)
    } else if starts("[V4+ Styles]") {
        Some(SectionHeader::StylesAss)
    } else if starts("[Events]") {
        Some(SectionHeader::Events)
    } else if starts("[Fonts]") {
        Some(SectionHeader::Other)
    } else {
        None
    }
}

/// Нормализует имя поля для сравнения форматов: `Actor` (точно) считается `Name`.
fn normalized_format_token(token: &str) -> &str {
    if token == "Actor" { "Name" } else { token }
}

/// `format_line_compare`: равны ли две строки `Format:` (поля без учёта регистра и
/// пробелов по краям, `Actor` = `Name`, длина списков одинакова).
fn format_lines_equivalent(first: &str, second: &str) -> bool {
    let mut first_cursor = FieldCursor::new(first);
    let mut second_cursor = FieldCursor::new(second);
    loop {
        match (
            first_cursor.next_token(TrailingSpaces::Trim),
            second_cursor.next_token(TrailingSpaces::Trim),
        ) {
            (None, None) => return true,
            (Some(left), Some(right)) => {
                let left = normalized_format_token(left);
                let right = normalized_format_token(right);
                if !left.eq_ignore_ascii_case(right) {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// `parse_ycbcr_matrix`: пустая строка — `Default`; значение (не длиннее 15 байт, как
/// буфер libass) сравнивается без учёта регистра.
fn parse_ycbcr_matrix(value: &str) -> AssYCbCrMatrix {
    let trimmed = rskip_spaces(skip_spaces(value));
    if trimmed.is_empty() {
        return AssYCbCrMatrix::Default;
    }
    let limited = trimmed.get(..15).unwrap_or(trimmed);
    const KNOWN: &[(&str, AssYCbCrMatrix)] = &[
        ("none", AssYCbCrMatrix::None),
        ("tv.601", AssYCbCrMatrix::Bt601Tv),
        ("pc.601", AssYCbCrMatrix::Bt601Pc),
        ("tv.709", AssYCbCrMatrix::Bt709Tv),
        ("pc.709", AssYCbCrMatrix::Bt709Pc),
        ("tv.240m", AssYCbCrMatrix::Smpte240mTv),
        ("pc.240m", AssYCbCrMatrix::Smpte240mPc),
        ("tv.fcc", AssYCbCrMatrix::FccTv),
        ("pc.fcc", AssYCbCrMatrix::FccPc),
    ];
    KNOWN
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(limited))
        .map_or(AssYCbCrMatrix::Unknown, |(_, matrix)| *matrix)
}
