//! Встроенные шрифты Noto (OFL-1.1, unhinted TTF), вшитые через `include_bytes!`.
//!
//! Состав — решение владельца: Noto Sans Regular + Bold (латиница, греческий, кириллица) как
//! основной шрифт и Regular основных алфавитов как fallback по глифам. CJK и эмодзи
//! сознательно не встроены (берутся из системы). Порядок в списке — порядок fallback.

use super::style::{FontStyle, FontWeightClass};

/// Зачем шрифт встроен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedFontPurpose {
    /// Основной шрифт простых субтитров (латиница + греческий + кириллица).
    PrimaryLatinGreekCyrillic,
    /// Fallback для отдельного алфавита (название алфавита — для диагностики).
    ScriptFallback {
        /// Человекочитаемое название алфавита (английское, для логов).
        script: &'static str,
    },
}

/// Описание одного встроенного шрифта.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedFont {
    /// Имя семейства, как в таблице `name` шрифта.
    pub family: &'static str,
    /// Насыщенность файла.
    pub weight: FontWeightClass,
    /// Наклон файла.
    pub style: FontStyle,
    /// Назначение.
    pub purpose: EmbeddedFontPurpose,
    /// Содержимое TTF.
    pub data: &'static [u8],
}

/// Короткая запись fallback-шрифта алфавита (Regular, прямой).
const fn script_font(
    family: &'static str,
    script: &'static str,
    data: &'static [u8],
) -> EmbeddedFont {
    EmbeddedFont {
        family,
        weight: FontWeightClass::Regular,
        style: FontStyle::Normal,
        purpose: EmbeddedFontPurpose::ScriptFallback { script },
        data,
    }
}

/// Все встроенные шрифты. Первые два — основной Noto Sans (Regular, Bold).
pub static EMBEDDED_FONTS: [EmbeddedFont; 22] = [
    EmbeddedFont {
        family: "Noto Sans",
        weight: FontWeightClass::Regular,
        style: FontStyle::Normal,
        purpose: EmbeddedFontPurpose::PrimaryLatinGreekCyrillic,
        data: include_bytes!("../../fonts/NotoSans-Regular.ttf"),
    },
    EmbeddedFont {
        family: "Noto Sans",
        weight: FontWeightClass::Bold,
        style: FontStyle::Normal,
        purpose: EmbeddedFontPurpose::PrimaryLatinGreekCyrillic,
        data: include_bytes!("../../fonts/NotoSans-Bold.ttf"),
    },
    script_font(
        "Noto Sans Arabic",
        "Arabic",
        include_bytes!("../../fonts/NotoSansArabic-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Hebrew",
        "Hebrew",
        include_bytes!("../../fonts/NotoSansHebrew-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Devanagari",
        "Devanagari",
        include_bytes!("../../fonts/NotoSansDevanagari-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Bengali",
        "Bengali",
        include_bytes!("../../fonts/NotoSansBengali-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Tamil",
        "Tamil",
        include_bytes!("../../fonts/NotoSansTamil-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Telugu",
        "Telugu",
        include_bytes!("../../fonts/NotoSansTelugu-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Kannada",
        "Kannada",
        include_bytes!("../../fonts/NotoSansKannada-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Malayalam",
        "Malayalam",
        include_bytes!("../../fonts/NotoSansMalayalam-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Gujarati",
        "Gujarati",
        include_bytes!("../../fonts/NotoSansGujarati-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Gurmukhi",
        "Gurmukhi",
        include_bytes!("../../fonts/NotoSansGurmukhi-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Thai",
        "Thai",
        include_bytes!("../../fonts/NotoSansThai-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Lao",
        "Lao",
        include_bytes!("../../fonts/NotoSansLao-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Georgian",
        "Georgian",
        include_bytes!("../../fonts/NotoSansGeorgian-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Armenian",
        "Armenian",
        include_bytes!("../../fonts/NotoSansArmenian-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Ethiopic",
        "Ethiopic",
        include_bytes!("../../fonts/NotoSansEthiopic-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Khmer",
        "Khmer",
        include_bytes!("../../fonts/NotoSansKhmer-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Myanmar",
        "Myanmar",
        include_bytes!("../../fonts/NotoSansMyanmar-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Sinhala",
        "Sinhala",
        include_bytes!("../../fonts/NotoSansSinhala-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Symbols",
        "Symbols",
        include_bytes!("../../fonts/NotoSansSymbols-Regular.ttf"),
    ),
    script_font(
        "Noto Sans Symbols 2",
        "Symbols 2",
        include_bytes!("../../fonts/NotoSansSymbols2-Regular.ttf"),
    ),
];
