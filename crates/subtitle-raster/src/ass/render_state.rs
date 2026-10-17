// Порт из libass 0.17.5: ass_render.c (init_render_context, ass_reset_render_context) и
// ass_parse.c (ass_parse_tags — применение тегов к состоянию рендера при `pwr = 1`,
// lookup_style_strict, ass_update_font).
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

//! Состояние рендера реплики ASS: что «сейчас включено» в текущей точке текста.
//!
//! libass разбирает теги и сразу меняет `RenderContext`; символы читаются с тем состоянием,
//! которое было на момент символа. Здесь то же самое: [`RenderState`] хранит значения в
//! единицах СЦЕНАРИЯ (как libass; масштаб до пикселей применяется позже, в момент символа),
//! а [`RenderState::apply`] применяет один разобранный тег ([`AssOverride`]).
//!
//! ## Две группы состояния
//! - Сбрасываемое `\r` (и начальное): шрифт, размер, цвета, обводка, тень, масштаб, интервал,
//!   подчёркивание/зачёркивание, кодировка, повороты `\frx \fry \frz` (по умолчанию `Angle`
//!   стиля) и наклон `\fax \fay` — `ass_reset_render_context`.
//! - Живущее всю реплику и `\r` НЕ сбрасываемое: выравнивание (побеждает ПЕРВЫЙ тег, даже
//!   недопустимый), режим переноса `\q` (побеждает ПОСЛЕДНИЙ), позиция `\pos` (побеждает
//!   первый), центр поворота `\org` (побеждает первый), режим рисунка `\p`.
//!
//! ## «Стиль сброса»
//! Теги без аргумента (`\fs`, `\bord`, `\b`, `\1c` ...) возвращают значение из `style` —
//! последнего стиля, к которому сбросили состояние (стиль реплики или `\rИмя`), как
//! `state->style` в libass.

use subtitle_core::{
    AssAlignment, AssAlphaTarget, AssAxes, AssBold, AssColor, AssColorSlot, AssDrawingMode,
    AssFontSize, AssOverride, AssRotationAxis, AssScript, AssShearAxis, AssStyle, AssStyleIndex,
    AssWrapStyle,
};

use super::filter_params::FilterParams;
use crate::event_fade::EventFade;
use crate::glyph_transform::{Rotation3d, ShearFactors};

/// Позиция, заданная `\pos` (координаты сценария).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScriptPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

/// Пара значений по осям (единицы сценария).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AxisValues {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

impl AxisValues {
    fn uniform(value: f64) -> Self {
        Self { x: value, y: value }
    }
}

/// Состояние рендера реплики.
#[derive(Debug, Clone)]
pub(crate) struct RenderState<'script> {
    script: &'script AssScript,
    /// Стиль реплики (для `\r` без имени и для промаха имени).
    event_style: &'script AssStyle,
    /// Стиль последнего сброса (`state->style`).
    style: &'script AssStyle,

    // --- Сбрасываемое `\r` ---
    pub(crate) font_name: String,
    pub(crate) bold: AssBold,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strike_out: bool,
    pub(crate) font_size: f64,
    /// Четыре цвета: основной, вторичный, контур, тень.
    pub(crate) colors: [AssColor; 4],
    pub(crate) border: AxisValues,
    pub(crate) shadow: AxisValues,
    pub(crate) scale: AxisValues,
    pub(crate) spacing: f64,
    pub(crate) encoding: i32,
    /// Повороты в градусах: `\frx`, `\fry` — 0 по умолчанию, `\frz` — `Angle` стиля.
    pub(crate) rotation: Rotation3d,
    /// Наклон `\fax`, `\fay`.
    pub(crate) shear: ShearFactors,
    /// Теги слоя 3: `\blur` и `\be` сбрасывает `\r`; `\clip`/`\iclip` и `\pbo` живут всю
    /// реплику (`init_render_context`, `ass_reset_render_context`).
    pub(crate) filters: FilterParams,

    // --- Живёт всю реплику ---
    pub(crate) alignment: AssAlignment,
    alignment_locked: bool,
    pub(crate) wrap_style: AssWrapStyle,
    pub(crate) position: Option<ScriptPoint>,
    /// Центр поворота `\org` (libass: `have_origin`, `org_x/org_y`).
    pub(crate) origin: Option<ScriptPoint>,
    pub(crate) drawing: AssDrawingMode,
    /// Затухание, действующее с места первого `\fad`/`\fade` (libass: `state->fade`, первый
    /// тег побеждает через `PARSED_FADE`); до тега — 0. `\r` его не сбрасывает.
    pub(crate) fade: EventFade,
    /// Значение затухания на момент показа: его получит `fade`, когда встретится тег.
    event_fade: EventFade,
    fade_locked: bool,
}

impl<'script> RenderState<'script> {
    /// Начальное состояние реплики (libass: `init_render_context`).
    ///
    /// `initial_wrap_style` — режим переноса дорожки (или «без переноса» для `Banner`).
    pub(crate) fn new(
        script: &'script AssScript,
        event_style: &'script AssStyle,
        initial_wrap_style: AssWrapStyle,
    ) -> Self {
        Self {
            script,
            event_style,
            style: event_style,
            font_name: event_style.font_name.clone(),
            bold: event_style.bold,
            italic: event_style.italic,
            underline: event_style.underline,
            strike_out: event_style.strike_out,
            font_size: event_style.font_size,
            colors: style_colors(event_style),
            border: AxisValues::uniform(event_style.outline),
            shadow: AxisValues::uniform(event_style.shadow),
            scale: AxisValues {
                x: event_style.scale_x,
                y: event_style.scale_y,
            },
            spacing: event_style.spacing,
            encoding: event_style.encoding,
            rotation: style_rotation(event_style),
            shear: ShearFactors::default(),
            filters: FilterParams::default(),
            alignment: event_style.alignment,
            alignment_locked: false,
            wrap_style: initial_wrap_style,
            position: None,
            origin: None,
            drawing: AssDrawingMode::Off,
            fade: EventFade::NONE,
            event_fade: EventFade::NONE,
            fade_locked: false,
        }
    }

    /// Задаёт значение затухания реплики на момент показа (см. модуль анимации). Оно вступает
    /// в силу в месте первого тега затухания, а не с начала текста.
    pub(crate) fn with_event_fade(mut self, fade: EventFade) -> Self {
        self.event_fade = fade;
        self
    }

    /// Стиль последнего сброса: от него зависят отступы реплики.
    pub(crate) fn style(&self) -> &'script AssStyle {
        self.style
    }

    /// Сброс к стилю (libass: `ass_reset_render_context`). Выравнивание, `\q`, `\pos` и режим
    /// рисунка не затрагиваются.
    fn reset_to(&mut self, style: &'script AssStyle) {
        self.style = style;
        self.colors = style_colors(style);
        self.underline = style.underline;
        self.strike_out = style.strike_out;
        self.font_size = style.font_size;
        self.font_name.clone_from(&style.font_name);
        self.bold = style.bold;
        self.italic = style.italic;
        self.border = AxisValues::uniform(style.outline);
        self.scale = AxisValues {
            x: style.scale_x,
            y: style.scale_y,
        };
        self.spacing = style.spacing;
        self.shadow = AxisValues::uniform(style.shadow);
        self.encoding = style.encoding;
        self.rotation = style_rotation(style);
        self.shear = ShearFactors::default();
        self.filters.reset_blur();
    }

    /// Применяет один тег (libass: ветки `ass_parse_tags` при `pwr = 1`, без анимации).
    pub(crate) fn apply(&mut self, tag: &AssOverride) {
        match tag {
            AssOverride::Bold(value) => self.bold = value.unwrap_or(self.style.bold),
            AssOverride::Italic(value) => self.italic = value.unwrap_or(self.style.italic),
            AssOverride::Underline(value) => {
                self.underline = value.unwrap_or(self.style.underline);
            }
            AssOverride::StrikeOut(value) => {
                self.strike_out = value.unwrap_or(self.style.strike_out);
            }
            AssOverride::FontName(name) => {
                self.font_name = name.clone().unwrap_or_else(|| self.style.font_name.clone());
            }
            AssOverride::FontSize(size) => self.apply_font_size(*size),
            AssOverride::FontEncoding(value) => {
                self.encoding = value.unwrap_or(self.style.encoding);
            }
            AssOverride::Color { slot, value } => self.apply_color(*slot, *value),
            AssOverride::Alpha { target, value } => self.apply_alpha(*target, *value),
            AssOverride::Border { axes, value } => {
                let fallback = self.style.outline;
                apply_axes(&mut self.border, *axes, value.unwrap_or(fallback));
            }
            AssOverride::Shadow { axes, value } => {
                let fallback = self.style.shadow;
                apply_axes(&mut self.shadow, *axes, value.unwrap_or(fallback));
            }
            AssOverride::Alignment(value) => {
                // Побеждает первый тег выравнивания, даже недопустимый (тогда — стиль).
                if !self.alignment_locked {
                    self.alignment = value.unwrap_or(self.style.alignment);
                    self.alignment_locked = true;
                }
            }
            AssOverride::Position { x, y } => {
                // Побеждает первый `\pos` (EVENT_POSITIONED).
                if self.position.is_none() {
                    self.position = Some(ScriptPoint { x: *x, y: *y });
                }
            }
            AssOverride::Move(movement) => {
                // libass тоже ставит EVENT_POSITIONED по `\move`: последующий `\pos` игнорируется.
                // Слой 1 не анимирует: реплика стоит в начальной точке (положение при `t <= t1`).
                if self.position.is_none() {
                    self.position = Some(ScriptPoint {
                        x: movement.from_x,
                        y: movement.from_y,
                    });
                }
            }
            AssOverride::Origin { x, y } => {
                // Побеждает первый `\org` (`have_origin`); коллизии отключает планировщик.
                if self.origin.is_none() {
                    self.origin = Some(ScriptPoint { x: *x, y: *y });
                }
            }
            AssOverride::Rotation { axis, degrees } => self.apply_rotation(*axis, *degrees),
            AssOverride::Shear { axis, factor } => {
                // Тег без аргумента сбрасывает наклон в 0 (не в стиль).
                let value = factor.unwrap_or(0.0);
                match axis {
                    AssShearAxis::X => self.shear.horizontal = value,
                    AssShearAxis::Y => self.shear.vertical = value,
                }
            }
            AssOverride::Fade(_) => {
                // Побеждает первый тег (`PARSED_FADE`); символы до него остаются без затухания.
                if !self.fade_locked {
                    self.fade = self.event_fade;
                    self.fade_locked = true;
                }
            }
            AssOverride::Reset(name) => self.apply_reset(name.as_deref()),
            AssOverride::WrapStyle(value) => {
                self.wrap_style = value.unwrap_or(self.script.info().wrap_style);
            }
            AssOverride::ScaleX(value) => self.scale.x = value.unwrap_or(self.style.scale_x),
            AssOverride::ScaleY(value) => self.scale.y = value.unwrap_or(self.style.scale_y),
            AssOverride::ResetScale => {
                self.scale = AxisValues {
                    x: self.style.scale_x,
                    y: self.style.scale_y,
                };
            }
            AssOverride::Spacing(value) => self.spacing = value.unwrap_or(self.style.spacing),
            AssOverride::Drawing(mode) => self.drawing = *mode,
            AssOverride::Blur(_)
            | AssOverride::EdgeBlur(_)
            | AssOverride::DrawingBaselineOffset(_)
            | AssOverride::Clip(_) => self.filters.apply(tag),
            // `\t` и караоке (сессия 10b) пока не исполняются (их считает разбор).
            AssOverride::Unsupported(_) => {}
        }
    }

    /// `\frx \fry \frz`: без аргумента X и Y возвращаются в 0, Z — в угол стиля.
    fn apply_rotation(&mut self, axis: AssRotationAxis, degrees: Option<f64>) {
        match axis {
            AssRotationAxis::X => self.rotation.x_degrees = degrees.unwrap_or(0.0),
            AssRotationAxis::Y => self.rotation.y_degrees = degrees.unwrap_or(0.0),
            AssRotationAxis::Z => {
                self.rotation.z_degrees = degrees.unwrap_or(self.style.angle);
            }
        }
    }

    /// `\fs`: итог `<= 0` заменяется размером стиля (libass).
    fn apply_font_size(&mut self, size: AssFontSize) {
        let new_size = match size {
            AssFontSize::Absolute(value) => value,
            AssFontSize::Relative(change) => self.font_size * (1.0 + change / 10.0),
            AssFontSize::Reset => 0.0,
        };
        self.font_size = if new_size > 0.0 {
            new_size
        } else {
            self.style.font_size
        };
    }

    /// `\c`, `\1c`..`\4c`: меняется RGB, прозрачность слота сохраняется (`change_color`).
    fn apply_color(&mut self, slot: AssColorSlot, value: Option<subtitle_core::AssRgb>) {
        let index = slot_index(slot);
        let new_rgb = value.unwrap_or_else(|| style_colors(self.style)[index].rgb());
        if let Some(color) = self.colors.get_mut(index) {
            *color = AssColor::new(new_rgb.red, new_rgb.green, new_rgb.blue, color.transparency);
        }
    }

    /// `\alpha`, `\1a`..`\4a`: меняется только прозрачность; без аргумента — прозрачность стиля.
    fn apply_alpha(&mut self, target: AssAlphaTarget, value: Option<u8>) {
        let slots: &[AssColorSlot] = match target {
            AssAlphaTarget::All => &[
                AssColorSlot::Primary,
                AssColorSlot::Secondary,
                AssColorSlot::Outline,
                AssColorSlot::Back,
            ],
            AssAlphaTarget::Slot(AssColorSlot::Primary) => &[AssColorSlot::Primary],
            AssAlphaTarget::Slot(AssColorSlot::Secondary) => &[AssColorSlot::Secondary],
            AssAlphaTarget::Slot(AssColorSlot::Outline) => &[AssColorSlot::Outline],
            AssAlphaTarget::Slot(AssColorSlot::Back) => &[AssColorSlot::Back],
        };
        for slot in slots {
            let index = slot_index(*slot);
            let transparency =
                value.unwrap_or_else(|| style_colors(self.style)[index].transparency);
            if let Some(color) = self.colors.get_mut(index) {
                *color = color.with_transparency(transparency);
            }
        }
    }

    /// `\r` и `\rИмя`: имя ищется строго; промах — стиль реплики (libass передаёт `NULL`).
    fn apply_reset(&mut self, name: Option<&str>) {
        let found: Option<AssStyleIndex> =
            name.and_then(|style_name| self.script.lookup_style_strict(style_name));
        let style = match found {
            Some(index) => self.script.style(index),
            None => self.event_style,
        };
        self.reset_to(style);
    }
}

/// Начальные повороты стиля: только `Angle` (вокруг оси Z).
fn style_rotation(style: &AssStyle) -> Rotation3d {
    Rotation3d {
        x_degrees: 0.0,
        y_degrees: 0.0,
        z_degrees: style.angle,
    }
}

/// Четыре цвета стиля в порядке слотов.
fn style_colors(style: &AssStyle) -> [AssColor; 4] {
    [
        style.primary_color,
        style.secondary_color,
        style.outline_color,
        style.back_color,
    ]
}

fn slot_index(slot: AssColorSlot) -> usize {
    match slot {
        AssColorSlot::Primary => 0,
        AssColorSlot::Secondary => 1,
        AssColorSlot::Outline => 2,
        AssColorSlot::Back => 3,
    }
}

fn apply_axes(target: &mut AxisValues, axes: AssAxes, value: f64) {
    match axes {
        AssAxes::Both => *target = AxisValues::uniform(value),
        AssAxes::X => target.x = value,
        AssAxes::Y => target.y = value,
    }
}

#[cfg(test)]
mod tests;
