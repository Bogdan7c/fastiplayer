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
    AssAlignment, AssCue, AssDrawingMode, AssEffect, AssHorizontalAlign, AssKerning, AssOverride,
    AssTextItem, AssVerticalAlign, AssWrapStyle, CuePlacement, ScreenAnchor,
};

use super::animation::AssAnimationState;
use super::clip_animation::ClipAnimation;
use super::clip_plan::EventClipPlan;
use super::drawing_scale::outline_scale;
use super::explicit::is_explicit_event;
use super::frame_scale::AssFrameScale;
use super::look::{LookPolicy, UserLook};
use super::options::{AssRenderOptions, AssStyleSource};
use super::render_state::{RenderState, ScriptPoint};
use super::scroll::{scroll_band, scroll_placement};
use super::transform_scan::animates_orientation;
use crate::appearance::{CollisionDirection, PlateStyle, SubtitleAppearance, cue_style};
use crate::event_style::{
    BaseDirection, BlockAnchor, EventPlacement, EventStyle, ExactPlacement, RotationOrigin,
    ScrollPlacement, TextMargins, WrapStyle,
};
use crate::overlay::{CanvasSize, OverlayPlacementGroup};
use crate::source_text::{
    DRAWING_SYMBOL, DrawingId, DrawingSource, KerningMode, SourceChar, SourceText,
};
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
    /// Группа размещения картинок реплики: основной диалог (без `\pos`/`\move`/`\org`,
    /// поворотов и наклона, снизу) или фиксированная реплика.
    pub(crate) placement_group: OverlayPlacementGroup,
    /// Клипы `\clip` / `\iclip` в экранных величинах. Не зависят от времени и не входят в
    /// ключ кэша: накладываются стадией [`crate::clip`] после кэша реплик.
    pub(crate) clip: EventClipPlan,
}

/// Строит план реплики для холста `canvas`.
///
/// `appearance` читается только при [`AssStyleSource::User`]. `animation` — состояние
/// `\move` и `\fad` на момент показа (см. [`super::evaluate_animation`]): движущаяся реплика
/// ставится в текущую точку, затухание уходит в стиль реплики и плашку.
pub(crate) fn plan_ass_event(
    cue: &AssCue,
    canvas: CanvasSize,
    options: &AssRenderOptions,
    appearance: &SubtitleAppearance,
    animation: &AssAnimationState,
) -> AssEventPlan {
    let script = &*cue.script;
    let frame = AssFrameScale::new(script.info(), canvas, options.storage_size);
    let explicit = is_explicit_event(cue);
    let (policy, plate) = look_policy(&frame, options, appearance, canvas, explicit);

    // Banner: libass принудительно выключает перенос строк (`wrap_style = 2`).
    let effect = cue.transition_effect();
    let initial_wrap = if matches!(effect, AssEffect::Banner { .. }) {
        AssWrapStyle::NoWrap
    } else {
        script.info().wrap_style
    };
    let mut state = RenderState::new(script, cue.style(), initial_wrap)
        .with_event_fade(animation.fade)
        .with_event_clock(animation.clock);

    let expected_chars = cue.text.iter().fold(0_usize, |total, item| match item {
        AssTextItem::Text(text) => total.saturating_add(text.len()),
        AssTextItem::HardBreak | AssTextItem::SoftBreak => total.saturating_add(1),
        AssTextItem::Overrides(_) => total,
    });
    let mut walker = TextWalker::new(&policy, expected_chars);
    for item in &cue.text {
        walker.walk_item(item, &mut state);
    }
    let final_look = policy.make_look(&state);
    // Плашка пользовательского стиля и фон `BorderStyle = 4` гаснут затуханием, действующим
    // в конце разбора (libass: `state->fade` при рисовании фона), а не затуханием символа.
    let plate = plate.map(|plate| PlateStyle {
        color: state.fade.apply_to(plate.color),
        ..plate
    });

    let direction = match state.alignment.vertical {
        AssVerticalAlign::Bottom => CollisionDirection::Up,
        AssVerticalAlign::Middle | AssVerticalAlign::Top => CollisionDirection::Down,
    };
    let collision = CollisionPlan {
        // `\org` (как `\pos`) отключает поиск свободного места: автор сам выбрал геометрию.
        detect: state.position.is_none() && state.origin.is_none() && !disables_collisions(cue),
        direction,
    };
    // `\move`: `RenderState` знает только начальную точку (первый позиционирующий тег), а
    // текущую точку даёт анимация; для `\pos` и неподвижного `\move` она `None`.
    let position = animation.position.or(state.position);
    let scroll = scroll_placement(effect, animation.scroll_shift.unwrap_or(0.0), &frame);
    let style = event_style(cue, &state, position, scroll, &frame, &final_look);
    // Позиционная привязка (EVENT_POSITIONED libass) читается из состояния разбора, а не из
    // `hard_overrides`: тот шире (клипы и пр.) и не означает «автор задал место».
    // Повёрнутые и наклонённые реплики, как и реплики с центром `\org`, тоже «надписи»: они не
    // диалог, поэтому не поднимаются над панелью управления вместе с основными репликами.
    // `\t`, анимирующий поворот или наклон, делает реплику «надписью» навсегда: иначе группа
    // менялась бы со временем (в нулевой момент поворота ещё нет).
    let is_dialogue_line = state.position.is_none()
        && state.origin.is_none()
        && !walker.has_transformed_text()
        && !animates_orientation(cue)
        // Прокрутка сама выбирает место на кадре: такая реплика не диалог.
        && effect == AssEffect::None
        && state.alignment.vertical == AssVerticalAlign::Bottom;
    let placement_group = if is_dialogue_line {
        OverlayPlacementGroup::Main
    } else {
        OverlayPlacementGroup::Fixed
    };
    // Масштаб кадра для клипа: как у libass, с пользовательским масштабом текста только у
    // не явных реплик (у реплики с клипом он не применяется).
    let clip_scale = frame.event_scale((!explicit).then(|| f64::from(options.text_scale.get())));
    let mut clip =
        EventClipPlan::from_filters(&state.filters, &frame, &clip_scale, canvas, explicit);
    clip.animation = ClipAnimation::for_cue(cue, &frame, canvas, explicit);
    // `Scroll up/down`: текст виден только в полосе прокрутки (постоянна во времени).
    if let Some(band) = scroll_band(effect, &frame) {
        clip.limit_to_scroll_band(band, canvas);
    }
    AssEventPlan {
        placement_group,
        clip,
        source: walker.finish(kerning_mode(script.info().kerning)),
        style,
        plate,
        collision,
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
                // Камеру задаёт масштаб кадра без пользовательского коэффициента текста
                // (`init_font_scale` в режиме без `font_size_coeff`).
                camera_scale: frame.event_scale(None).blur_y,
            };
            (LookPolicy::user(look), user.plate)
        }
    }
}

/// Теги, после которых libass не ищет место для реплики (`detect_collisions = 0`).
fn disables_collisions(cue: &AssCue) -> bool {
    // Эффект без достаточного числа параметров libass не применяет (`AssEffect::None`) и
    // коллизии не отключает.
    let effect_scrolls = cue.transition_effect() != AssEffect::None;
    effect_scrolls
        || cue.text.iter().any(|item| match item {
            // Любой `\t`, даже некорректный: `detect_collisions = 0` стоит до проверки формы.
            AssTextItem::Overrides(overrides) => overrides
                .iter()
                .any(|tag| matches!(tag, AssOverride::Transform(_))),
            _ => false,
        })
}

/// Общий стиль реплики по состоянию на конец разбора.
fn event_style(
    cue: &AssCue,
    state: &RenderState<'_>,
    position: Option<ScriptPoint>,
    scroll: ScrollPlacement,
    frame: &AssFrameScale,
    final_look: &SpanLook,
) -> EventStyle {
    // Отступы: ненулевое поле события сильнее стиля; стиль — последний, к которому сбросили
    // состояние (`state->style`).
    let margins = cue.margins.resolve_with(state.style());
    let text_left = frame.x_to_screen(f64::from(margins.left));
    let text_width = frame.x_to_screen(frame.play_res_x() - f64::from(margins.right)) - text_left;
    let anchor = match position {
        Some(point) => BlockAnchor::AtPoint {
            x: frame.x_to_screen(point.x),
            y: frame.y_to_screen(point.y),
        },
        None => BlockAnchor::ByAlignment,
    };
    let rotation_origin = match state.origin {
        Some(point) => RotationOrigin::AtPoint {
            x: frame.x_to_screen(point.x),
            y: frame.y_to_screen(point.y),
        },
        None => RotationOrigin::ByAlignment,
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
            scroll,
            rotation_origin,
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
        fade: state.fade,
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

/// Сколько символов вектор резервирует заранее: оценка идёт по байтам текста реплики, а
/// враждебно длинный текст не должен сразу занимать память под все символы (их число ограничено
/// отдельно при разборе).
const MAX_PREALLOCATED_CHARS: usize = 512;

/// Проход по тексту реплики: накапливает символы и их виды.
struct TextWalker<'policy> {
    policy: &'policy LookPolicy,
    chars: Vec<SourceChar>,
    looks: Vec<SpanLook>,
    /// Индекс вида текущего состояния; `None` — состояние менялось после последнего символа.
    current_look: Option<usize>,
    /// Вид предыдущего символа (для склейки равных видов в один участок).
    previous_look: Option<usize>,
    /// Рисунки `\p` реплики; индексируются из `SourceChar::drawing`.
    drawings: Vec<DrawingSource>,
}

impl<'policy> TextWalker<'policy> {
    /// `expected_chars` - верхняя оценка числа символов (байтов текста), чтобы вектор символов
    /// не наращивался по одному.
    fn new(policy: &'policy LookPolicy, expected_chars: usize) -> Self {
        Self {
            policy,
            chars: Vec::with_capacity(expected_chars.min(MAX_PREALLOCATED_CHARS)),
            looks: Vec::new(),
            current_look: None,
            previous_look: None,
            drawings: Vec::new(),
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
                // Текст в режиме рисунка — это команды рисования, а не буквы: весь кусок до
                // следующего блока тегов становится одним псевдоглифом.
                if let AssDrawingMode::On { scale } = state.drawing {
                    self.push_drawing(content, scale, state);
                    return;
                }
                // Нулевой размер шрифта: буквы не рисуются (остальной текст рисуется), но
                // каждая остаётся символом без ширины и забирает теги караоке, как глиф libass.
                let look = self.letter_look_index(state);
                for ch in content.chars() {
                    // Возврат каретки и NUL разбор не должен пропускать, но на всякий случай
                    // они не попадают в раскладку (как в SRT-пути).
                    if ch != '\r' && ch != '\0' {
                        self.chars.push(SourceChar {
                            ch,
                            look,
                            fade: state.fade,
                            drawing: None,
                            karaoke: state.take_karaoke_mark(),
                        });
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

    /// Добавляет рисунок `\p<scale>` одним псевдоглифом (libass: `parse_events`, ветка
    /// `drawing_scale`: `code = 0xfffc`, `drawing_text` — сырой кусок текста).
    ///
    /// Рисунок пропускается только если оформление «мой стиль» (см.
    /// [`LookPolicy::drawing_event_scale`]). Размер шрифта на рисунок не влияет (решение
    /// владельца, как в libass): в стиле с `Fontsize: 0` рисунок рисуется, а буквы нет.
    fn push_drawing(&mut self, commands: &str, scale: u32, state: &mut RenderState<'_>) {
        let Some(frame) = self.policy.drawing_event_scale() else {
            return;
        };
        let look_index = self.look_index(state);
        let Some(look) = self.looks.get(look_index) else {
            return;
        };
        let source = DrawingSource {
            commands: commands.to_owned(),
            outline_scale: outline_scale(look.scale, scale, &frame),
            // libass: `drawing_pbo = (int) state->pbo`.
            baseline_offset: state.filters.baseline_offset() as i32,
        };
        let id = DrawingId(self.drawings.len());
        self.drawings.push(source);
        self.chars.push(SourceChar {
            ch: DRAWING_SYMBOL,
            look: look_index,
            fade: state.fade,
            drawing: Some(id),
            karaoke: state.take_karaoke_mark(),
        });
    }

    fn push_break(&mut self, state: &mut RenderState<'_>, ch: char) {
        if state.drawing.is_on() {
            return;
        }
        // Перенос при нулевом размере шрифта остаётся переносом (как глиф libass) и забирает
        // теги караоке; рисуется он тем же невидимым видом, что и буквы нулевого размера.
        let look = self.letter_look_index(state);
        self.chars.push(SourceChar {
            ch,
            look,
            fade: state.fade,
            drawing: None,
            karaoke: state.take_karaoke_mark(),
        });
    }

    /// Индекс вида буквы (или переноса) в текущем состоянии: обычный, а при нулевом размере
    /// шрифта — невидимый вид без ширины ([`LookPolicy::make_phantom_look`]).
    fn letter_look_index(&mut self, state: &RenderState<'_>) -> usize {
        if self.policy.draws_glyphs(state) {
            return self.look_index(state);
        }
        let phantom = self.policy.make_phantom_look(state);
        let known = self.looks.iter().position(|look| *look == phantom);
        let index = known.unwrap_or_else(|| {
            self.looks.push(phantom);
            self.looks.len() - 1
        });
        // Кэш `current_look` описывает видимый вид; невидимый его не заменяет.
        self.current_look = None;
        index
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

    /// Хоть один видимый символ повёрнут или наклонён (`\frx \fry \frz \fax \fay` или
    /// `Angle` стиля): такие реплики попадают в группу «надписей».
    fn has_transformed_text(&self) -> bool {
        self.looks
            .iter()
            .any(|look| look.rotation.is_rotated() || look.shear.is_sheared())
    }

    /// Готовый плоский текст; `kerning` — настройка `Kerning:` заголовка сценария.
    fn finish(&mut self, kerning: KerningMode) -> SourceText {
        SourceText {
            chars: std::mem::take(&mut self.chars),
            looks: std::mem::take(&mut self.looks),
            kerning,
            drawings: std::mem::take(&mut self.drawings),
        }
    }
}

#[cfg(test)]
mod geometry_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transform_tests;
