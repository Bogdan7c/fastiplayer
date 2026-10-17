// Порт поведения libass 0.17.5 (ISC): ass_parse.c (ветки `clip`, `iclip`, `blur`, `be`, `pbo`,
// parse_vector_clip).
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

//! Сборка тегов слоя 3 (`\clip`, `\iclip`, `\blur`, `\be`, `\pbo`) из уже разделённых
//! аргументов. Разделение аргументов и выбор тега по префиксу — в `tags.rs`.
//!
//! Здесь ТОЛЬКО разбор значений. Правила состояния (последний прямоугольный клип побеждает,
//! первый векторный побеждает, зажатие `\blur`/`\be`) принадлежат растеризатору: парсер
//! сохраняет каждое вхождение тега по порядку.

use subtitle_core::{AssClip, AssClipMode, AssClipShape, AssOverride};

use super::scan::{parse_f64, parse_i32_clamped};

/// Аргумент `argtoi32`.
fn arg_i32(text: &str) -> i32 {
    parse_i32_clamped(text, 10)
}

/// `\blur`: `argtod` без зажатия (зажатие — после интерполяции `\t`, в растеризаторе).
/// Без аргумента libass жёстко ставит `state->blur = 0.0`, что отражает `None`.
pub(super) fn build_blur(args: &[&str]) -> AssOverride {
    AssOverride::Blur(args.first().map(|text| parse_f64(text)))
}

/// `\be`: `argtod` без округления и зажатия (`dtoi32(v + 0.5)` и `[0, MAX_BE]` делает
/// растеризатор). Без аргумента libass ставит `state->be = 0`, что отражает `None`.
pub(super) fn build_edge_blur(args: &[&str]) -> AssOverride {
    AssOverride::EdgeBlur(args.first().map(|text| parse_f64(text)))
}

/// `\pbo`: `argtod(*args)`; пустой аргумент даёт `0.0`.
pub(super) fn build_drawing_baseline_offset(args: &[&str]) -> AssOverride {
    AssOverride::DrawingBaselineOffset(args.first().map_or(0.0, |text| parse_f64(text)))
}

/// `\clip` / `\iclip`.
///
/// - Ровно 4 аргумента — прямоугольник (`argtoi32` для каждого).
/// - Иначе 1 или 2 аргумента — векторный клип `parse_vector_clip`: при двух аргументах
///   первый — масштаб, текст рисунка — всегда последний аргумент.
/// - Любое другое число аргументов (включая 0 и 3) libass тихо пропускает; здесь это `None`,
///   и вызывающий код записывает тег как некорректный.
pub(super) fn build_clip(mode: AssClipMode, args: &[&str]) -> Option<AssOverride> {
    let shape = match args {
        [x0, y0, x1, y1] => AssClipShape::Rect {
            x0: arg_i32(x0),
            y0: arg_i32(y0),
            x1: arg_i32(x1),
            y1: arg_i32(y1),
        },
        [drawing] => AssClipShape::Vector {
            scale: 1,
            drawing: (*drawing).to_owned(),
        },
        [scale, drawing] => AssClipShape::Vector {
            scale: arg_i32(scale),
            drawing: (*drawing).to_owned(),
        },
        _ => return None,
    };
    Some(AssOverride::Clip(AssClip { mode, shape }))
}
