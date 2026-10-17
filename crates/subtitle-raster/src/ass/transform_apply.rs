// Порт из libass 0.17.5: ass_parse.c (ветка `complex_tag("t")` в `ass_parse_tags`: рекурсивный
// разбор вложенных тегов с подменой `pwr`).
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

//! Обход вложенных тегов `\t` с правилами подмены веса, общий для всех «получателей» тегов.
//!
//! Получатель ([`TagSink`]) — любое состояние, которое умеет применить один тег с заданным
//! весом: полное состояние рендера ([`super::render_state::RenderState`]) или только фильтры
//! реплики ([`super::filter_params::FilterParams`], для покадрового клипа). Правила обхода
//! (одни и те же для обоих) такие:
//! - вложенные теги `\t` применяются с весом `k` этого `\t` (внешний `\t` вне `\t` имеет вес 1);
//! - вложенный `\t` ЗАМЕНЯЕТ вес (а не умножает), причём заменённый вес действует и на теги,
//!   записанные после него в том же списке (`if (nested) pwr = k`);
//! - тег, у которого нет анимации (`\pos`, `\b`, `\fn`...), применяется сразу и целиком, даже
//!   при `k = 0`: получатель просто игнорирует вес.

use subtitle_core::{AssOverride, AssTransform};

use super::animation::AssEventClock;
use super::tag_weight::{TagWeight, transform_weight};

/// Состояние, к которому применяются теги с весом.
pub(crate) trait TagSink {
    /// Применяет один тег (не `\t`) с весом `weight`. Теги без анимации вес игнорируют.
    fn apply_tag(&mut self, tag: &AssOverride, weight: TagWeight);
}

/// Вес `\t` на часах; без часов анимация считается завершённой (вес 1): так ведёт себя
/// растеризация без привязки ко времени (тесты, простой разбор).
fn weight_of(transform: &AssTransform, clock: Option<&AssEventClock>) -> TagWeight {
    clock.map_or(TagWeight::FULL, |clock| {
        transform_weight(&transform.timing, clock)
    })
}

/// Применяет `\t` верхнего уровня: его вложенные теги получают вес `k`; вес следующих за `\t`
/// тегов блока остаётся 1 (внешний разбор вес не меняет).
pub(crate) fn apply_transform<S: TagSink>(
    sink: &mut S,
    transform: &AssTransform,
    clock: Option<&AssEventClock>,
) {
    let weight = weight_of(transform, clock);
    apply_nested(sink, &transform.tags, weight, clock);
}

/// Вложенный список тегов: `weight` начальный, вложенный `\t` подменяет его до конца списка.
fn apply_nested<S: TagSink>(
    sink: &mut S,
    tags: &[AssOverride],
    initial_weight: TagWeight,
    clock: Option<&AssEventClock>,
) {
    let mut weight = initial_weight;
    for tag in tags {
        match tag {
            AssOverride::Transform(inner) => {
                weight = weight_of(inner, clock);
                apply_nested(sink, &inner.tags, weight, clock);
            }
            other => sink.apply_tag(other, weight),
        }
    }
}

#[cfg(test)]
mod tests;
