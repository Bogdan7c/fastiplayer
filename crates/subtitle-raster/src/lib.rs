//! Растеризатор субтитров: от шрифтов до списка картинок поверх кадра.
//!
//! Crate строится слоями (волны сессии 02 плана `user/subtitles/`):
//! 1. [`overlay`] — нейтральный выход: холст, список [`SubtitleBitmap`] (8-битная маска
//!    покрытия + цвет + позиция) и признак изменения [`OverlayChange`] — по образцу `ASS_Image`
//!    из libass 0.17.5 (ISC). Рендер (GPU, GLES2) зависит только от этого типа.
//! 2. [`fonts`] — встроенный Noto Sans и алфавиты, системные шрифты через `fontdb`,
//!    интерфейс для вложенных (MKV); выбор лица по имени, fallback по глифам, метрики.
//! 3. Порт растеризатора libass 0.17.5 (внутренние модули `outline`, `raster`, `bitmap`):
//!    контуры 26.6, эллиптическая обводка, тайловая растеризация в маску покрытия.
//! 4. Текстовый конвейер: шейпинг (`harfrust`), BiDi, раскладка, композиция картинок.
//! 5. [`appearance`] — пользовательский стиль и пресеты; [`renderer`] — публичный
//!    [`SubtitleRasterizer`]: реплики + стиль + холст -> [`SubtitleOverlay`], кэши и
//!    «растеризуем только при изменении».
//!
//! Границы: crate не зависит от GPU, UI и плеера (закреплено в `scripts/check-refactor-guardrails.py`).
//! Все лимиты приходят параметрами от вызывающего; ошибка субтитров никогда не паникует и
//! не должна останавливать воспроизведение видео.

pub mod appearance;
mod bitmap;
pub mod cache;
mod compose;
mod event_style;
mod face_cache;
mod fixed_point;
pub mod fonts;
mod glyph_info;
mod glyph_render;
mod glyph_transform;
mod layout;
#[cfg(test)]
mod libass_parity;
mod outline;
pub mod overlay;
mod plate;
mod raster;
mod render_diagnostics;
pub mod renderer;
mod shaping;
mod source_text;
#[cfg(test)]
mod test_support;
mod text_renderer;

pub use appearance::{
    BoxDecoration, CanvasFraction, EmSize, FontSizeFraction, InvalidFraction, LineWidthFraction,
    OutlineShadowDecoration, SubtitleAppearance, TextDecoration,
};
pub use cache::CacheStatistics;
pub use face_cache::FaceCacheStatistics;
pub use fonts::{
    AscentDescentSource, AttachedFontError, AttachedFontLimits, AttachedFontRegistration,
    AttachedFontStatus, EMBEDDED_FONTS, EmbeddedFont, EmbeddedFontPurpose, FaceBytes,
    FaceDescription, FaceId, FaceMatch, FaceMetrics, FaceOrigin, FamilyMatch, FontLibraryError,
    FontLibraryStatistics, FontLookupCacheLimits, FontRequest, FontStyle, FontWeightClass,
    GlyphLookup, SubtitleFontLibrary, SyntheticBold, SyntheticItalic, SystemFontScanReport,
    SystemFontScanStatus, TextLanguagePreference,
};
pub use overlay::{
    BitmapOutsideCanvas, CanvasSize, OverlayChange, OverlayRevision, PixelPoint, SubtitleBitmap,
    SubtitleBitmapError, SubtitleBitmapKind, SubtitleOverlay,
};
pub use renderer::{
    CueKey, CueSourceId, RasterCacheLimits, RasterInputLimits, RasterLimits, RasterStatistics,
    RenderRequest, RenderableCue, SubtitleRasterizer,
};
pub use skrifa::GlyphId;
