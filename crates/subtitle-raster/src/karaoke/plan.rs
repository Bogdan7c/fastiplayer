// Порт из libass 0.17.5: ass_parse.c (`ass_process_karaoke_effects`, часть без `tm_current`).
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

//! Время и геометрия «слов» караоке, не зависящие от кадра.
//!
//! libass считает всё в одной функции с `tm_current = render_time - Start`. Здесь она
//! разделена: [`KaraokePlan::build`] делает то, что от кадра не зависит (границы слов,
//! `tm_start`/`tm_end`, крайние видимые глифы, направление заливки), а
//! [`KaraokeWord::progress_at`] — часть, которой нужен момент кадра.

use super::mark::KaraokeEffect;
use crate::glyph_info::TextCluster;

/// Значение `x` до начала слога (libass: `-100000000`).
const X_BEFORE_WORD: i32 = -100_000_000;
/// Значение `x` после конца слога (libass: `100000000`).
const X_AFTER_WORD: i32 = 100_000_000;

/// Слово караоке: подряд идущие символы одного участка с общим эффектом.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct KaraokeWord {
    /// Вид эффекта слова (унаследованный от предыдущих слов, если свой не задан).
    pub(crate) effect: KaraokeEffect,
    /// Начало слога, мс от начала реплики (`tm_start`).
    pub(crate) start_ms: i64,
    /// Конец слога (`tm_end`); у всех видов, кроме [`KaraokeEffect::Fill`], равен началу.
    pub(crate) end_ms: i64,
    /// Левый край первого видимого глифа в предварительной раскладке, 26.6 (`x_start`).
    x_start: i32,
    /// Правый край последнего видимого глифа в предварительной раскладке, 26.6 (`x_end`).
    x_end: i32,
    /// Заливка идёт справа налево: `fmod(frz, 360)` в `(90, 270)`, цвета меняются местами.
    reversed: bool,
}

/// Где слово находится на момент кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WordProgress {
    /// Слог ещё не начался (`x = -100000000`).
    NotStarted,
    /// Слог закончился (`x = 100000000`).
    Finished,
    /// Идёт заливка: граница слова `split_x` (26.6, предварительная раскладка).
    Filling {
        /// Граница между «спетой» и «неспетой» частями слова, 26.6.
        split_x: i32,
        /// Цвета слова поменяны местами (заливка справа налево).
        colors_swapped: bool,
    },
}

impl KaraokeWord {
    /// Положение слова на моменте `elapsed_ms` (libass: ветка `if (tm_current < tm_start) ...`).
    pub(crate) fn progress_at(&self, elapsed_ms: i64) -> WordProgress {
        if elapsed_ms < self.start_ms {
            return WordProgress::NotStarted;
        }
        if elapsed_ms >= self.end_ms {
            return WordProgress::Finished;
        }
        // Сюда попадает только Fill: у остальных видов конец равен началу.
        let mut progress =
            (elapsed_ms - self.start_ms) as f64 / (self.end_ms - self.start_ms) as f64;
        if self.reversed {
            progress = 1.0 - progress;
        }
        let width = i64::from(self.x_end.wrapping_sub(self.x_start));
        // `x = x_start + ass_lrint((x_end - x_start) * dt)`: сумма в `long`, в `int` — усечение.
        let advanced = ((width as f64) * progress).round_ties_even() as i64;
        let split_x = (i64::from(self.x_start) + advanced) as i32;
        WordProgress::Filling {
            split_x,
            colors_swapped: self.reversed,
        }
    }
}

impl WordProgress {
    /// `x` слова в терминах libass: по нему считается `effect_timing = x - pos.x` каждого глифа.
    pub(crate) fn x(self) -> i32 {
        match self {
            Self::NotStarted => X_BEFORE_WORD,
            Self::Finished => X_AFTER_WORD,
            Self::Filling { split_x, .. } => split_x,
        }
    }

    /// Цвета слова поменяны местами.
    pub(crate) fn colors_swapped(self) -> bool {
        matches!(
            self,
            Self::Filling {
                colors_swapped: true,
                ..
            }
        )
    }
}

/// Слова караоке одной раскладки и положение их символов.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct KaraokePlan {
    words: Vec<KaraokeWord>,
    /// Для каждого символа (в логическом порядке): номер слова или `None`. Пусто, если в
    /// реплике нет караоке.
    word_of_cluster: Vec<Option<u32>>,
    /// Позиция корневого глифа каждого символа в предварительной раскладке (`info->pos.x` на
    /// момент `ass_process_karaoke_effects`), 26.6. Пусто, если в реплике нет караоке.
    origin_x: Vec<i32>,
}

impl KaraokePlan {
    /// Слово символа `cluster_index` и позиция его глифа в предварительной раскладке.
    pub(crate) fn word_at(&self, cluster_index: usize) -> Option<(KaraokeWord, i32)> {
        let word_index = (*self.word_of_cluster.get(cluster_index)?)?;
        let word = *self.words.get(usize::try_from(word_index).ok()?)?;
        let origin = *self.origin_x.get(cluster_index)?;
        Some((word, origin))
    }

    /// Строит слова по раскладке после переноса строк и до порядка BiDi (позиции глифов
    /// должны быть предварительными, а `starts_new_run` и `skip` — уже окончательными).
    pub(crate) fn build(clusters: &[TextCluster]) -> Self {
        if !clusters
            .iter()
            .any(|cluster| cluster.karaoke.effect.is_some())
        {
            return Self::default();
        }
        let mut plan = Self {
            words: Vec::new(),
            word_of_cluster: vec![None; clusters.len()],
            origin_x: clusters.iter().map(|cluster| cluster.root.pos.x).collect(),
        };
        let mut timeline = Timeline::default();
        let mut last_boundary: Option<usize> = None;
        for index in 0..=clusters.len() {
            let inside_word = clusters
                .get(index)
                .is_some_and(|cluster| !cluster.starts_new_run);
            if inside_word {
                if let Some(cluster) = clusters.get(index) {
                    timeline.absorb_inner_symbol(cluster);
                }
                continue;
            }
            let start = last_boundary;
            last_boundary = Some(index);
            let Some(start) = start else { continue };
            plan.close_word(clusters, start..index, &mut timeline);
        }
        plan
    }

    /// Завершает слово `symbols` (индексы символов).
    fn close_word(
        &mut self,
        clusters: &[TextCluster],
        symbols: std::ops::Range<usize>,
        timeline: &mut Timeline,
    ) {
        let Some(first) = clusters.get(symbols.start) else {
            return;
        };
        let Some(times) = timeline.close_word(&first.karaoke) else {
            return;
        };
        let (x_start, x_end) = visible_span(clusters, symbols.clone());
        // `fmod` знаковый: -180 в диапазон не попадает.
        let frz = first.rotation_z_degrees % 360.0;
        let word_index = u32::try_from(self.words.len()).unwrap_or(u32::MAX);
        self.words.push(KaraokeWord {
            effect: times.effect,
            start_ms: times.start_ms,
            end_ms: times.end_ms,
            x_start,
            x_end,
            reversed: frz > 90.0 && frz < 270.0,
        });
        for slot in self.word_of_cluster.get_mut(symbols).into_iter().flatten() {
            *slot = Some(word_index);
        }
    }
}

/// Границы слова по видимым глифам: от левого края первого до правого края последнего.
///
/// libass: `first_visible`/`last_visible` сдвигаются только пока `first < last`, поэтому у слова
/// из одних невидимых символов границами остаются крайние (пусть и невидимые) глифы.
fn visible_span(clusters: &[TextCluster], symbols: std::ops::Range<usize>) -> (i32, i32) {
    let mut first = symbols.start;
    let mut last = symbols.end.saturating_sub(1).max(symbols.start);
    let skipped = |index: usize| clusters.get(index).is_some_and(|cluster| cluster.skip);
    while first < last && skipped(first) {
        first += 1;
    }
    while first < last && skipped(last) {
        last -= 1;
    }
    let x_start = clusters.get(first).map_or(0, |cluster| cluster.root.pos.x);
    let x_end = clusters.get(last).map_or(0, |cluster| {
        cluster.root.pos.x.wrapping_add(cluster.root.advance.x)
    });
    (x_start, x_end)
}

/// Накопленное время слов (локальные `timing`, `skip_timing`, `effect_type`, `has_reset`).
#[derive(Debug, Default)]
struct Timeline {
    /// Конец прошлого слога плюс пропуск (`timing`).
    timing: i32,
    /// Пропуск, накопленный внутренними символами слова (`skip_timing`).
    skip_timing: i32,
    /// Последний заданный вид эффекта (`effect_type`).
    effect: Option<KaraokeEffect>,
    /// Во внутренних символах слова был `\kt` (`has_reset`).
    has_reset: bool,
}

/// Время закрытого слова.
struct WordTimes {
    effect: KaraokeEffect,
    start_ms: i64,
    end_ms: i64,
}

impl Timeline {
    /// Символ внутри слова (не первый): его `\kt` и пропуск двигают начало СЛЕДУЮЩЕГО слова.
    ///
    /// Совместимость с VSFilter: `\k12345\k0` без разрыва участка оставляет текст в том же
    /// слове, время слова не меняется, а следующее слово начинается позже.
    fn absorb_inner_symbol(&mut self, cluster: &TextCluster) {
        if cluster.karaoke.resets_timeline {
            self.has_reset = true;
            self.skip_timing = 0;
        }
        self.skip_timing = self.skip_timing.wrapping_add(cluster.karaoke.skip_ms);
    }

    /// Закрывает слово, начинающееся символом с захватом `start`. `None` — у слова нет
    /// эффекта (и унаследовать нечего): libass пропускает его, не трогая накопленное.
    fn close_word(&mut self, start: &super::KaraokeMark) -> Option<WordTimes> {
        if let Some(effect) = start.effect {
            self.effect = Some(effect);
        }
        let effect = self.effect?;
        if start.resets_timeline {
            self.timing = 0;
        }
        // `timing + skip` — арифметика `int`, дальше `long long`.
        let start_ms = i64::from(self.timing.wrapping_add(start.skip_ms));
        let mut end_ms = start_ms + i64::from(start.timing_ms);
        let carried = if self.has_reset { 0 } else { end_ms };
        // `timing = !has_reset * tm_end + skip_timing`: запись в `int32_t` усекает.
        self.timing = (carried + i64::from(self.skip_timing)) as i32;
        self.skip_timing = 0;
        self.has_reset = false;
        if effect != KaraokeEffect::Fill {
            end_ms = start_ms;
        }
        Some(WordTimes {
            effect,
            start_ms,
            end_ms,
        })
    }
}
