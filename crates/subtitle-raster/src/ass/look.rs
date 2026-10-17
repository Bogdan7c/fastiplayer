//! Превращение состояния рендера ASS в вид символов конвейера (`SpanLook`, значения в пикселях).
//!
//! В libass это делает `parse_events`: в `GlyphInfo` копируются значения состояния, а размер
//! шрифта, обводка и тень умножаются на масштаб кадра (`screen_scale_y`, `border_scale_*`).
//! Здесь та же операция вынесена в [`LookPolicy`] — единственное место, где решается, ЧЬЁ
//! оформление берётся:
//! - [`LookPolicy::author`] — значения тегов и стиля автора;
//! - [`LookPolicy::user`] — шрифт, размер, цвета, обводка, тень и подложка из пользовательского
//!   стиля; от автора остаются только жирность, наклон, подчёркивание и зачёркивание.

use subtitle_core::{AssBold, AssBorderStyle, AssColor, SubtitleColor};

use super::frame_scale::EventScale;
use super::render_state::RenderState;
use crate::event_style::{AxisPair, BorderStyle, StyleColors};
use crate::fonts::{FontStyle, FontWeightClass};
use crate::glyph_transform::{Rotation3d, ShearFactors};
use crate::span_look::{SpanBlur, SpanLook, TextDecorations};

/// Во сколько раз размер шрифта может превышать большую сторону холста.
///
/// libass таких пределов не знает, но размер растровой маски растёт как квадрат размера буквы:
/// `\fs100000` из враждебного файла выделил бы гигабайты. Пределом служат четыре холста — буква
/// такого размера и так целиком закрывает кадр.
const MAX_FONT_SIZE_IN_CANVASES: f64 = 4.0;

/// Во сколько раз обводка и смещение тени могут превышать большую сторону холста: обводка в
/// целый холст уже закрашивает кадр, а стоимость обводки растёт с её толщиной.
const MAX_DECORATION_IN_CANVASES: f64 = 1.0;

/// Оформление, которое пользователь навязывает ASS-репликам («мой стиль»), уже в пикселях.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserLook {
    /// Размер шрифта («высота ячейки»).
    pub(crate) font_size: f64,
    /// Цвета.
    pub(crate) colors: StyleColors,
    /// Обводка.
    pub(crate) border: AxisPair,
    /// Тень.
    pub(crate) shadow: AxisPair,
    /// Масштаб расстояния камеры 3D-поворотов (`blur_scale_y` кадра), см. `SpanLook::camera_scale`.
    pub(crate) camera_scale: f64,
}

/// Чьё оформление применять к символам.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LookPolicy {
    /// Автор: значения в единицах сценария, умножаются на масштаб реплики.
    Author {
        /// Множители размеров реплики.
        scale: EventScale,
        /// Предел размера шрифта в пикселях (см. [`MAX_FONT_SIZE_IN_CANVASES`]).
        max_font_size: f64,
        /// Предел обводки и тени в пикселях (см. [`MAX_DECORATION_IN_CANVASES`]).
        max_decoration: f64,
    },
    /// Пользователь: оформление готово в пикселях.
    User(UserLook),
}

impl LookPolicy {
    /// Оформление автора. `canvas_side` — большая сторона холста в пикселях.
    pub(crate) fn author(scale: EventScale, canvas_side: f64) -> Self {
        Self::Author {
            scale,
            max_font_size: canvas_side * MAX_FONT_SIZE_IN_CANVASES,
            max_decoration: canvas_side * MAX_DECORATION_IN_CANVASES,
        }
    }

    /// Оформление пользователя (из `SubtitleAppearance`, уже переведённое в пиксели).
    pub(crate) fn user(look: UserLook) -> Self {
        Self::User(look)
    }

    /// Получают ли буквы текущего состояния размер, достаточный для глифов.
    ///
    /// Размер шрифта 0 (стиль с `Fontsize: 0`; тег `\fs0` сюда не приводит: libass возвращает
    /// размер стиля) делает глифы нулевыми: libass их не рисует, остальной текст реплики
    /// рисуется. Растеризатор такие буквы вообще не берёт в раскладку.
    pub(crate) fn draws_glyphs(&self, state: &RenderState<'_>) -> bool {
        match self {
            Self::Author { scale, .. } => (state.font_size * scale.screen_y).abs() > 0.0,
            Self::User(_) => true,
        }
    }

    /// Множители кадра для рисунков `\p`. Рисуются только рисунки автора.
    ///
    /// РЕШЕНИЕ ВЛАДЕЛЬЦА (17.10.2026, сессия 10a): в режиме «мой стиль» (`AssStyleSource::User`)
    /// рисунки автора скрыты, а авторское размытие (`\blur`, `\be`) не применяется (см.
    /// `SpanLook::blur` = `SpanBlur::NONE` у [`Self::User`] в [`Self::make_look`]; камера 3D-поворотов
    /// при этом сохраняется): рисунок и
    /// размытие — часть заданной автором геометрии и вида, а пользовательский стиль её заменяет.
    /// Поведение сохранено сознательно; оно описано в `docs/subtitles/compatibility.md` и
    /// закреплено тестами `user_style_skips_author_drawings_but_keeps_letters` и
    /// `user_style_mode_ignores_author_blur`.
    pub(crate) fn drawing_event_scale(&self) -> Option<EventScale> {
        match self {
            Self::Author { scale, .. } => Some(*scale),
            Self::User(_) => None,
        }
    }

    /// Вид символов для текущего состояния.
    pub(crate) fn make_look(&self, state: &RenderState<'_>) -> SpanLook {
        let weight_value = font_weight_value(state.bold);
        let common_weight = FontWeightClass::from_os2_weight(weight_value);
        let slant = if state.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        let decorations = TextDecorations {
            underline: state.underline,
            strike_out: state.strike_out,
        };
        match self {
            Self::Author {
                scale,
                max_font_size,
                max_decoration,
            } => {
                // libass: `fabs(font_size * screen_scale_y)`.
                let font_size = (state.font_size * scale.screen_y).abs().min(*max_font_size);
                SpanLook {
                    font_family: family_name(&state.font_name),
                    font_size,
                    weight: common_weight,
                    weight_value,
                    slant,
                    colors: StyleColors {
                        primary: subtitle_color(state.colors[0]),
                        secondary: subtitle_color(state.colors[1]),
                        outline: subtitle_color(state.colors[2]),
                        back: subtitle_color(state.colors[3]),
                    },
                    border: AxisPair {
                        horizontal: (state.border.x * scale.border_x).min(*max_decoration),
                        vertical: (state.border.y * scale.border_y).min(*max_decoration),
                    },
                    shadow: AxisPair {
                        horizontal: clamp_signed(state.shadow.x * scale.border_x, *max_decoration),
                        vertical: clamp_signed(state.shadow.y * scale.border_y, *max_decoration),
                    },
                    border_style: border_style(state.style().border_style),
                    scale: AxisPair {
                        horizontal: bounded_scale(state.scale.x, font_size, *max_font_size),
                        vertical: bounded_scale(state.scale.y, font_size, *max_font_size),
                    },
                    // libass: `hspacing * screen_scale_x / par_scale_x` (множитель
                    // `scale_x` добавляет конвейер при расчёте продвижения).
                    letter_spacing: clamp_signed(
                        state.spacing * scale.screen_x / scale.pixel_aspect_x,
                        *max_font_size,
                    ),
                    decorations,
                    rotation: finite_rotation(state.rotation),
                    shear: finite_shear(state.shear),
                    blur: SpanBlur {
                        radius: state.filters.blur_radius(),
                        scale: AxisPair {
                            horizontal: scale.blur_x,
                            vertical: scale.blur_y,
                        },
                        edge_passes: state.filters.edge_blur(),
                    },
                    camera_scale: scale.blur_y,
                }
            }
            Self::User(user) => SpanLook {
                // Пользовательский шрифт: встроенный Noto Sans.
                font_family: None,
                font_size: user.font_size,
                weight: common_weight,
                weight_value,
                slant,
                colors: user.colors,
                border: user.border,
                shadow: user.shadow,
                border_style: BorderStyle::OutlineAndShadow,
                scale: AxisPair::uniform(1.0),
                letter_spacing: 0.0,
                decorations,
                rotation: finite_rotation(state.rotation),
                shear: finite_shear(state.shear),
                // Пользовательский стиль заменяет оформление автора целиком, размытие тоже.
                blur: SpanBlur::NONE,
                // Камера - свойство масштаба кадра, а не авторского оформления: libass берёт
                // `20000 * blur_scale_y` и при любых подменах стиля.
                camera_scale: user.camera_scale,
            },
        }
    }
}

/// Нечисловой угол (`\frz nan`) превращается в 0: поворот не должен ломать арифметику конвейера.
fn finite_rotation(rotation: Rotation3d) -> Rotation3d {
    let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
    Rotation3d {
        x_degrees: finite(rotation.x_degrees),
        y_degrees: finite(rotation.y_degrees),
        z_degrees: finite(rotation.z_degrees),
    }
}

/// Нечисловой наклон превращается в 0 (см. [`finite_rotation`]).
fn finite_shear(shear: ShearFactors) -> ShearFactors {
    let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
    ShearFactors {
        horizontal: finite(shear.horizontal),
        vertical: finite(shear.vertical),
    }
}

/// Ограничивает по модулю (смещение тени бывает отрицательным); NaN становится нулём.
fn clamp_signed(value: f64, limit: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(-limit, limit)
    }
}

/// Масштаб букв по одной оси (`\fscx`/`\fscy`) с защитой от враждебных значений.
///
/// Размер маски растёт как квадрат размера буквы, поэтому ширина (высота) буквы
/// `font_size * scale` не должна превышать `max_glyph_size` — тот же предел, что и у размера
/// шрифта. Отрицательные и нечисловые масштабы (libass допускает их в стиле) дают 0: буква
/// без площади не рисуется.
fn bounded_scale(scale: f64, font_size: f64, max_glyph_size: f64) -> f64 {
    if !scale.is_finite() || scale <= 0.0 {
        return 0.0;
    }
    if font_size <= 0.0 {
        return scale.min(1.0);
    }
    scale.min(max_glyph_size / font_size)
}

/// Числовой вес для `FontWeightClass` и синтетического утолщения (`ass_update_font`).
fn font_weight_value(bold: AssBold) -> u16 {
    u16::try_from(bold.font_weight().clamp(1, 1000)).unwrap_or(400)
}

/// Имя семейства: пустое — «по умолчанию»; ведущий `@` (вертикальный шрифт) отбрасывается:
/// вертикальный текст не поддержан, буквы рисуются горизонтально.
fn family_name(raw: &str) -> Option<String> {
    let name = raw.strip_prefix('@').unwrap_or(raw);
    (!name.is_empty()).then(|| name.to_owned())
}

/// Цвет ASS (прозрачность) -> цвет конвейера (непрозрачность).
fn subtitle_color(color: AssColor) -> SubtitleColor {
    SubtitleColor {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: color.opacity(),
    }
}

/// `BorderStyle` стиля ASS -> вид подложки конвейера.
fn border_style(style: AssBorderStyle) -> BorderStyle {
    match style {
        AssBorderStyle::OutlineAndShadow => BorderStyle::OutlineAndShadow,
        AssBorderStyle::OpaqueBox => BorderStyle::OpaqueBox,
        AssBorderStyle::Style4 => BorderStyle::EventBackground,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_name_strips_vertical_prefix_and_rejects_empty() {
        assert_eq!(family_name("Arial"), Some("Arial".to_owned()));
        assert_eq!(family_name("@Noto Sans"), Some("Noto Sans".to_owned()));
        assert_eq!(family_name(""), None);
        assert_eq!(family_name("@"), None);
    }

    #[test]
    fn weight_value_follows_libass_and_feeds_the_face_class() {
        assert_eq!(font_weight_value(AssBold::Normal), 400);
        assert_eq!(font_weight_value(AssBold::Bold), 700);
        assert_eq!(font_weight_value(AssBold::Weight(550)), 550);
        assert_eq!(font_weight_value(AssBold::Weight(100_000)), 1000);
        assert_eq!(
            FontWeightClass::from_os2_weight(font_weight_value(AssBold::Weight(549))),
            FontWeightClass::Regular
        );
    }

    #[test]
    fn shadow_offset_is_clamped_in_both_directions_and_nan_is_zero() {
        assert_eq!(clamp_signed(1.0e12, 100.0), 100.0);
        assert_eq!(clamp_signed(-1.0e12, 100.0), -100.0);
        assert_eq!(clamp_signed(-5.0, 100.0), -5.0);
        assert_eq!(clamp_signed(f64::NAN, 100.0), 0.0);
    }

    #[test]
    fn transparency_becomes_opacity() {
        let color = subtitle_color(AssColor::new(1, 2, 3, 0x80));
        assert_eq!(
            (color.red, color.green, color.blue, color.alpha),
            (1, 2, 3, 0x7f)
        );
    }
}
