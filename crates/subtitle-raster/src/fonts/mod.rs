//! Шрифты субтитров: встроенные Noto, системные (`fontdb`), вложенные; выбор лица по запросу,
//! fallback по глифам и вертикальные метрики.
//!
//! Границы: модуль не знает про шейпинг, контуры и растеризацию — он отвечает на вопросы
//! «какое лицо взять по имени» и «в каком лице есть глиф для символа».

mod attached;
mod counters;
mod coverage;
mod embedded;
mod face;
mod face_language;
#[cfg(test)]
mod fallback_tests;
mod family_index;
mod glyph_lookup;
mod language;
mod library;
mod lookup_cache;
mod metrics;
mod resolve;
mod shared_bytes;
mod style;
mod system_fallback;
mod system_scan;
#[cfg(test)]
pub(crate) mod test_fonts;
#[cfg(test)]
mod tests;

pub use attached::{
    AttachedFontError, AttachedFontLimits, AttachedFontRegistration, AttachedFontStatus,
};
pub use embedded::{EMBEDDED_FONTS, EmbeddedFont, EmbeddedFontPurpose};
pub use face::{FaceBytes, FaceId, FaceOrigin};
pub use glyph_lookup::GlyphLookup;
pub use language::TextLanguagePreference;
pub use library::{
    FaceDescription, FontLibraryError, FontLibraryStatistics, SubtitleFontLibrary,
    SystemFontScanReport, SystemFontScanStatus,
};
pub use lookup_cache::FontLookupCacheLimits;
pub use metrics::{AscentDescentSource, FaceMetrics};
pub use resolve::{FaceMatch, FamilyMatch};
pub use style::{FontRequest, FontStyle, FontWeightClass, SyntheticBold, SyntheticItalic};
