// Порт поведения libass 0.17.5: ass_font.c (ass_face_get_weight, ass_face_get_style_flags,
// ass_face_is_postscript и условия синтетики в ass_font_get_glyph).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
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

//! Что лицо само говорит о своём начертании: решает, нужна ли синтетика, и хранит линии
//! подчёркивания/зачёркивания.
//!
//! libass решает «дорисовать ли жирность/наклон» не по тому, какое лицо подобрал выбор шрифта,
//! а по самому лицу и запросу символа ([`FaceStyleInfo::variant_for`]):
//! - курсив дорисовывается, если у лица нет флага Italic (OS/2 `fsSelection`) и запрошен курсив;
//! - жирность — если у лица нет флага Bold и запрошенный вес больше веса лица больше чем на 150.
//!
//! Тот же расчёт делается для запасных лиц (символ из другого шрифта), поэтому вариант глифа —
//! это пара «лицо + запрос», а не свойство запроса.

use skrifa::FontRef;
use skrifa::raw::TableProvider;
use skrifa::raw::tables::head::MacStyle;
use skrifa::raw::tables::os2::SelectionFlags;

use crate::fonts::{FontStyle, SyntheticBold, SyntheticItalic};
use crate::span_look::{SpanLook, TextDecorations};

/// На сколько запрошенный вес должен превышать вес лица, чтобы включилось утолщение.
const SYNTHETIC_BOLD_WEIGHT_MARGIN: i32 = 150;

/// Линия подчёркивания или зачёркивания в единицах шрифта (после проверок libass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecorationLine {
    /// Положение линии относительно базовой линии в единицах шрифта (как в таблице шрифта:
    /// у подчёркивания не положительное, у зачёркивания не отрицательное).
    pub(crate) position_units: i32,
    /// Толщина линии в единицах шрифта, положительная.
    pub(crate) thickness_units: i32,
}

/// Какой вариант контура нужен глифу: синтетика и линии (часть ключа кэша контуров).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GlyphVariant {
    /// Утолщать ли контур.
    pub(crate) synthetic_bold: SyntheticBold,
    /// Наклонять ли контур.
    pub(crate) synthetic_italic: SyntheticItalic,
    /// Подчёркивание и зачёркивание, которые надо дорисовать.
    pub(crate) decorations: TextDecorations,
}

impl GlyphVariant {
    /// Контур без изменений (шрифт содержит нужное начертание, линий нет).
    pub(crate) const PLAIN: Self = Self {
        synthetic_bold: SyntheticBold::NotNeeded,
        synthetic_italic: SyntheticItalic::NotNeeded,
        decorations: TextDecorations {
            underline: false,
            strike_out: false,
        },
    };
}

/// Начертание лица, прочитанное из таблиц шрифта (один раз при подготовке лица).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FaceStyleInfo {
    /// Вес лица в шкале CSS (libass: `ass_face_get_weight`).
    weight: i32,
    /// Лицо помечено как жирное (OS/2 `fsSelection` бит 5).
    is_bold: bool,
    /// Лицо помечено как курсив (OS/2 `fsSelection` бит 0).
    is_italic: bool,
    /// Контуры лица PostScript (таблица `CFF `): наклон у них другой (tan 10° вместо GDI).
    is_postscript: bool,
    /// Линия подчёркивания из таблицы `post`, если она подходит под условия libass.
    underline: Option<DecorationLine>,
    /// Линия зачёркивания из таблицы OS/2, если она подходит под условия libass.
    strike_out: Option<DecorationLine>,
}

impl FaceStyleInfo {
    /// Читает начертание лица. Отсутствующие таблицы дают значения «по умолчанию» libass:
    /// без OS/2 флаги берутся из `head.macStyle`, вес — 400 (700 у жирного), линий нет.
    pub(crate) fn from_font(font: &FontRef<'_>) -> Self {
        let mac_style = font.head().map(|head| head.mac_style()).ok();
        let mac_bold = mac_style.is_some_and(|style| style.contains(MacStyle::BOLD));
        let mac_italic = mac_style.is_some_and(|style| style.contains(MacStyle::ITALIC));
        let os2 = font.os2().ok();
        let (is_bold, is_italic) = match &os2 {
            // Если есть OS/2, libass считает флаги сам: FreeType подмешивает лишнее (GDI это
            // игнорирует).
            Some(table) => {
                let selection = table.fs_selection();
                (
                    selection.contains(SelectionFlags::BOLD),
                    selection.contains(SelectionFlags::ITALIC),
                )
            }
            None => (mac_bold, mac_italic),
        };
        let weight = face_weight(os2.as_ref().map(|table| table.us_weight_class()), mac_bold);
        let underline = font.post().ok().and_then(|post| {
            let position = i32::from(post.underline_position().to_i16());
            let thickness = i32::from(post.underline_thickness().to_i16());
            (position <= 0 && thickness > 0).then_some(DecorationLine {
                position_units: position,
                thickness_units: thickness,
            })
        });
        let strike_out = os2.as_ref().and_then(|table| {
            let position = i32::from(table.y_strikeout_position());
            let size = i32::from(table.y_strikeout_size());
            (position >= 0 && size > 0).then_some(DecorationLine {
                position_units: position,
                thickness_units: size,
            })
        });
        Self {
            weight,
            is_bold,
            is_italic,
            is_postscript: font.table_data(skrifa::Tag::new(b"CFF ")).is_some(),
            underline,
            strike_out,
        }
    }

    /// Вариант контура для символа с видом `look` (libass: условия в `ass_font_get_glyph` и
    /// флаги `DECO_*` в ключе кэша контуров).
    pub(crate) fn variant_for(&self, look: &SpanLook) -> GlyphVariant {
        let wants_italic = look.slant == FontStyle::Italic;
        let synthetic_italic = if wants_italic && !self.is_italic {
            SyntheticItalic::Needed
        } else {
            SyntheticItalic::NotNeeded
        };
        let wants_heavier =
            i32::from(look.weight_value) > self.weight + SYNTHETIC_BOLD_WEIGHT_MARGIN;
        let synthetic_bold = if !self.is_bold && wants_heavier {
            SyntheticBold::Needed
        } else {
            SyntheticBold::NotNeeded
        };
        GlyphVariant {
            synthetic_bold,
            synthetic_italic,
            decorations: look.decorations,
        }
    }

    /// Контуры лица PostScript (CFF).
    pub(crate) fn is_postscript(&self) -> bool {
        self.is_postscript
    }

    /// Линия подчёркивания, если лицо её задаёт.
    pub(crate) fn underline(&self) -> Option<DecorationLine> {
        self.underline
    }

    /// Линия зачёркивания, если лицо её задаёт.
    pub(crate) fn strike_out(&self) -> Option<DecorationLine> {
        self.strike_out
    }
}

/// Вес лица (libass: `ass_face_get_weight`): значения OS/2 `usWeightClass` 1..9 — это
/// «сотни» старой шкалы Microsoft, остальные берутся как есть; нулевой класс заменяется
/// 400 (700 у жирного по флагам лица).
fn face_weight(os2_weight_class: Option<u16>, mac_bold: bool) -> i32 {
    match os2_weight_class.unwrap_or(0) {
        0 => 400 + if mac_bold { 300 } else { 0 },
        1 => 100,
        2 => 200,
        3 => 300,
        4 => 350,
        5 => 400,
        6 => 600,
        7 => 700,
        8 => 800,
        9 => 900,
        other => i32::from(other),
    }
}

#[cfg(test)]
mod tests;
