// Порт из libass 0.17.5: ass_parse.c (ветки `kt`, `kf`/`K`, `ko`, `k` в `ass_parse_tags`) и
// ass_render.c (захват эффекта в `parse_events`, правила `split_style_runs`).
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

//! Захват тегов караоке на символ.

/// Вид караоке-эффекта слога (libass: `Effect`, без `EF_NONE`: «нет эффекта» — это `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KaraokeEffect {
    /// `\kf`, `\K` (libass: `EF_KARAOKE_KF`): плавная заливка слева направо.
    Fill,
    /// `\ko` (`EF_KARAOKE_KO`): цвет меняется скачком, обводка до начала слога скрыта.
    Outline,
    /// `\k` (`EF_KARAOKE`): цвет меняется скачком.
    Plain,
}

/// Эффект, «прикреплённый» к символу: всё, что накопили теги с прошлого символа.
///
/// libass складывает эти поля в `RenderContext` по мере разбора тегов, а `parse_events` при
/// добавлении очередного глифа копирует их в `GlyphInfo` и сразу сбрасывает. Времена —
/// миллисекунды (`dtoi32(сотые * 10)`) в `i32`, переполнение оборачивается, как у `uint32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct KaraokeMark {
    /// Вид эффекта слога; `None` — к символу тег `\k*` не относится.
    pub(crate) effect: Option<KaraokeEffect>,
    /// Длительность слога (`effect_timing`), мс.
    pub(crate) timing_ms: i32,
    /// Накопленный пропуск перед слогом (`effect_skip_timing`), мс.
    pub(crate) skip_ms: i32,
    /// Был `\kt`: время слова отсчитывается заново (`reset_effect`).
    pub(crate) resets_timeline: bool,
}

impl KaraokeMark {
    /// У символа нет караоке-тегов.
    pub(crate) const NONE: Self = Self {
        effect: None,
        timing_ms: 0,
        skip_ms: 0,
        resets_timeline: false,
    };

    /// Начало слога: `\k`, `\kf`, `\K`, `\ko` (libass: общая часть трёх веток).
    ///
    /// `skip += (uint32_t) timing` прибавляет длительность ПРЕДЫДУЩЕГО слога без символа между
    /// тегами (`\k10\k20`): тот слог теряет текст, но его время сохраняется как пропуск.
    pub(crate) fn begin_syllable(&mut self, effect: KaraokeEffect, timing_ms: i32) {
        self.effect = Some(effect);
        self.skip_ms = self.skip_ms.wrapping_add(self.timing_ms);
        self.timing_ms = timing_ms;
    }

    /// `\kt`: пропуск присваивается (не накапливается), длительность обнуляется, время слова
    /// начинается заново; вид эффекта не меняется.
    pub(crate) fn skip_timeline(&mut self, skip_ms: i32) {
        self.skip_ms = skip_ms;
        self.timing_ms = 0;
        self.resets_timeline = true;
    }

    /// Забирает накопленное и сбрасывает (libass: хвост цикла `parse_events` после глифа).
    pub(crate) fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    /// Правило `split_style_runs` для караоке: символ начинает новый участок, если у него
    /// ненулевая длительность (пропуск не считается) или вид эффекта отличается от последнего
    /// заданного вида (`last_effect` — вид последнего символа с эффектом до него).
    pub(crate) fn splits_run(&self, last_effect: Option<KaraokeEffect>) -> bool {
        self.timing_ms != 0 || (self.effect.is_some() && self.effect != last_effect)
    }
}
