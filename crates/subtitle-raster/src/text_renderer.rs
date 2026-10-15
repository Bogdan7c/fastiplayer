//! Точка сборки текстового конвейера: реплика -> список картинок.
//!
//! Стадии (каждая — в своём модуле, порядок как в `ass_render_event` из libass 0.17.5):
//! 1. [`crate::source_text`] — плоский текст с внешним видом символов;
//! 2. [`crate::shaping`] — лица, письменность, BiDi, шейпинг `harfrust`;
//! 3. [`crate::glyph_render`] — контуры глифов (масштаб 256 px, как при `ASS_HINTING_NONE`);
//! 4. [`crate::layout`] — переносы, порядок BiDi, выравнивание, положение блока;
//! 5. [`crate::compose`] — растеризация, обводка, тень, плашка, обрезка по холсту.
//!
//! Ошибки типизированы; «пустая реплика» ошибкой не является — это пустой список.

use subtitle_core::StyledText;

use crate::compose::{ComposeError, ComposeInput, compose_text};
use crate::event_style::EventStyle;
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fonts::{SubtitleFontLibrary, TextLanguagePreference};
use crate::glyph_render::{FREETYPE_REQUEST_SIZE, retrieve_glyphs};
use crate::layout::{LineBox, arrange_text, line_boxes};
use crate::overlay::{CanvasSize, SubtitleBitmap};
use crate::render_diagnostics::RenderDiagnostics;
use crate::shaping::shape_text;
use crate::source_text::SourceText;

/// Почему реплика не отрисована.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum TextRenderError {
    /// Размер шрифта не положителен или не конечен.
    #[error("font size must be a positive finite number, got {size}")]
    InvalidFontSize {
        /// Полученное значение.
        size: f64,
    },
    /// Толщина обводки или смещение тени не являются конечными числами.
    #[error("border and shadow sizes must be finite numbers")]
    InvalidDecorationSize,
    /// Лицо шрифта недоступно или повреждено.
    #[error(transparent)]
    Face(#[from] FaceLoadError),
    /// Сбой композиции картинок.
    #[error(transparent)]
    Compose(#[from] ComposeError),
}

/// Результат отрисовки текста реплики.
#[derive(Debug)]
pub(crate) struct RenderedText {
    /// Картинки в порядке отрисовки, целиком внутри холста.
    pub(crate) images: Vec<SubtitleBitmap>,
    /// Прямоугольники видимых строк (для плашки позади строк); пусто для пустой реплики.
    pub(crate) lines: Vec<LineBox>,
    /// Что пропущено при отрисовке (глифы без контура, глифы без обводки).
    pub(crate) diagnostics: RenderDiagnostics,
}

/// Рисует одну текстовую реплику стилем `style` на холсте `canvas`.
///
/// Пустой текст и текст из одних пробелов дают пустой результат (подложка и тень без букв
/// не рисуются). Подготовленные лица берутся из `faces` и остаются там для следующих реплик:
/// владелец кэша решает, когда его чистить ([`FaceCache::trim_to`]).
pub(crate) fn render_cue_text(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    text: &StyledText,
    style: &EventStyle,
    canvas: CanvasSize,
    language: TextLanguagePreference,
) -> Result<RenderedText, TextRenderError> {
    validate_style(style)?;
    let source = SourceText::from_styled(text, style);
    if source.is_blank() {
        return Ok(RenderedText {
            images: Vec::new(),
            lines: Vec::new(),
            diagnostics: RenderDiagnostics::default(),
        });
    }

    let mut clusters = shape_text(fonts, faces, &source, style, language)?;
    let font_scale = style.font_size / FREETYPE_REQUEST_SIZE;
    let retrieved = retrieve_glyphs(faces, &mut clusters, &source.looks, font_scale)?;
    let layout = arrange_text(&mut clusters, style, canvas);
    // Строки читаем до композиции: она переводит позиции глифов в координаты холста.
    let lines = line_boxes(&clusters, &layout, style);
    let composed = compose_text(
        &mut clusters,
        &ComposeInput {
            looks: &source.looks,
            style,
            layout: &layout,
            canvas,
            font_scale,
        },
    )?;
    Ok(RenderedText {
        images: composed.images,
        lines,
        diagnostics: retrieved.combined(RenderDiagnostics {
            skipped_glyphs: 0,
            skipped_borders: composed.skipped_borders,
        }),
    })
}

/// Рисует реплику с новым кэшем лиц и отдаёт только картинки (удобство для тестов паритета).
#[cfg(test)]
pub(crate) fn render_text_event(
    fonts: &mut SubtitleFontLibrary,
    text: &StyledText,
    style: &EventStyle,
    canvas: CanvasSize,
) -> Result<Vec<SubtitleBitmap>, TextRenderError> {
    let mut faces = FaceCache::new();
    render_cue_text(
        fonts,
        &mut faces,
        text,
        style,
        canvas,
        TextLanguagePreference::unknown(),
    )
    .map(|rendered| rendered.images)
}

/// Проверяет числа стиля, от которых зависит арифметика конвейера.
fn validate_style(style: &EventStyle) -> Result<(), TextRenderError> {
    if !style.font_size.is_finite() || style.font_size <= 0.0 {
        return Err(TextRenderError::InvalidFontSize {
            size: style.font_size,
        });
    }
    let decorations = [
        style.border.horizontal,
        style.border.vertical,
        style.shadow.horizontal,
        style.shadow.vertical,
        style.line_spacing,
    ];
    if decorations.iter().any(|value| !value.is_finite()) {
        return Err(TextRenderError::InvalidDecorationSize);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
