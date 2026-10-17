// Порт из libass 0.17.5: ass_parse.c (`mult_alpha`, `ass_apply_fade`).
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

//! Затухание реплики (`\fad`, `\fade`) в момент показа: одно число на реплику.
//!
//! libass хранит затухание как «прозрачность, которую домножают на прозрачность каждого из
//! четырёх цветов» (`state->fade`, 0..255). Конвейер рисования его не вычисляет (время ему
//! неизвестно): значение приходит готовым в [`EventFade`] из модуля анимации ASS и применяется
//! в одном месте — при окраске готовых картинок. Логика «заливка обводки» смотрит на
//! [`EventFade::is_none`] (libass: `info->fade == 0`), поэтому значения самих цветов
//! (`SpanLook::colors`) остаются «до затухания».

use subtitle_core::SubtitleColor;

/// Прозрачность затухания реплики в момент показа.
///
/// `0` — затухания нет. Значение может быть и отрицательным или больше 255 (так пишет автор
/// `\fade`): libass применяет затухание только при `> 0`, а «заливку обводки» отключает при
/// любом `!= 0`, поэтому число хранится как есть.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EventFade(i32);

impl EventFade {
    /// Нет затухания.
    pub(crate) const NONE: Self = Self(0);

    /// Затухание с прозрачностью `value` (в шкале libass: 255 — полностью прозрачно).
    pub(crate) const fn new(value: i32) -> Self {
        Self(value)
    }

    /// Исходное число (для ключей кэша).
    pub(crate) const fn value(self) -> i32 {
        self.0
    }

    /// Затухания нет (libass: `fade == 0`; отрицательное значение «затуханием» остаётся).
    pub(crate) const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Применяет затухание к цвету картинки (libass: `ass_apply_fade`).
    ///
    /// Цвет конвейера хранит НЕпрозрачность, libass — прозрачность, поэтому значение
    /// переводится туда и обратно. Затухание `<= 0` цвет не меняет («совместимость с VSFilter»).
    pub(crate) fn apply_to(self, color: SubtitleColor) -> SubtitleColor {
        let Ok(fade) = u32::try_from(self.0) else {
            return color;
        };
        if fade == 0 {
            return color;
        }
        let transparency = u32::from(u8::MAX - color.alpha);
        // libass приводит результат к байту (`(uint8_t)`): для значений сверх 255 это отсечение
        // старших битов, а не насыщение.
        let faded = (mult_alpha(transparency, fade) & 0xFF) as u8;
        SubtitleColor {
            alpha: u8::MAX - faded,
            ..color
        }
    }
}

/// Произведение двух прозрачностей (libass: `mult_alpha`): `a + b - a*b/255` с округлением.
///
/// Арифметика 64-битная без паники, как в C: результат приводится к 32 битам.
fn mult_alpha(first: u32, second: u32) -> u32 {
    let product = (u64::from(first) * u64::from(second) + 0x7F) / 0xFF;
    u64::from(first)
        .wrapping_sub(product)
        .wrapping_add(u64::from(second)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(alpha: u8) -> SubtitleColor {
        SubtitleColor {
            red: 10,
            green: 20,
            blue: 30,
            alpha,
        }
    }

    #[test]
    fn no_fade_leaves_the_color_unchanged() {
        assert_eq!(EventFade::NONE.apply_to(color(200)), color(200));
        // Отрицательное затухание цвет не трогает, но «затуханием» остаётся.
        assert_eq!(EventFade::new(-5).apply_to(color(200)), color(200));
        assert!(!EventFade::new(-5).is_none());
    }

    #[test]
    fn full_fade_makes_any_color_transparent() {
        assert_eq!(EventFade::new(255).apply_to(color(255)).alpha, 0);
        assert_eq!(EventFade::new(255).apply_to(color(77)).alpha, 0);
    }

    #[test]
    fn half_fade_of_an_opaque_color_gives_half_opacity() {
        // transparency = 0, fade = 128: 0 - (0+0x7F)/0xFF + 128 = 128 -> opacity 127.
        assert_eq!(EventFade::new(128).apply_to(color(255)).alpha, 127);
    }

    #[test]
    fn fade_of_a_semi_transparent_color_multiplies_transparencies() {
        // opacity 128 -> transparency 127; fade 128:
        // 127 - (127*128 + 127)/255 + 128 = 127 - 64 + 128 = 191 -> opacity 64.
        assert_eq!(EventFade::new(128).apply_to(color(128)).alpha, 64);
    }

    #[test]
    fn huge_fade_does_not_panic_and_truncates_like_libass() {
        let result = EventFade::new(i32::MAX).apply_to(color(255));
        let expected = (mult_alpha(0, i32::MAX as u32) & 0xFF) as u8;
        assert_eq!(result.alpha, u8::MAX - expected);
    }
}
