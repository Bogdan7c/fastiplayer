// Порт поведения libass 0.17.5 (ISC): ass_parse.c (ass_parse_tags, parse_color_tag, parse_alpha_tag, push_arg).
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

//! Разбор блока тегов `{\tag...}` (порт `ass_parse_tags` из libass, без состояния рендера).
//!
//! libass разбирает теги и сразу применяет их к состоянию рендера. Здесь разбор отделён от
//! применения: результат — список [`AssOverride`] с уже вычисленной семантикой аргументов
//! (допустимые значения, зажатие в диапазон, «нет аргумента = взять стиль»), а состояние
//! (например, «первый `\an` побеждает») применяет растеризатор.
//!
//! Что повторено дословно:
//! - порядок проверки имён тегов: побеждает ПЕРВОЕ имя из таблицы [`TAG_RULES`], являющееся
//!   префиксом написанного (`\fscx` раньше `\fs`, `\bord` раньше `\b`, `\an` раньше `\a`,
//!   `\shad` раньше `\s`, `\clip` раньше `\c` ...); регистр важен;
//! - разбор круглых скобок и запятых (до 8 аргументов, пробелы по краям срезаются, обратная
//!   косая черта «склеивает» остаток скобок в один аргумент);
//! - «хвост» имени без скобок (`\fs20`) становится ещё одним аргументом ПОСЛЕ аргументов в
//!   скобках, а у «сложных» тегов (`\pos`, `\move`, `\t`, `\clip` ...) такого хвоста нет.

use subtitle_core::{
    AssAlignment, AssAlphaTarget, AssAxes, AssBold, AssClipMode, AssColorSlot, AssDrawingMode,
    AssFade, AssFontSize, AssMove, AssOverride, AssRgb, AssRotationAxis, AssShearAxis,
    AssUnsupportedTag, AssWrapStyle,
};

use super::filter_tags::{build_blur, build_clip, build_drawing_baseline_offset, build_edge_blur};
use super::report::AssTagStats;
use super::scan::{is_ass_space, parse_f64, parse_i32_clamped, rskip_spaces};

/// Максимум аргументов, которые libass сохраняет (`MAX_VALID_NARGS + 1`).
const MAX_STORED_ARGS: usize = 8;
/// Пока сохранено не больше этого числа аргументов, следующий ещё принимается.
const MAX_VALID_ARGS: usize = 7;

/// Бюджет тегов на одну реплику (общий для всех её блоков `{...}`).
#[derive(Debug)]
pub(super) struct TagBudget {
    remaining: usize,
}

impl TagBudget {
    pub(super) fn new(max_tags_per_event: usize) -> Self {
        Self {
            remaining: max_tags_per_event,
        }
    }

    /// Расходует единицу бюджета; `false` — бюджет исчерпан.
    fn try_take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

/// Какой тег распознан (поддерживаемый на слое 1 или известный, но не исполняемый).
#[derive(Debug, Clone, Copy)]
enum Tag {
    Border(AssAxes),
    Shadow(AssAxes),
    ScaleX,
    ScaleY,
    ResetScale,
    Spacing,
    FontSize,
    FontName,
    Alpha(AssAlphaTarget),
    AlignNumpad,
    AlignLegacy,
    Position,
    /// `\move`: позиция реплики; анимация между точками — слой 2.
    Move,
    Color(AssColorSlot),
    Reset,
    Bold,
    Italic,
    Underline,
    StrikeOut,
    WrapStyle,
    FontEncoding,
    /// `\p<n>`: режим рисунка (аргумент нужен рендеру, поэтому не `Unsupported`).
    Drawing,
    /// `\frx`, `\fry`, `\frz`, `\fr`.
    Rotation(AssRotationAxis),
    /// `\fax`, `\fay`.
    Shear(AssShearAxis),
    /// `\org(x,y)`.
    Origin,
    /// `\fad(a,b)` и `\fade(...)`.
    Fade,
    /// `\blur`: гауссово размытие (разбор в `filter_tags.rs`).
    Blur,
    /// `\be`: box-размытие края.
    EdgeBlur,
    /// `\clip` / `\iclip`.
    Clip(AssClipMode),
    /// `\pbo`: смещение базовой линии рисунка.
    DrawingBaselineOffset,
    Unsupported(AssUnsupportedTag),
}

/// Берёт ли тег «хвост» имени как дополнительный аргумент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrailingArg {
    /// Обычный тег (`tag(...)` в libass): `\fs20` -> аргумент `20`.
    Accepted,
    /// «Сложный» тег (`complex_tag(...)`): аргументы только в скобках.
    Ignored,
}

#[derive(Debug, Clone, Copy)]
struct TagRule {
    name: &'static str,
    tag: Tag,
    trailing: TrailingArg,
}

const fn plain(name: &'static str, tag: Tag) -> TagRule {
    TagRule {
        name,
        tag,
        trailing: TrailingArg::Accepted,
    }
}

const fn complex(name: &'static str, tag: Tag) -> TagRule {
    TagRule {
        name,
        tag,
        trailing: TrailingArg::Ignored,
    }
}

/// Таблица тегов В ТОЧНОМ порядке цепочки `else if` из `ass_parse_tags` (libass 0.17.5).
/// Менять порядок нельзя: от него зависит, какой тег «съест» общий префикс.
const TAG_RULES: &[TagRule] = &[
    plain("xbord", Tag::Border(AssAxes::X)),
    plain("ybord", Tag::Border(AssAxes::Y)),
    plain("xshad", Tag::Shadow(AssAxes::X)),
    plain("yshad", Tag::Shadow(AssAxes::Y)),
    plain("fax", Tag::Shear(AssShearAxis::X)),
    plain("fay", Tag::Shear(AssShearAxis::Y)),
    complex("iclip", Tag::Clip(AssClipMode::Outside)),
    plain("blur", Tag::Blur),
    plain("fscx", Tag::ScaleX),
    plain("fscy", Tag::ScaleY),
    plain("fsc", Tag::ResetScale),
    plain("fsp", Tag::Spacing),
    plain("fs", Tag::FontSize),
    plain("bord", Tag::Border(AssAxes::Both)),
    complex("move", Tag::Move),
    plain("frx", Tag::Rotation(AssRotationAxis::X)),
    plain("fry", Tag::Rotation(AssRotationAxis::Y)),
    plain("frz", Tag::Rotation(AssRotationAxis::Z)),
    plain("fr", Tag::Rotation(AssRotationAxis::Z)),
    plain("fn", Tag::FontName),
    plain("alpha", Tag::Alpha(AssAlphaTarget::All)),
    plain("an", Tag::AlignNumpad),
    plain("a", Tag::AlignLegacy),
    complex("pos", Tag::Position),
    complex("fade", Tag::Fade),
    complex("fad", Tag::Fade),
    complex("org", Tag::Origin),
    complex("t", Tag::Unsupported(AssUnsupportedTag::Transform)),
    complex("clip", Tag::Clip(AssClipMode::Inside)),
    plain("c", Tag::Color(AssColorSlot::Primary)),
    plain("1c", Tag::Color(AssColorSlot::Primary)),
    plain("2c", Tag::Color(AssColorSlot::Secondary)),
    plain("3c", Tag::Color(AssColorSlot::Outline)),
    plain("4c", Tag::Color(AssColorSlot::Back)),
    plain(
        "1a",
        Tag::Alpha(AssAlphaTarget::Slot(AssColorSlot::Primary)),
    ),
    plain(
        "2a",
        Tag::Alpha(AssAlphaTarget::Slot(AssColorSlot::Secondary)),
    ),
    plain(
        "3a",
        Tag::Alpha(AssAlphaTarget::Slot(AssColorSlot::Outline)),
    ),
    plain("4a", Tag::Alpha(AssAlphaTarget::Slot(AssColorSlot::Back))),
    plain("r", Tag::Reset),
    plain("be", Tag::EdgeBlur),
    plain("b", Tag::Bold),
    plain("i", Tag::Italic),
    plain("kt", Tag::Unsupported(AssUnsupportedTag::Karaoke)),
    plain("kf", Tag::Unsupported(AssUnsupportedTag::Karaoke)),
    plain("K", Tag::Unsupported(AssUnsupportedTag::Karaoke)),
    plain("ko", Tag::Unsupported(AssUnsupportedTag::Karaoke)),
    plain("k", Tag::Unsupported(AssUnsupportedTag::Karaoke)),
    plain("shad", Tag::Shadow(AssAxes::Both)),
    plain("s", Tag::StrikeOut),
    plain("u", Tag::Underline),
    plain("pbo", Tag::DrawingBaselineOffset),
    plain("p", Tag::Drawing),
    plain("q", Tag::WrapStyle),
    plain("fe", Tag::FontEncoding),
];

/// Аргументы тега: до [`MAX_STORED_ARGS`] срезов; недостающие — пустые (как в libass).
#[derive(Debug, Default)]
struct TagArgs<'a> {
    items: [&'a str; MAX_STORED_ARGS],
    count: usize,
}

impl<'a> TagArgs<'a> {
    /// `push_arg`: пробелы справа срезаются, пустой аргумент не сохраняется.
    fn push(&mut self, text: &'a str) {
        if self.count > MAX_VALID_ARGS {
            return;
        }
        let trimmed = rskip_spaces(text);
        if trimmed.is_empty() {
            return;
        }
        if let Some(slot) = self.items.get_mut(self.count) {
            *slot = trimmed;
            self.count += 1;
        }
    }

    /// Сохранённые аргументы (без пустых «слотов»).
    fn provided(&self) -> &[&'a str] {
        self.items.get(..self.count).unwrap_or_default()
    }

    /// Первый аргумент (пустая строка, если аргументов нет — как `args[0]` в libass).
    fn first(&self) -> &'a str {
        self.items.first().copied().unwrap_or_default()
    }
}

/// Разбирает содержимое одного блока `{...}` (без самих фигурных скобок).
///
/// Каждый тег расходует бюджет реплики; когда он исчерпан, остальные теги блока
/// отбрасываются и считаются. Неизвестные теги и теги слоёв 2–3 считаются в `stats`.
pub(super) fn parse_override_block(
    block: &str,
    budget: &mut TagBudget,
    stats: &mut AssTagStats,
) -> Vec<AssOverride> {
    let cursor = BlockCursor::new(block);
    let mut overrides = Vec::new();
    let mut pos = 0usize;
    while pos < cursor.end {
        // Ищем начало следующего тега.
        while pos != cursor.end && cursor.at(pos) != b'\\' {
            pos += 1;
        }
        if pos == cursor.end {
            break;
        }
        pos += 1;
        if pos != cursor.end {
            pos = cursor.skip_spaces(pos);
        }
        let name_start = pos;
        let name_end = cursor.find_name_end(name_start);
        if name_end == name_start {
            // Пустое имя (например, `\\` или `\(`): идём дальше с этого места.
            pos = name_end;
            continue;
        }
        let mut args = TagArgs::default();
        let after_args = cursor.split_parenthesized_args(name_end, &mut args);
        let name = cursor.slice(name_start, name_end);
        if let Some(rule) = find_rule(name) {
            if rule.trailing == TrailingArg::Accepted {
                args.push(cursor.slice(name_start + rule.name.len(), name_end));
            }
            if budget.try_take() {
                match build_override(rule.tag, &args) {
                    Some(parsed) => {
                        if let AssOverride::Unsupported(tag) = &parsed {
                            stats.record_unsupported(*tag);
                        }
                        overrides.push(parsed);
                    }
                    None => stats.record_malformed(),
                }
            } else {
                stats.record_over_limit();
            }
        } else if budget.try_take() {
            stats.record_unknown(name);
        } else {
            stats.record_over_limit();
        }
        pos = after_args;
    }
    overrides
}

/// Первое правило таблицы, чьё имя — префикс написанного имени.
fn find_rule(written_name: &str) -> Option<&'static TagRule> {
    TAG_RULES
        .iter()
        .find(|rule| written_name.starts_with(rule.name))
}

/// Байтовый курсор по блоку: чтение за концом даёт 0, как закрывающая `}` у libass.
struct BlockCursor<'a> {
    text: &'a str,
    end: usize,
}

impl<'a> BlockCursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            end: text.len(),
        }
    }

    fn at(&self, index: usize) -> u8 {
        self.text.as_bytes().get(index).copied().unwrap_or(0)
    }

    /// Срез по позициям, лежащим на границах ASCII-символов.
    fn slice(&self, start: usize, end: usize) -> &'a str {
        self.text.get(start..end).unwrap_or_default()
    }

    fn skip_spaces(&self, mut pos: usize) -> usize {
        while is_ass_space(self.at(pos)) {
            pos += 1;
        }
        pos
    }

    /// Конец имени тега: `(`, `\` или конец блока.
    fn find_name_end(&self, start: usize) -> usize {
        let mut pos = start;
        while pos != self.end && self.at(pos) != b'(' && self.at(pos) != b'\\' {
            pos += 1;
        }
        pos
    }

    /// Разбирает `(a, b, c)` после имени тега, если оно есть. Возвращает позицию, с которой
    /// продолжается поиск следующего тега.
    ///
    /// Запятые делят аргументы; встретив `\`, остаток до `)` (или до конца блока)
    /// сливается в один аргумент — так libass хранит вложенные теги `\t(...)`.
    fn split_parenthesized_args(&self, name_end: usize, args: &mut TagArgs<'a>) -> usize {
        if self.at(name_end) != b'(' {
            return name_end;
        }
        let mut pos = name_end + 1;
        loop {
            if pos != self.end {
                pos = self.skip_spaces(pos);
            }
            let mut arg_end = pos;
            while arg_end != self.end && !matches!(self.at(arg_end), b',' | b'\\' | b')') {
                arg_end += 1;
            }
            if self.at(arg_end) == b',' {
                args.push(self.slice(pos, arg_end));
                pos = arg_end + 1;
                continue;
            }
            if self.at(arg_end) == b'\\' {
                arg_end = self.find_closing_paren(arg_end);
            }
            args.push(self.slice(pos, arg_end));
            // Закрывающей скобки может не быть.
            return if arg_end != self.end {
                arg_end + 1
            } else {
                arg_end
            };
        }
    }

    /// Позиция первой `)` начиная с `from`, либо конец блока.
    fn find_closing_paren(&self, from: usize) -> usize {
        (from..self.end)
            .find(|index| self.at(*index) == b')')
            .unwrap_or(self.end)
    }
}

/// Целый аргумент как `argtoi32`.
fn arg_i32(text: &str) -> i32 {
    parse_i32_clamped(text, 10)
}

/// Значение 0/1 для `\i`, `\u`, `\s`: `None` — вне допустимого (взять стиль).
fn arg_flag(args: &TagArgs<'_>) -> Option<bool> {
    if args.count == 0 {
        return None;
    }
    match arg_i32(args.first()) {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Строит тег из аргументов. `None` — неверное число аргументов (тег игнорируется).
fn build_override(tag: Tag, args: &TagArgs<'_>) -> Option<AssOverride> {
    let has_arg = args.count > 0;
    let number = || parse_f64(args.first());
    Some(match tag {
        Tag::Border(axes) => AssOverride::Border {
            axes,
            // \bord, \xbord, \ybord зажимаются снизу нулём.
            value: has_arg.then(|| number().max(0.0)),
        },
        Tag::Shadow(axes) => AssOverride::Shadow {
            axes,
            // VSFilter: зажимается только \shad, но не \xshad и \yshad.
            value: has_arg.then(|| match axes {
                AssAxes::Both => number().max(0.0),
                AssAxes::X | AssAxes::Y => number(),
            }),
        },
        Tag::ScaleX => AssOverride::ScaleX(has_arg.then(|| (number() / 100.0).max(0.0))),
        Tag::ScaleY => AssOverride::ScaleY(has_arg.then(|| (number() / 100.0).max(0.0))),
        Tag::ResetScale => AssOverride::ResetScale,
        Tag::Spacing => AssOverride::Spacing(has_arg.then(number)),
        Tag::FontSize => AssOverride::FontSize(if has_arg {
            let value = number();
            if args.first().starts_with(['+', '-']) {
                AssFontSize::Relative(value)
            } else {
                AssFontSize::Absolute(value)
            }
        } else {
            AssFontSize::Reset
        }),
        Tag::FontName => AssOverride::FontName(font_name(args)),
        Tag::Alpha(target) => AssOverride::Alpha {
            target,
            value: has_arg.then(|| parse_alpha(args.first())),
        },
        Tag::AlignNumpad => {
            AssOverride::Alignment(AssAlignment::from_numpad_tag(arg_i32(args.first())))
        }
        Tag::AlignLegacy => {
            AssOverride::Alignment(AssAlignment::from_legacy_tag(arg_i32(args.first())))
        }
        Tag::Position => {
            if args.count != 2 {
                return None;
            }
            let second = args.items.get(1).copied().unwrap_or_default();
            AssOverride::Position {
                x: number(),
                y: parse_f64(second),
            }
        }
        Tag::Move => return build_move(args).map(AssOverride::Move),
        Tag::Rotation(axis) => AssOverride::Rotation {
            axis,
            degrees: has_arg.then(number),
        },
        Tag::Shear(axis) => AssOverride::Shear {
            axis,
            factor: has_arg.then(number),
        },
        Tag::Origin => {
            if args.count != 2 {
                return None;
            }
            let second = args.items.get(1).copied().unwrap_or_default();
            AssOverride::Origin {
                x: number(),
                y: parse_f64(second),
            }
        }
        Tag::Fade => return build_fade(args).map(AssOverride::Fade),
        Tag::Color(slot) => AssOverride::Color {
            slot,
            value: has_arg.then(|| parse_color(args.first())),
        },
        Tag::Reset => AssOverride::Reset(has_arg.then(|| args.first().to_owned())),
        Tag::Bold => AssOverride::Bold(bold_value(args)),
        Tag::Italic => AssOverride::Italic(arg_flag(args)),
        Tag::Underline => AssOverride::Underline(arg_flag(args)),
        Tag::StrikeOut => AssOverride::StrikeOut(arg_flag(args)),
        Tag::WrapStyle => AssOverride::WrapStyle(if has_arg {
            AssWrapStyle::from_tag_value(arg_i32(args.first()))
        } else {
            None
        }),
        Tag::FontEncoding => AssOverride::FontEncoding(has_arg.then(|| arg_i32(args.first()))),
        // libass: `val = max(0, argtoi32(args[0]))`; без аргумента args[0] пуст, то есть 0.
        Tag::Drawing => AssOverride::Drawing(AssDrawingMode::from_tag_value(arg_i32(args.first()))),
        Tag::Blur => build_blur(args.provided()),
        Tag::EdgeBlur => build_edge_blur(args.provided()),
        Tag::Clip(mode) => return build_clip(mode, args.provided()),
        Tag::DrawingBaselineOffset => build_drawing_baseline_offset(args.provided()),
        Tag::Unsupported(kind) => AssOverride::Unsupported(kind),
    })
}

/// `\move`: 4 или 6 аргументов (иначе тег игнорируется, как `continue` в libass). При шести
/// границы времени упорядочиваются; «обе не положительные» значит «на всю реплику».
fn build_move(args: &TagArgs<'_>) -> Option<AssMove> {
    if args.count != 4 && args.count != 6 {
        return None;
    }
    let number_at = |index: usize| parse_f64(args.items.get(index).copied().unwrap_or_default());
    let time_at = |index: usize| arg_i32(args.items.get(index).copied().unwrap_or_default());
    let (mut start_ms, mut end_ms) = (0, 0);
    if args.count == 6 {
        (start_ms, end_ms) = (time_at(4), time_at(5));
        if start_ms > end_ms {
            std::mem::swap(&mut start_ms, &mut end_ms);
        }
    }
    if start_ms <= 0 && end_ms <= 0 {
        (start_ms, end_ms) = (0, 0);
    }
    Some(AssMove {
        from_x: number_at(0),
        from_y: number_at(1),
        to_x: number_at(2),
        to_y: number_at(3),
        start_ms,
        end_ms,
    })
}

/// `\fad`/`\fade`: два аргумента (`\fad`) или семь (`\fade`); иначе тег игнорируется, как
/// `continue` в libass. Пересчёт времён через длительность реплики делает растеризатор.
fn build_fade(args: &TagArgs<'_>) -> Option<AssFade> {
    let int_at = |index: usize| arg_i32(args.items.get(index).copied().unwrap_or_default());
    match args.count {
        2 => Some(AssFade::Simple {
            fade_in_ms: int_at(0),
            fade_out_ms: int_at(1),
        }),
        7 => Some(AssFade::Full {
            alphas: [int_at(0), int_at(1), int_at(2)],
            times_ms: [int_at(3), int_at(4), int_at(5), int_at(6)],
        }),
        _ => None,
    }
}

/// `\fn`: без аргумента и при аргументе ровно `0` — шрифт стиля; иначе имя без ведущих
/// пробелов.
fn font_name(args: &TagArgs<'_>) -> Option<String> {
    if args.count == 0 || args.first() == "0" {
        return None;
    }
    Some(args.first().trim_start_matches([' ', '\t']).to_owned())
}

/// `\b`: допустимы `0`, `1` и `>= 100`; всё остальное — шрифт стиля.
fn bold_value(args: &TagArgs<'_>) -> Option<AssBold> {
    if args.count == 0 {
        return None;
    }
    match arg_i32(args.first()) {
        value @ (0 | 1) => Some(AssBold::from_libass_value(value)),
        value if value >= 100 => Some(AssBold::from_libass_value(value)),
        _ => None,
    }
}

/// `parse_alpha_tag`: пропустить `&` и `H`, прочитать hex, взять младший байт
/// (`change_alpha` приводит результат к `uint8_t`).
fn parse_alpha(text: &str) -> u8 {
    let value = parse_i32_clamped(text.trim_start_matches(['&', 'H']), 16);
    (value & 0xff) as u8
}

/// `parse_color_tag`: пропустить `&` и `H`, прочитать hex; число — `&HBBGGRR`.
/// Мусор даёт 0, то есть чёрный (так делает libass).
fn parse_color(text: &str) -> AssRgb {
    let value = parse_i32_clamped(text.trim_start_matches(['&', 'H']), 16);
    AssRgb::from_bgr_value(value as u32)
}
