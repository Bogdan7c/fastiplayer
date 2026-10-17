// Порт из libass 0.17.5: ass_render.c (ass_layout_res, ass_start_frame — расчёт `par_scale_x`,
// init_font_scale, x2scr_pos, x2scr_left, x2scr_right, y2scr_pos, y2scr, y2scr_top, y2scr_sub)
// и ass_render_api.c (ass_set_font_scale).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
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

//! Масштаб «координаты сценария -> пиксели холста».
//!
//! ASS задаёт всё в координатах сценария (`PlayResX × PlayResY`), а рисуем мы на холсте
//! другого размера. Два уровня масштаба, как в libass:
//! - [`AssFrameScale`] — зависит только от сценария, холста и размера хранения видео
//!   (`ass_layout_res`, `par_scale_x`, перевод координат `x2scr_*`/`y2scr_*`);
//! - [`EventScale`] — множители размера шрифта, обводки и тени конкретной реплики
//!   (`init_font_scale`): к базовым добавляется пользовательский масштаб текста, если он
//!   применим к реплике.
//!
//! Режим `use_margins` libass выключен (картинка субтитров всегда ровно по холсту, чёрных
//! полос у ASS нет — решение владельца 12), поэтому `fit_width`/`fit_height` и поля кадра
//! не нужны: все `x2scr_*` совпадают с `x2scr_pos`, все `y2scr_*` — с `y2scr_pos`.

use subtitle_core::{AssScaledBorderAndShadow, AssScriptInfo};

use super::options::AssStorageSize;
use crate::overlay::CanvasSize;

/// Масштаб сценария на холсте.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AssFrameScale {
    canvas_width: f64,
    canvas_height: f64,
    play_res_x: f64,
    play_res_y: f64,
    /// Разрешение «раскладки» (libass: `ass_layout_res`): по нему считается масштаб
    /// обводки и тени, когда `ScaledBorderAndShadow: no`.
    layout_res_x: f64,
    layout_res_y: f64,
    /// `par_scale_x`: растяжение по горизонтали, когда пропорции хранения не равны пропорциям холста.
    pixel_aspect_x: f64,
    scaled_border_and_shadow: AssScaledBorderAndShadow,
}

impl AssFrameScale {
    /// Считает масштаб сценария на холсте (libass: `ass_start_frame` + `ass_layout_res`).
    pub(crate) fn new(info: &AssScriptInfo, canvas: CanvasSize, storage: AssStorageSize) -> Self {
        let canvas_width = f64::from(canvas.width());
        let canvas_height = f64::from(canvas.height());
        let play_res_x = f64::from(info.play_res.width);
        let play_res_y = f64::from(info.play_res.height);
        // LayoutRes сценария сильнее размера хранения, тот сильнее PlayRes.
        let layout_declared = info.layout_res.map(|layout| {
            (
                f64::from(layout.width),
                f64::from(layout.height),
                LayoutSource::Script,
            )
        });
        let storage_declared = match storage {
            AssStorageSize::Known { width, height } => Some((
                f64::from(width.get()),
                f64::from(height.get()),
                LayoutSource::Storage,
            )),
            AssStorageSize::Unknown => None,
        };
        let (layout_res_x, layout_res_y, source) = layout_declared
            .or(storage_declared)
            .unwrap_or((play_res_x, play_res_y, LayoutSource::PlayRes));
        // par = DAR / SAR только если разрешение раскладки известно (LayoutRes или
        // размер хранения); иначе пиксели считаются квадратными.
        let pixel_aspect_x = match source {
            LayoutSource::PlayRes => 1.0,
            LayoutSource::Script | LayoutSource::Storage => {
                (canvas_width / canvas_height) / (layout_res_x / layout_res_y)
            }
        };
        Self {
            canvas_width,
            canvas_height,
            play_res_x,
            play_res_y,
            layout_res_x,
            layout_res_y,
            pixel_aspect_x,
            scaled_border_and_shadow: info.scaled_border_and_shadow,
        }
    }

    /// `par_scale_x`.
    pub(crate) fn pixel_aspect_x(&self) -> f64 {
        self.pixel_aspect_x
    }

    /// Ширина сценария (`PlayResX`).
    pub(crate) fn play_res_x(&self) -> f64 {
        self.play_res_x
    }

    /// `layout_res.x / PlayResX`: во сколько раз разрешение раскладки крупнее сценария по
    /// горизонтали (libass: `scale_x` в `ass_apply_transition_effects`).
    pub(crate) fn layout_per_play_res_x(&self) -> f64 {
        self.layout_res_x / self.play_res_x
    }

    /// `layout_res.y / PlayResY` (libass: `scale_y` в `ass_apply_transition_effects`).
    pub(crate) fn layout_per_play_res_y(&self) -> f64 {
        self.layout_res_y / self.play_res_y
    }

    /// Горизонтальная координата сценария -> пиксели (libass: `x2scr_pos`; при выключенных
    /// полях совпадает с `x2scr_left` и `x2scr_right`).
    pub(crate) fn x_to_screen(&self, x: f64) -> f64 {
        x * self.canvas_width / self.pixel_aspect_x / self.play_res_x
    }

    /// Горизонтальная координата сценария -> пиксели БЕЗ деления на `par_scale_x` (libass:
    /// `x2scr_pos_scaled`; так переводятся углы прямоугольного клипа).
    pub(crate) fn x_to_screen_ignoring_pixel_aspect(&self, x: f64) -> f64 {
        x * self.canvas_width / self.play_res_x
    }

    /// Вертикальная координата сценария -> пиксели (libass: `y2scr_pos`; совпадает с
    /// `y2scr`, `y2scr_top` и `y2scr_sub` при выключенных полях).
    pub(crate) fn y_to_screen(&self, y: f64) -> f64 {
        y * self.canvas_height / self.play_res_y
    }

    /// Множители размеров реплики (libass: `init_font_scale`).
    ///
    /// `text_scale` — пользовательский масштаб (`font_size_coeff`); `None` — не применяется
    /// (событие с жёстким позиционированием, режим «мой стиль»).
    pub(crate) fn event_scale(&self, text_scale: Option<f64>) -> EventScale {
        let mut screen_x = self.canvas_width / self.play_res_x;
        let mut screen_y = self.canvas_height / self.play_res_y;
        let mut blur_x = self.canvas_width / self.layout_res_x;
        let mut blur_y = self.canvas_height / self.layout_res_y;
        let (mut border_x, mut border_y) = match self.scaled_border_and_shadow {
            AssScaledBorderAndShadow::Enabled => (screen_x, screen_y),
            AssScaledBorderAndShadow::Disabled => (blur_x, blur_y),
        };
        if let Some(coefficient) = text_scale {
            screen_x *= coefficient;
            screen_y *= coefficient;
            border_x *= coefficient;
            border_y *= coefficient;
            blur_x *= coefficient;
            blur_y *= coefficient;
        }
        EventScale {
            screen_x,
            screen_y,
            border_x,
            border_y,
            blur_x,
            blur_y,
            pixel_aspect_x: self.pixel_aspect_x,
        }
    }
}

/// Откуда взято разрешение раскладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutSource {
    Script,
    Storage,
    PlayRes,
}

/// Множители размеров одной реплики (libass: `screen_scale_x/y`, `border_scale_x/y`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EventScale {
    /// Горизонтальный масштаб размера букв и интервалов.
    pub(crate) screen_x: f64,
    /// Вертикальный масштаб размера букв: размер шрифта = `font_size * screen_y`.
    pub(crate) screen_y: f64,
    /// Горизонтальный масштаб обводки и тени.
    pub(crate) border_x: f64,
    /// Вертикальный масштаб обводки и тени.
    pub(crate) border_y: f64,
    /// Горизонтальный масштаб радиуса `\blur` (libass: `blur_scale_x`, `font_scr_w / layout_res.x`).
    pub(crate) blur_x: f64,
    /// Вертикальный масштаб радиуса `\blur` (libass: `blur_scale_y`).
    pub(crate) blur_y: f64,
    /// `par_scale_x`: на него делится горизонтальный размер (межбуквенный интервал, обводка).
    pub(crate) pixel_aspect_x: f64,
}

#[cfg(test)]
mod tests {
    use subtitle_core::{AssKerning, AssLayoutRes, AssPlayRes, AssWrapStyle, AssYCbCrMatrix};

    use super::*;

    fn info(
        play: (i32, i32),
        layout: Option<(i32, i32)>,
        scaled: AssScaledBorderAndShadow,
    ) -> AssScriptInfo {
        AssScriptInfo {
            play_res: AssPlayRes::resolve(play.0, play.1),
            layout_res: layout.map(|(width, height)| AssLayoutRes { width, height }),
            scaled_border_and_shadow: scaled,
            wrap_style: AssWrapStyle::Smart,
            kerning: AssKerning::Disabled,
            ycbcr_matrix: AssYCbCrMatrix::Default,
            language: None,
        }
    }

    fn canvas(width: u32, height: u32) -> CanvasSize {
        CanvasSize::new(width, height).expect("canvas")
    }

    #[test]
    fn coordinates_scale_from_play_res_to_canvas() {
        let scale = AssFrameScale::new(
            &info((384, 288), None, AssScaledBorderAndShadow::Enabled),
            canvas(1280, 720),
            AssStorageSize::Unknown,
        );
        assert!((scale.x_to_screen(192.0) - 640.0).abs() < 1e-9);
        assert!((scale.y_to_screen(144.0) - 360.0).abs() < 1e-9);
        assert_eq!(scale.pixel_aspect_x(), 1.0);
    }

    #[test]
    fn unknown_storage_scales_border_like_play_res_either_way() {
        for scaled in [
            AssScaledBorderAndShadow::Enabled,
            AssScaledBorderAndShadow::Disabled,
        ] {
            let scale = AssFrameScale::new(
                &info((384, 288), None, scaled),
                canvas(1280, 720),
                AssStorageSize::Unknown,
            );
            let event = scale.event_scale(None);
            assert!((event.border_y - 2.5).abs() < 1e-9, "{scaled:?}");
            assert!((event.screen_y - 2.5).abs() < 1e-9);
        }
    }

    #[test]
    fn known_storage_decouples_border_scale_when_scaled_border_is_off() {
        let storage = AssStorageSize::from_pixels(640, 360);
        let off = AssFrameScale::new(
            &info((384, 288), None, AssScaledBorderAndShadow::Disabled),
            canvas(1280, 720),
            storage,
        )
        .event_scale(None);
        // Обводка растёт вместе с видео (1280 / 640), а не с PlayRes.
        assert!((off.border_y - 2.0).abs() < 1e-9);
        assert!((off.screen_y - 2.5).abs() < 1e-9);
        let on = AssFrameScale::new(
            &info((384, 288), None, AssScaledBorderAndShadow::Enabled),
            canvas(1280, 720),
            storage,
        )
        .event_scale(None);
        assert!((on.border_y - 2.5).abs() < 1e-9);
    }

    #[test]
    fn layout_res_beats_storage_and_defines_pixel_aspect() {
        let scale = AssFrameScale::new(
            &info(
                (1920, 1080),
                Some((1440, 1080)),
                AssScaledBorderAndShadow::Disabled,
            ),
            canvas(1920, 1080),
            AssStorageSize::from_pixels(640, 360),
        );
        // DAR 16:9 при раскладке 4:3: текст растягивается по горизонтали на 4/3.
        assert!((scale.pixel_aspect_x() - 4.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn storage_with_the_same_aspect_keeps_square_pixels() {
        let scale = AssFrameScale::new(
            &info((384, 288), None, AssScaledBorderAndShadow::Enabled),
            canvas(1280, 720),
            AssStorageSize::from_pixels(1920, 1080),
        );
        assert!((scale.pixel_aspect_x() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn text_scale_multiplies_all_four_factors_when_applied() {
        let frame = AssFrameScale::new(
            &info((1280, 720), None, AssScaledBorderAndShadow::Enabled),
            canvas(1280, 720),
            AssStorageSize::Unknown,
        );
        let plain = frame.event_scale(None);
        let scaled = frame.event_scale(Some(1.5));
        assert!((scaled.screen_x - plain.screen_x * 1.5).abs() < 1e-12);
        assert!((scaled.screen_y - plain.screen_y * 1.5).abs() < 1e-12);
        assert!((scaled.border_x - plain.border_x * 1.5).abs() < 1e-12);
        assert!((scaled.border_y - plain.border_y * 1.5).abs() < 1e-12);
    }
}
