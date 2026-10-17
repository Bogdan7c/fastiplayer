// Порт поведения libass 0.17.5 (ISC): ass_parse.c (ветка `complex_tag("t")` в ass_parse_tags).
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

//! Сборка тега `\t(...)` из уже разделённых аргументов (разделение — в `tags.rs`).
//!
//! Что повторено дословно:
//! - число аргументов `cnt = nargs - 1` выбирает формат времён (см. [`AssTransformTiming`]);
//!   пустые аргументы (`\t(,500,\fs5)`) в `nargs` не входят, как в `push_arg`;
//! - вложенные теги берутся из ПОСЛЕДНЕГО аргумента, только если он содержал `\`;
//! - при `cnt < 0 || cnt > 3` вложенные теги не разбираются совсем (в libass они тоже не
//!   разбираются и поэтому не считаются ни неизвестными, ни тратящими бюджет);
//! - первая `)` обрывает аргумент (совместимость с VSFilter): всё, что записано после неё в
//!   том же блоке, внешний разбор продолжает читать как обычные теги блока — это обеспечивает
//!   `BlockCursor::split_parenthesized_args`, возвращающий позицию сразу за `)`.
//!
//! Защита от больших входов (в libass её нет): каждый вложенный тег тратит бюджет реплики,
//! а глубина вложенности `\t(\t(\t(...` ограничена [`MAX_TRANSFORM_NESTING`]. Вложенный `\t`
//! внутри `\t` всегда «незакрыт» (внешний аргумент уже оборван первой `)`), поэтому реальные
//! файлы глубже одного-двух уровней не встречаются.

use subtitle_core::{AssTransform, AssTransformTiming};

use super::report::AssTagStats;
use super::scan::{parse_f64, parse_i32_clamped};
use super::tags::{TagArgs, TagBudget, parse_tags};

/// Максимальная глубина вложенных `\t` (внешний блок реплики — глубина 0). Теги глубже
/// отбрасываются и считаются в `over_limit`; на реальных файлах граница не достигается.
pub(super) const MAX_TRANSFORM_NESTING: usize = 16;

/// Наибольшее значение `cnt`, для которого libass разбирает вложенные теги.
const MAX_VALID_TRANSFORM_COUNT: usize = 3;

/// Аргумент `argtoi32`.
fn arg_i32(text: &str) -> i32 {
    parse_i32_clamped(text, 10)
}

/// `dtoi32` libass: значение вне `i32` и NaN дают `INT32_MIN`, иначе усечение к нулю.
fn dtoi32(value: f64) -> i32 {
    if value.is_nan() || value <= f64::from(i32::MIN) || value >= f64::from(i32::MAX) + 1.0 {
        return i32::MIN;
    }
    value as i32
}

/// Строит [`AssTransform`] из аргументов `\t`. `nesting_depth` — глубина, на которой стоит сам
/// этот `\t`.
pub(super) fn build_transform(
    args: &TagArgs<'_>,
    budget: &mut TagBudget,
    stats: &mut AssTagStats,
    nesting_depth: usize,
) -> AssTransform {
    let provided = args.provided();
    let timing = transform_timing(provided);
    let tags = match nested_tags_text(args) {
        Some(text) if nesting_depth < MAX_TRANSFORM_NESTING => {
            parse_tags(text, budget, stats, nesting_depth + 1)
        }
        Some(_) => {
            // Слишком глубокая вложенность: тег сохраняется (он влияет на `pwr` и коллизии),
            // но его содержимое отбрасывается и считается как превышение лимита.
            stats.record_over_limit();
            Vec::new()
        }
        None => Vec::new(),
    };
    AssTransform { timing, tags }
}

/// Времена и ускорение по числу аргументов (`cnt = nargs - 1`).
fn transform_timing(provided: &[&str]) -> AssTransformTiming {
    match provided {
        [t1, t2, accel, _] => AssTransformTiming::RangeAccel {
            t1_ms: arg_i32(t1),
            t2_ms: arg_i32(t2),
            accel: parse_f64(accel),
        },
        [t1, t2, _] => AssTransformTiming::Range {
            t1_ms: dtoi32(parse_f64(t1)),
            t2_ms: dtoi32(parse_f64(t2)),
        },
        [accel, _] => AssTransformTiming::Accel(parse_f64(accel)),
        // `cnt == 0` и все некорректные формы (нет аргументов, больше четырёх).
        _ => AssTransformTiming::Default,
    }
}

/// Текст вложенных тегов: последний аргумент, если форма корректна и в нём была `\`.
fn nested_tags_text<'a>(args: &TagArgs<'a>) -> Option<&'a str> {
    let provided = args.provided();
    let is_valid_form = (1..=MAX_VALID_TRANSFORM_COUNT + 1).contains(&provided.len());
    if !is_valid_form || !args.has_backslash_arg() {
        return None;
    }
    provided.last().copied()
}
