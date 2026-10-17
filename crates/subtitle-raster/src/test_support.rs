//! Общие помощники тестов текстового конвейера.

use subtitle_core::{ScreenAnchor, SpanStyle, StyledText, SubtitleColor, TextSpan};

use crate::event_fade::EventFade;
use crate::event_style::{
    AxisPair, BaseDirection, BorderStyle, EventPlacement, EventStyle, StyleColors, TextMargins,
    WrapStyle,
};
use crate::face_cache::FaceCache;
use crate::fonts::{
    FontLookupCacheLimits, FontStyle, FontWeightClass, SubtitleFontLibrary, TextLanguagePreference,
};
use crate::glyph_info::TextCluster;
use crate::glyph_render::retrieve_glyphs;
use crate::shaping::shape_text;
use crate::source_text::SourceText;

pub(crate) const WHITE: SubtitleColor = SubtitleColor {
    red: 255,
    green: 255,
    blue: 255,
    alpha: 255,
};

/// Библиотека только со встроенными шрифтами.
pub(crate) fn embedded_fonts() -> SubtitleFontLibrary {
    SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts must load")
}

/// Простой стиль: Noto Sans 48 px, снизу по центру, поля 40 px, без обводки и тени.
pub(crate) fn plain_style() -> EventStyle {
    EventStyle {
        font_family: None,
        font_size: 48.0,
        font_weight: FontWeightClass::Regular,
        font_slant: FontStyle::Normal,
        border_style: BorderStyle::OutlineAndShadow,
        border: AxisPair::uniform(0.0),
        shadow: AxisPair::uniform(0.0),
        colors: StyleColors {
            primary: WHITE,
            secondary: WHITE,
            outline: SubtitleColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            back: SubtitleColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 155,
            },
        },
        alignment: ScreenAnchor::BottomCenter,
        margins: TextMargins {
            left: 40,
            right: 40,
            vertical: 40,
        },
        placement: EventPlacement::ByMargins,
        wrap_style: WrapStyle::Smart,
        line_spacing: 0.0,
        base_direction: BaseDirection::LeftToRight,
        fade: EventFade::NONE,
    }
}

/// Текст из одного спана без стиля.
pub(crate) fn plain_text(text: &str) -> StyledText {
    StyledText::new(vec![TextSpan::text(text, SpanStyle::default())])
}

/// Шейпинг и загрузка контуров (всё до раскладки).
pub(crate) fn shaped_clusters(
    fonts: &mut SubtitleFontLibrary,
    text: &StyledText,
    style: &EventStyle,
) -> Vec<TextCluster> {
    let source = SourceText::from_styled(text, style);
    let mut faces = FaceCache::new();
    let mut clusters = shape_text(
        fonts,
        &mut faces,
        &source,
        style,
        TextLanguagePreference::unknown(),
    )
    .expect("shaping");
    retrieve_glyphs(&faces, &mut clusters, &source.looks).expect("outlines");
    clusters
}
