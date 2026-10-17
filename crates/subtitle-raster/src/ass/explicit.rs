// Порт из libass 0.17.5: ass_parse.c (ass_event_has_hard_overrides) и ass_parse.c
// (ass_apply_transition_effects — только условия, при которых реплика становится «явной»).
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

//! «Явные» реплики: те, к которым пользовательское переопределение стиля не должно лезть.
//!
//! libass называет реплику явной (`state->explicit`), если автор жёстко расставил её на кадре:
//! `\pos`, `\move`, `\clip`, `\iclip`, `\org`, `\pbo`, `\p` (рисунок), либо задан эффект
//! `Banner`/`Scroll up`/`Scroll down`. Для такой реплики пользовательский масштаб текста
//! (`font_size_coeff`) не применяется, чтобы надпись не уехала от места, на которое рассчитана.
//!
//! Теги ищет `ass_event_has_hard_overrides` по СЫРОМУ тексту; этот поиск выполнен при разборе
//! (`subtitle-formats`) и лежит в [`AssCue::hard_overrides`], поэтому здесь он не повторяется.

use subtitle_core::{AssCue, AssEffect, AssHardOverrides};

/// Явная ли реплика (libass: `explicit`).
pub(crate) fn is_explicit_event(cue: &AssCue) -> bool {
    cue.transition_effect() != AssEffect::None || cue.hard_overrides == AssHardOverrides::Present
}

#[cfg(test)]
mod tests {
    use subtitle_core::AssTextItem;

    use super::*;
    use crate::ass::test_script::{cue_with, default_style, script_with};

    fn cue(effect: &str, hard_overrides: AssHardOverrides) -> AssCue {
        let script = script_with(vec![default_style()]);
        let mut cue = cue_with(&script, vec![AssTextItem::Text("x".to_owned())]);
        cue.effect = effect.to_owned();
        cue.hard_overrides = hard_overrides;
        cue
    }

    #[test]
    fn hard_overrides_found_at_parse_time_make_an_event_explicit() {
        assert!(is_explicit_event(&cue("", AssHardOverrides::Present)));
        assert!(!is_explicit_event(&cue("", AssHardOverrides::Absent)));
    }

    #[test]
    fn banner_and_scroll_effects_need_enough_parameters() {
        let absent = AssHardOverrides::Absent;
        assert!(is_explicit_event(&cue("Banner;50;0;80", absent)));
        assert!(is_explicit_event(&cue("Banner;", absent)));
        assert!(is_explicit_event(&cue("Scroll up;10;200;5;0", absent)));
        // Мало параметров: libass эффект не применяет, реплика остаётся обычной.
        assert!(!is_explicit_event(&cue("Scroll up;10", absent)));
        assert!(!is_explicit_event(&cue("Karaoke", absent)));
    }
}
