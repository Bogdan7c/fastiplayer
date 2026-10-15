// Порт из libass 0.17.5: ass_shaper.c (ass_shaper_determine_script, script_to_language,
// is_harfbuzz_ignorable, hb_shaper_get_run_language).
//
// Copyright (C) 2011 Grigori Goronzy <greg@chown.ath.cx>
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

//! Письменность символов и язык участков шейпинга.

use harfrust::{Language, Script, UnicodeBuffer, script};

/// Письменность символа или `None`, если она определяется по контексту (общие и
/// наследуемые символы: пробелы, цифры, знаки препинания, комбинируемые метки).
///
/// harfrust не раскрывает таблицу Unicode Script напрямую; её знает
/// `UnicodeBuffer::guess_segment_properties`, который берёт письменность первого символа с
/// собственной письменностью. Для одного символа это ровно `hb_unicode_script`, где
/// «нет собственной письменности» совпадает с Common/Inherited/Unknown.
fn own_script(ch: char) -> Option<Script> {
    let mut buffer = UnicodeBuffer::new();
    buffer.add(ch, 0);
    buffer.guess_segment_properties();
    let detected = buffer.script();
    (detected != script::UNKNOWN && detected != script::COMMON && detected != script::INHERITED)
        .then_some(detected)
}

/// Письменность каждого символа (libass: `ass_shaper_determine_script`).
///
/// Общие и наследуемые символы получают письменность предыдущего символа с собственной
/// письменностью; если такого нет (текст начинается с цифры или пробела) — следующего.
/// Если письменности нет нигде — `script::COMMON`.
pub(crate) fn determine_scripts(symbols: &[char]) -> Vec<Script> {
    let own: Vec<Option<Script>> = symbols.iter().map(|ch| own_script(*ch)).collect();
    let mut resolved: Vec<Option<Script>> = Vec::with_capacity(own.len());
    let mut last_script: Option<Script> = None;
    for detected in &own {
        match detected {
            Some(found) => {
                last_script = Some(*found);
                resolved.push(Some(*found));
            }
            None => resolved.push(last_script),
        }
    }
    // Обратный проход для символов в начале, у которых слева контекста не было.
    let mut next_script: Option<Script> = None;
    for (index, slot) in resolved.iter_mut().enumerate().rev() {
        match own.get(index).copied().flatten() {
            Some(found) => next_script = Some(found),
            None if slot.is_none() => *slot = next_script,
            None => {}
        }
    }
    resolved
        .into_iter()
        .map(|found| found.unwrap_or(script::COMMON))
        .collect()
}

/// Язык по умолчанию для письменности (libass: `script_to_language`, таблица как у Pango).
///
/// `None` — у письменности нет представительного языка. libass в этом случае берёт язык
/// системной локали; мы язык не задаём, чтобы результат не зависел от машины.
pub(crate) fn language_for_script(script_value: Script) -> Option<Language> {
    const TABLE: &[(Script, &str)] = &[
        (script::ARABIC, "ar"),
        (script::ARMENIAN, "hy"),
        (script::BENGALI, "bn"),
        (script::CANADIAN_SYLLABICS, "iu"),
        (script::CHEROKEE, "chr"),
        (script::COPTIC, "cop"),
        (script::CYRILLIC, "ru"),
        (script::DEVANAGARI, "hi"),
        (script::GEORGIAN, "ka"),
        (script::GREEK, "el"),
        (script::GUJARATI, "gu"),
        (script::GURMUKHI, "pa"),
        (script::HANGUL, "ko"),
        (script::HEBREW, "he"),
        (script::HIRAGANA, "ja"),
        (script::KANNADA, "kn"),
        (script::KATAKANA, "ja"),
        (script::LAO, "lo"),
        (script::LATIN, "en"),
        (script::MALAYALAM, "ml"),
        (script::MONGOLIAN, "mn"),
        (script::ORIYA, "or"),
        (script::SYRIAC, "syr"),
        (script::TAMIL, "ta"),
        (script::TELUGU, "te"),
        (script::THAI, "th"),
        (script::TIBETAN, "bo"),
        (script::ETHIOPIC, "am"),
        (script::KHMER, "km"),
        (script::MYANMAR, "my"),
        (script::SINHALA, "si"),
        (script::THAANA, "dv"),
        (script::BUHID, "bku"),
        (script::HANUNOO, "hnn"),
        (script::TAGALOG, "tl"),
        (script::TAGBANWA, "tbw"),
        (script::UGARITIC, "uga"),
        (script::BUGINESE, "bug"),
        (script::OLD_PERSIAN, "peo"),
        (script::SYLOTI_NAGRI, "syl"),
        (script::NKO, "nko"),
    ];
    TABLE
        .iter()
        .find(|(known, _)| *known == script_value)
        .and_then(|(_, tag)| Language::new(tag))
}

/// HarfBuzz заменяет этот символ нулевым пробелом и не рисует (libass:
/// `is_harfbuzz_ignorable`, подмножество Default_Ignorable_Code_Point из hb-unicode).
pub(crate) fn is_harfbuzz_ignorable(symbol: char) -> bool {
    let code = u32::from(symbol);
    match code >> 8 {
        0x00 => code == 0x00AD,
        0x03 => code == 0x034F,
        0x06 => code == 0x061C,
        0x17 => (0x17B4..=0x17B5).contains(&code),
        0x18 => (0x180B..=0x180E).contains(&code),
        0x20 => {
            (0x200B..=0x200F).contains(&code)
                || (0x202A..=0x202E).contains(&code)
                || (0x2060..=0x206F).contains(&code)
        }
        0xFE => (0xFE00..=0xFE0F).contains(&code) || code == 0xFEFF,
        0xFF => (0xFFF0..=0xFFF8).contains(&code),
        0x1D1 => (0x1D173..=0x1D17A).contains(&code),
        _ => (0xE0000..=0xE0FFF).contains(&code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_symbols_inherit_script_from_context() {
        // Пробел и запятая между кириллицей наследуют кириллицу, цифра после латиницы — латиницу.
        let text: Vec<char> = "Привет, мир 5 abc 7".chars().collect();
        let scripts = determine_scripts(&text);
        assert_eq!(scripts[0], script::CYRILLIC);
        assert_eq!(scripts[6], script::CYRILLIC, "comma inherits Cyrillic");
        assert_eq!(scripts[12], script::CYRILLIC, "digit after Cyrillic text");
        assert_eq!(scripts[14], script::LATIN);
        assert_eq!(scripts[18], script::LATIN, "trailing digit follows Latin");
    }

    #[test]
    fn leading_common_symbols_take_the_following_script() {
        let text: Vec<char> = "  12 مرحبا".chars().collect();
        let scripts = determine_scripts(&text);
        assert_eq!(scripts[0], script::ARABIC);
        assert_eq!(scripts[3], script::ARABIC);
    }

    #[test]
    fn text_without_any_script_stays_common() {
        let text: Vec<char> = "12 !".chars().collect();
        assert!(
            determine_scripts(&text)
                .iter()
                .all(|s| *s == script::COMMON)
        );
    }

    #[test]
    fn language_table_maps_known_scripts_only() {
        let arabic = language_for_script(script::ARABIC).expect("Arabic has a language");
        assert_eq!(arabic.as_str(), "ar");
        assert!(language_for_script(script::COMMON).is_none());
        assert!(language_for_script(script::HAN).is_none());
    }

    #[test]
    fn ignorable_set_matches_harfbuzz_subset() {
        for ch in ['\u{00AD}', '\u{200B}', '\u{200D}', '\u{202E}', '\u{FEFF}'] {
            assert!(is_harfbuzz_ignorable(ch), "{ch:?} is ignorable");
        }
        for ch in ['a', ' ', '\u{0627}', '\u{2010}'] {
            assert!(!is_harfbuzz_ignorable(ch), "{ch:?} is visible");
        }
    }
}
