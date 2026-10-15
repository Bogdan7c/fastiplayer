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

mod color;
mod event;
mod language;
mod language_table;
mod placement;
mod styled_text;
mod time;
mod timeline;
mod track;

pub use color::{InvalidSubtitleColor, SubtitleColor};
pub use event::{
    InvalidSubtitleInterval, SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitleTextCue,
};
pub use language::{InvalidSubtitleLanguage, SubtitleLanguage};
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
pub use timeline::{SubtitleEventId, SubtitleTimeline, TimelineBuildReport, TimelineInsertOutcome};
pub use track::{
    SubtitleFormat, SubtitleTrackDescriptor, SubtitleTrackDisposition, SubtitleTrackId,
    SubtitleTrackOrigin,
};
