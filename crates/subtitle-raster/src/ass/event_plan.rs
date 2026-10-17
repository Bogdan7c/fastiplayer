// Порт из libass 0.17.5: ass_render.c (часть ass_render_event: parse_events, выбор отступов,
// max_text_width, device_x/device_y) и ass_parse.c (ass_get_next_char).
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

//! План рисования одной реплики ASS: плоский текст с видами символов и стиль реплики.
//!
//! [`plan_ass_event`] — «разбор реплики» libass (`parse_events`): идёт по элементам текста,
//! применяет теги к [`RenderState`], на каждом символе снимает вид ([`SpanLook`]) и в конце
//! собирает общий стиль реплики ([`EventStyle`]): выравнивание, геометрию блока в пикселях
//! холста, режим переноса, направление текста, подложку. Дальше план идёт в тот же текстовый
//! конвейер, что и SRT (`render_source_text`).
//!
//! Что планировщик НЕ делает: ничего не рисует и не знает про шрифты, кэши и столкновения
//! между репликами.

use subtitle_core::{
    AssAlignment, AssCue, AssHorizontalAlign, AssKerning, AssOverride, AssTextItem,
    AssUnsupportedTag, AssVerticalAlign, AssWrapStyle, CuePlacement, ScreenAnchor,
};

use super::explicit::is_explicit_event;
use super::frame_scale::AssFrameScale;
use super::look::{LookPolicy, UserLook};
use super::options::{AssRenderOptions, AssStyleSource};
use super::render_state::RenderState;
use crate::appearance::{CollisionDirection, PlateStyle, SubtitleAppearance, cue_style};
use crate::event_style::{
    BaseDirection, BlockAnchor, EventPlacement, EventStyle, ExactPlacement, TextMargins, WrapStyle,
};
use crate::overlay::{CanvasSize, OverlayPlacementGroup};
use crate::source_text::{KerningMode, SourceChar, SourceText};
use crate::span_look::SpanLook;

/// Сколько последних видов проверяется при поиске совпадающего (предел стоимости разбора
/// враждебного текста с тысячами блоков тегов).
const LOOK_SEARCH_WINDOW: usize = 32;

/// Как реплика участвует в разрешении столкновений.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CollisionPlan {
    /// Реплика ищет свободное место (libass: `detect_collisions`): выключено у реплик с `\pos`,
    /// `\move`, `\org`, `\t` и у эффектов Banner/Scroll.
    pub(crate) detect: bool,
    /// Куда сдвигать (libass: `shift_direction`): нижние реплики вверх, остальные вниз.
    pub(crate) direction: CollisionDirection,
}

/// План рисования реплики ASS.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AssEventPlan {
    /// Символы реплики с видами.
    pub(crate) source: SourceText,
    /// Общий стиль реплики для текстового конвейера.
    pub(crate) style: EventStyle,
    /// Плашка пользовательского стиля («мой стиль» с `BoxDecoration`); у стиля автора `None`.
    pub(crate) plate: Option<PlateStyle>,
    /// Участие в разрешении столкновений.
    pub(crate) collision: CollisionPlan,
    /// Группа размещения картинок реплики: основной диалог (без `\pos`/`\move`, снизу) или
    /// фиксированная реплика.
    pub(crate) placement_group: OverlayPlacementGroup,
    /// Сколько кусков текста пропущено, потому что режим рисунка (`\p`) включён: векторные
    /// рисунки слой 1 не рисует.
    pub(crate) skipped_drawing_runs: u32,
}

/// Строит план реплики для холста `canvas`.
///
/// `appearance` читается только при [`AssStyleSource::User`].
pub(crate) fn plan_ass_event(
    cue: &AssCue,
    canvas: CanvasSize,
    options: &AssRenderOptions,
    appearance: &SubtitleAppearance,
) -> AssEventPlan {
    let script = &*cue.script;
    let frame = AssFrameScale::new(script.info(), canvas, options.storage_size);
    let explicit = is_explicit_event(cue);
    let (policy, plate) = look_policy(&frame, options, appearance, canvas, explicit);

    // Banner: libass принудительно выключает перенос строк (`wrap_style = 2`).
    let initial_wrap = if cue.effect.starts_with("Banner;") {
        AssWrapStyle::NoWrap
    } else {
        script.info().wrap_style
    };
    let mut state = RenderState::new(script, cue.style(), initial_wrap);

    let mut walker = TextWalker::new(&policy);
    for item in &cue.text {
        walker.walk_item(item, &mut state);
    }
    let final_look = policy.make_look(&state);

    let direction = match state.alignment.vertical {
        AssVerticalAlign::Bottom => CollisionDirection::Up,
        AssVerticalAlign::Middle | AssVerticalAlign::Top => CollisionDirection::Down,
    };
    let collision = CollisionPlan {
        detect: state.position.is_none() && !disables_collisions(cue),
        direction,
    };
    let style = event_style(cue, &state, &frame, &final_look);
    // Позиционная привязка (EVENT_POSITIONED libass) читается из состояния разбора, а не из
    // `hard_overrides`: тот шире (клипы и пр.) и не означает «автор задал место».
    let placement_group =
        if state.position.is_none() && state.alignment.vertical == AssVerticalAlign::Bottom {
            OverlayPlacementGroup::Main
        } else {
            OverlayPlacementGroup::Fixed
        };
    AssEventPlan {
        placement_group,
        source: walker.finish(kerning_mode(script.info().kerning)),
        style,
        plate,
        collision,
        skipped_drawing_runs: walker.skipped_drawing_runs,
    }
}

/// Политика оформления символов и плашка пользовательского стиля.
fn look_policy(
    frame: &AssFrameScale,
    options: &AssRenderOptions,
    appearance: &SubtitleAppearance,
    canvas: CanvasSize,
    explicit: bool,
) -> (LookPolicy, Option<PlateStyle>) {
    let canvas_side = f64::from(canvas.width().max(canvas.height()));
    match options.style_source {
        AssStyleSource::Author => {
            // Пользовательский масштаб текста не трогает «явные» реплики (libass: выборочное
            // переопределение шрифта).
            let text_scale = (!explicit).then(|| f64::from(options.text_scale.get()));
            (
                LookPolicy::author(frame.event_scale(text_scale), canvas_side),
                None,
            )
        }
        AssStyleSource::User => {
            // Оформление берётся из того же перевода `SubtitleAppearance -> пиксели`, что и у
            // SRT; положение стиля игнорируется (оно авторское).
            let user = cue_style(appearance, &CuePlacement::default(), canvas);
            let look = UserLook {
                font_size: user.event.font_size,
                colors: user.event.colors,
                border: user.event.border,
                shadow: user.event.shadow,
            };
            (LookPolicy::user(look), user.plate)
        }
    }
}

/// Теги, после которых libass не ищет место для реплики (`detect_collisions = 0`).
fn disables_collisions(cue: &AssCue) -> bool {
    let effect_scrolls = cue.effect.starts_with("Banner;")
        || cue.effect.starts_with("Scroll up;")
        || cue.effect.starts_with("Scroll down;");
    effect_scrolls
        || cue.text.iter().any(|item| match item {
            AssTextItem::Overrides(overrides) => overrides.iter().any(|tag| {
                matches!(
                    tag,
                    AssOverride::Unsupported(AssUnsupportedTag::Org | AssUnsupportedTag::Transform)
                )
            }),
            _ => false,
        })
}

/// Общий стиль реплики по состоянию на конец разбора.
fn event_style(
    cue: &AssCue,
    state: &RenderState<'_>,
    frame: &AssFrameScale,
    final_look: &SpanLook,
) -> EventStyle {
    // Отступы: ненулевое поле события сильнее стиля; стиль — последний, к которому сбросили
    // состояние (`state->style`).
    let margins = cue.margins.resolve_with(state.style());
    let text_left = frame.x_to_screen(f64::from(margins.left));
    let text_width = frame.x_to_screen(frame.play_res_x() - f64::from(margins.right)) - text_left;
    let anchor = match state.position {
        Some(point) => BlockAnchor::AtPoint {
            x: frame.x_to_screen(point.x),
            y: frame.y_to_screen(point.y),
        },
        None => BlockAnchor::ByAlignment,
    };
    EventStyle {
        font_family: final_look.font_family.clone(),
        font_size: final_look.font_size,
        font_weight: final_look.weight,
        font_slant: final_look.slant,
        border_style: final_look.border_style,
        border: final_look.border,
        shadow: final_look.shadow,
        colors: final_look.colors,
        alignment: screen_anchor(state.alignment),
        margins: TextMargins {
            left: 0,
            right: 0,
            vertical: 0,
        },
        placement: EventPlacement::Exact(ExactPlacement {
            text_left,
            text_width,
            vertical_margin: frame.y_to_screen(f64::from(margins.vertical)),
            anchor,
            pixel_aspect_x: frame.pixel_aspect_x(),
        }),
        wrap_style: wrap_style(state.wrap_style),
        // `ass_set_line_spacing` не вызывается: межстрочного интервала сверх шрифта нет.
        line_spacing: 0.0,
        // libass: `Encoding = -1` — автоопределение направления, иначе слева направо.
        base_direction: if state.encoding == -1 {
            BaseDirection::Auto
        } else {
            BaseDirection::LeftToRight
        },
    }
}

/// Выравнивание ASS -> якорь конвейера.
fn screen_anchor(alignment: AssAlignment) -> ScreenAnchor {
    use AssHorizontalAlign::{Center, Left, Right};
    use AssVerticalAlign::{Bottom, Middle, Top};
    match (alignment.vertical, alignment.horizontal) {
        (Top, Left) => ScreenAnchor::TopLeft,
        (Top, Center) => ScreenAnchor::TopCenter,
        (Top, Right) => ScreenAnchor::TopRight,
        (Middle, Left) => ScreenAnchor::MiddleLeft,
        (Middle, Center) => ScreenAnchor::MiddleCenter,
        (Middle, Right) => ScreenAnchor::MiddleRight,
        (Bottom, Left) => ScreenAnchor::BottomLeft,
        (Bottom, Center) => ScreenAnchor::BottomCenter,
        (Bottom, Right) => ScreenAnchor::BottomRight,
    }
}

/// `Kerning:` заголовка сценария -> режим шейпинга.
fn kerning_mode(kerning: AssKerning) -> KerningMode {
    match kerning {
        AssKerning::Disabled => KerningMode::Disabled,
        AssKerning::Enabled => KerningMode::Enabled,
    }
}

/// Режим переноса: libass различает только `1` и `2`, а `3` ведёт себя как `0`.
fn wrap_style(wrap: AssWrapStyle) -> WrapStyle {
    match wrap {
        AssWrapStyle::Smart | AssWrapStyle::SmartWideLower => WrapStyle::Smart,
        AssWrapStyle::EndOfLine => WrapStyle::EndOfLine,
        AssWrapStyle::NoWrap => WrapStyle::NoWrap,
    }
}

/// Проход по тексту реплики: накапливает символы и их виды.
struct TextWalker<'policy> {
    policy: &'policy LookPolicy,
    chars: Vec<SourceChar>,
    looks: Vec<SpanLook>,
    /// Индекс вида текущего состояния; `None` — состояние менялось после последнего символа.
    current_look: Option<usize>,
    /// Вид предыдущего символа (для склейки равных видов в один участок).
    previous_look: Option<usize>,
    skipped_drawing_runs: u32,
}

impl<'policy> TextWalker<'policy> {
    fn new(policy: &'policy LookPolicy) -> Self {
        Self {
            policy,
            chars: Vec::new(),
            looks: Vec::new(),
            current_look: None,
            previous_look: None,
            skipped_drawing_runs: 0,
        }
    }

    /// Обрабатывает один элемент текста (libass: `ass_get_next_char` и разбор `{...}`).
    fn walk_item(&mut self, item: &AssTextItem, state: &mut RenderState<'_>) {
        match item {
            AssTextItem::Overrides(overrides) => {
                for tag in overrides {
                    state.apply(tag);
                }
                self.current_look = None;
            }
            AssTextItem::Text(content) => {
                // Текст в режиме рисунка — это команды рисования, а не буквы.
                if state.drawing.is_on() {
                    self.skipped_drawing_runs = self.skipped_drawing_runs.saturating_add(1);
                    return;
                }
                // Нулевой размер шрифта: глифов нет (как в libass), остальной текст рисуется.
                if !self.policy.draws_glyphs(state) {
                    return;
                }
                let look = self.look_index(state);
                for ch in content.chars() {
                    // Возврат каретки и NUL разбор не должен пропускать, но на всякий случай
                    // они не попадают в раскладку (как в SRT-пути).
                    if ch != '\r' && ch != '\0' {
                        self.chars.push(SourceChar { ch, look });
                    }
                }
            }
            AssTextItem::HardBreak => self.push_break(state, '\n'),
            // `\n`: перевод строки только при режиме «без переноса» НА ЭТОТ МОМЕНТ, иначе пробел
            // (libass решает при чтении символа, с `wrap_style` состояния).
            AssTextItem::SoftBreak => {
                let ch = if state.wrap_style == AssWrapStyle::NoWrap {
                    '\n'
                } else {
                    ' '
                };
                self.push_break(state, ch);
            }
        }
    }

    fn push_break(&mut self, state: &RenderState<'_>, ch: char) {
        // Перенос при нулевом размере шрифта пропускается вместе с буквами (упрощение слоя 1).
        if state.drawing.is_on() || !self.policy.draws_glyphs(state) {
            return;
        }
        let look = self.look_index(state);
        self.chars.push(SourceChar { ch, look });
    }

    /// Индекс вида для текущего состояния: прежний, если ничего не менялось или вид равен
    /// виду предыдущего символа; иначе найденный среди последних или новый.
    fn look_index(&mut self, state: &RenderState<'_>) -> usize {
        if let Some(index) = self.current_look {
            return index;
        }
        let look = self.policy.make_look(state);
        let same_as_previous = self
            .previous_look
            .filter(|index| self.looks.get(*index) == Some(&look));
        let index = same_as_previous
            .or_else(|| {
                let from = self.looks.len().saturating_sub(LOOK_SEARCH_WINDOW);
                self.looks
                    .iter()
                    .enumerate()
                    .skip(from)
                    .find(|(_, known)| **known == look)
                    .map(|(index, _)| index)
            })
            .unwrap_or_else(|| {
                self.looks.push(look);
                self.looks.len() - 1
            });
        self.current_look = Some(index);
        self.previous_look = Some(index);
        index
    }

    /// Готовый плоский текст; `kerning` — настройка `Kerning:` заголовка сценария.
    fn finish(&mut self, kerning: KerningMode) -> SourceText {
        SourceText {
            chars: std::mem::take(&mut self.chars),
            looks: std::mem::take(&mut self.looks),
            kerning,
        }
    }
}

#[cfg(test)]
mod tests;
