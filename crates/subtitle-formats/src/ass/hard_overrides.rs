// Порт поведения libass 0.17.5 (ISC): ass_parse.c (ass_event_has_hard_overrides).
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

//! Поиск «жёстких» переопределений (`\pos`, `\move`, `\clip`, `\iclip`, `\org`, `\pbo`, `\p`)
//! по СЫРОМУ тексту реплики — точно как `ass_event_has_hard_overrides` в libass.
//!
//! Это не разбор тегов: libass просто сканирует строку и сравнивает префиксы после `\` внутри
//! `{...}`. Поэтому находятся и теги, которые настоящий разбор отбросил бы (`\pos(1)`), и теги
//! внутри `\t(...)`, и теги за пределом бюджета тегов. Любое имя, начинающееся на `p`
//! (`\p`, `\pos`, `\pbo`, `\pxyz`), тоже считается — как префикс `\p` у libass.

use subtitle_core::AssHardOverrides;

use super::scan::c_string_prefix;

/// Префиксы имён тегов, которые libass считает жёстким позиционированием.
const HARD_TAG_PREFIXES: [&[u8]; 7] = [b"pos", b"move", b"clip", b"iclip", b"org", b"pbo", b"p"];

/// Ищет жёсткие переопределения в сыром поле `Text` (после обрезки по первому NUL).
pub(super) fn scan_hard_overrides(raw_text: &str) -> AssHardOverrides {
    let bytes = c_string_prefix(raw_text).as_bytes();
    let mut index = 0usize;
    while let Some(&current) = bytes.get(index) {
        let next = bytes.get(index + 1).copied();
        if current == b'\\' && next.is_some() {
            // Экранирование вне блока (`\{`, `\N`): пропускаем пару, как `ass_get_next_char`.
            index += 2;
        } else if current == b'{' {
            index += 1;
            // Внутри блока обратная косая черта НЕ экранирует: ищем тег до `}` или конца.
            while let Some(&inner) = bytes.get(index) {
                if inner == b'}' {
                    break;
                }
                if inner == b'\\' && starts_hard_tag(bytes.get(index + 1..).unwrap_or_default()) {
                    return AssHardOverrides::Present;
                }
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    AssHardOverrides::Absent
}

/// Начинается ли хвост (сразу после `\`) с одного из жёстких имён.
fn starts_hard_tag(after_backslash: &[u8]) -> bool {
    HARD_TAG_PREFIXES
        .iter()
        .any(|prefix| after_backslash.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(text: &str) -> bool {
        scan_hard_overrides(text) == AssHardOverrides::Present
    }

    #[test]
    fn finds_every_hard_tag_even_when_the_real_parser_would_drop_it() {
        for text in [
            "{\\pos(1,2)}a",
            "{\\pos(1)}a",
            "{\\move(1,2,3,4)}a",
            "{\\clip(0,0,5,5)}a",
            "{\\iclip(0,0,5,5)}a",
            "{\\org(1,2)}a",
            "{\\pbo5}a",
            "{\\p1}m 0 0",
            "{\\fs20\\pos(1,2)}a",
            "{\\t(0,100,\\clip(0,0,5,5))}a",
        ] {
            assert!(present(text), "{text}");
        }
    }

    #[test]
    fn ignores_look_tags_escapes_and_text_outside_blocks() {
        for text in [
            "{\\b1\\fs20}plain",
            "plain \\pos(1,2) outside a block",
            "\\{\\pos(1,2)}",
            "{\\pos(1,2)",
            "no tags at all",
        ] {
            // Последний `{\pos(1,2)` без `}` всё равно сканируется до конца строки.
            let expected = text == "{\\pos(1,2)";
            assert_eq!(present(text), expected, "{text}");
        }
    }

    #[test]
    fn tag_beyond_a_nul_is_not_seen() {
        assert!(!present("a\0{\\pos(1,2)}"));
    }
}
