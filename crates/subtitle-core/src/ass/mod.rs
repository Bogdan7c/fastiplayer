//! Нейтральная модель ASS/SSA: сценарий (заголовок + стили) и реплики с разобранными тегами.
//!
//! Модуль содержит ТОЛЬКО данные и небольшие правила их интерпретации, которые однозначно
//! следуют из libass 0.17.5 (поиск стиля по имени, умолчания `PlayRes`, перевод выравнивания).
//! Разбор текста — в crate `subtitle-formats`; растеризация — в `subtitle-raster`.
//!
//! Устройство:
//! - [`AssScript`] — один на дорожку, раздаётся репликам через `Arc`;
//! - [`AssCue`] — содержимое одной реплики: стиль, отступы и последовательность
//!   [`AssTextItem`] (текст, переводы строк, блоки тегов [`AssOverride`]);
//! - теги хранятся уже с точной семантикой аргументов libass; то, что зависит от состояния
//!   рендера (например, «первый `\an` побеждает»), применяет растеризатор.

mod alignment;
mod animation_tags;
mod color;
mod cue;
mod effect;
mod event_format;
mod filter_tags;
mod geometry_tags;
mod karaoke_tags;
mod overrides;
mod script;
mod script_info;
mod style;

pub use alignment::{AssAlignment, AssHorizontalAlign, AssVerticalAlign};
pub use animation_tags::{AssTransform, AssTransformTiming};
pub use color::{AssColor, AssRgb};
pub use cue::{AssCue, AssEventMargins, AssHardOverrides, AssResolvedMargins, AssTextItem};
pub use effect::{AssBannerDirection, AssEffect, AssScrollDirection};
pub use event_format::{AssEventField, AssEventFormat};
pub use filter_tags::{ASS_BLUR_MAX_RADIUS, ASS_EDGE_BLUR_MAX, AssClip, AssClipMode, AssClipShape};
pub use geometry_tags::{AssFade, AssRotationAxis, AssShearAxis};
pub use karaoke_tags::{AssKaraoke, AssKaraokeKind};
pub use overrides::{
    AssAlphaTarget, AssAxes, AssColorSlot, AssDrawingMode, AssFontSize, AssMove, AssOverride,
};
pub use script::{AssScript, AssScriptKind, AssStyleIndex};
pub use script_info::{
    AssKerning, AssLayoutRes, AssPlayRes, AssScaledBorderAndShadow, AssScriptInfo, AssWrapStyle,
    AssYCbCrMatrix,
};
pub use style::{AssBold, AssBorderStyle, AssStyle};
