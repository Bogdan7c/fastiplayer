//! Нейтральная модель субтитров: дорожка -> реплики -> временная шкала.
//!
//! Модель строится слоями:
//! 1. Дорожка ([`SubtitleTrackDescriptor`]) описывает источник, формат, язык и флаги.
//! 2. Реплика ([`SubtitleEvent`]) — содержимое ([`SubtitleContent`]) на полуинтервале
//!    времени `[start, end)`; текст хранится спанами со стилями автора ([`StyledText`]),
//!    а позиционирование — в [`CuePlacement`].
//! 3. Временная шкала ([`SubtitleTimeline`]) отвечает на вопрос «что показывать в момент T»
//!    за O((k + 1) · log n) и подсказывает ближайший момент смены кадра субтитров.
//!
//! Инвариант нейтральности: crate не зависит ни от GPU, ни от UI, ни от парсеров форматов.
//! Пользовательский стиль накладывается позже, на этапе растеризации; здесь хранится только
//! то, что задал автор субтитров (`None` у цвета означает «взять из пользовательского стиля»).

mod ass;
mod color;
mod event;
mod language;
mod language_table;
mod picture;
mod placement;
mod styled_text;
mod time;
mod timeline;
mod track;

pub use ass::{
    ASS_BLUR_MAX_RADIUS, ASS_EDGE_BLUR_MAX, AssAlignment, AssAlphaTarget, AssAxes,
    AssBannerDirection, AssBold, AssBorderStyle, AssClip, AssClipMode, AssClipShape, AssColor,
    AssColorSlot, AssCue, AssDrawingMode, AssEffect, AssEventField, AssEventFormat,
    AssEventMargins, AssFade, AssFontSize, AssHardOverrides, AssHorizontalAlign, AssKaraoke,
    AssKaraokeKind, AssKerning, AssLayoutRes, AssMove, AssOverride, AssPlayRes, AssResolvedMargins,
    AssRgb, AssRotationAxis, AssScaledBorderAndShadow, AssScript, AssScriptInfo, AssScriptKind,
    AssScrollDirection, AssShearAxis, AssStyle, AssStyleIndex, AssTextItem, AssTransform,
    AssTransformTiming, AssVerticalAlign, AssWrapStyle, AssYCbCrMatrix,
};
pub use color::{InvalidSubtitleColor, SubtitleColor};
pub use event::{
    InvalidSubtitleInterval, SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitleTextCue,
};
pub use language::{InvalidSubtitleLanguage, SubtitleLanguage};
pub use picture::{
    IndexedPicture, InvalidPictureCue, PaletteColor, PictureEncoding, PictureForcedFlag,
    PicturePalette, PicturePosition, PictureRect, PictureReferenceSize, SubtitlePictureCue,
};
pub use placement::{
    CueLine, CueLineAlign, CuePlacement, CuePosition, CuePositionAlign, CueTextAlign,
    CueWritingDirection, InvalidPercentOfVideo, PercentOfVideo, RegionPoint, RegionScroll,
    ScreenAnchor, SubtitleRegion,
};
pub use styled_text::{
    FontSlant, FontWeight, SpanContent, SpanStyle, StyledText, TextSpan, TextStrikethrough,
    TextUnderline,
};
pub use time::{SubtitleSyncOffset, SubtitleTime};
pub use timeline::{
    EndShortening, ShortenError, ShortenOutcome, ShorteningHistoryLost, SubtitleEventId,
    SubtitleTimeline, TimelineBuildReport, TimelineInsertOutcome,
};
pub use track::{
    SubtitleFormat, SubtitleTrackDescriptor, SubtitleTrackDisposition, SubtitleTrackId,
    SubtitleTrackOrigin,
};
