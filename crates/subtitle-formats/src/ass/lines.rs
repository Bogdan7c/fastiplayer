// Порт поведения libass 0.17.5 (ISC): ass.c (process_text).
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

//! Разбиение текста ASS на строки (порт цикла `process_text` из libass).

/// Итератор по непустым строкам: разделители `\r`, `\n` и `\r\n`; пустые строки пропускаются;
/// U+FEFF (BOM) в начале строки отбрасывается. Вместе со строкой отдаётся номер физической
/// строки с нуля (для сообщений об ошибках).
pub(super) struct AssLines<'a> {
    rest: &'a str,
    physical_line_index: usize,
}

impl<'a> AssLines<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self {
            rest: text,
            physical_line_index: 0,
        }
    }
}

impl<'a> Iterator for AssLines<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            let line_end = self.rest.find(['\r', '\n']).unwrap_or(self.rest.len());
            let line = self.rest.get(..line_end).unwrap_or_default();
            let terminator = self.rest.get(line_end..).unwrap_or_default();
            // `\r\n` — один разделитель; одиночный `\r` или `\n` — тоже.
            let terminator_len = if terminator.starts_with("\r\n") {
                2
            } else {
                usize::from(!terminator.is_empty())
            };
            let current_index = self.physical_line_index;
            if terminator_len > 0 {
                self.physical_line_index += 1;
            }
            self.rest = terminator.get(terminator_len..).unwrap_or_default();
            let line = line.strip_prefix('\u{feff}').unwrap_or(line);
            if !line.is_empty() {
                return Some((current_index, line));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(text: &str) -> Vec<(usize, &str)> {
        AssLines::new(text).collect()
    }

    #[test]
    fn splits_on_all_terminators_and_keeps_physical_numbers() {
        assert_eq!(
            collect("a\r\n\r\nb\nc\rd"),
            vec![(0, "a"), (2, "b"), (3, "c"), (4, "d")]
        );
    }

    #[test]
    fn strips_bom_at_line_start_and_skips_empty_lines() {
        assert_eq!(
            collect("\u{feff}[Script Info]\n\u{feff}\nx"),
            vec![(0, "[Script Info]"), (2, "x")]
        );
    }

    #[test]
    fn empty_text_has_no_lines() {
        assert!(collect("").is_empty());
        assert!(collect("\n\r\n").is_empty());
    }
}
