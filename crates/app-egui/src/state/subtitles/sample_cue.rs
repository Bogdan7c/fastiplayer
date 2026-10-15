//! Образцовая реплика для режима правки: «так будут выглядеть субтитры».
//!
//! Режим правки обязан всегда показывать что-то, за что можно взяться мышью, даже если у видео
//! нет дорожек или субтитры выключены (решение владельца 2). Образец - обычная дорожка из одной
//! реплики, которая идёт через тот же растеризатор, что и настоящие реплики: стиль, размер и
//! цвета на образце в точности те, что получат настоящие.

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleSyncOffset, SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan,
    TimelineInsertOutcome,
};
use subtitle_raster::CueSourceId;

use super::active_track::ActiveSubtitleTrack;

/// Текст образца (английский, как все тексты интерфейса).
pub(super) const SAMPLE_CUE_TEXT: &str = "This is how subtitles will look";

/// Идентификатор источника образца. Выдача идентификаторов настоящих дорожек
/// (`SourceIdAllocator`) начинается с нуля и растёт на единицу, поэтому до этого значения она
/// не доберётся; образец и дорожка не могут разделить кэш растеризатора.
pub(super) const SAMPLE_CUE_SOURCE_ID: CueSourceId = CueSourceId::new(u64::MAX);

/// Собирает дорожку образца или `None` (с записью в лог), если таймлайн не собрался.
pub(super) fn build_sample_track() -> Option<ActiveSubtitleTrack> {
    let cue = SubtitleTextCue {
        text: StyledText::new(vec![TextSpan::text(SAMPLE_CUE_TEXT, SpanStyle::default())]),
        placement: CuePlacement::default(),
    };
    // Реплика длится «всегда»: время кадра не должно её гасить.
    let event = match SubtitleEvent::new(
        SubtitleTime::from_micros(0),
        SubtitleTime::from_micros(i64::MAX),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(cue),
    ) {
        Ok(event) => event,
        Err(error) => {
            tracing::error!(%error, "The sample subtitle cue could not be built");
            return None;
        }
    };
    let mut timeline = SubtitleTimeline::new();
    match timeline.insert(event) {
        TimelineInsertOutcome::Inserted(_) | TimelineInsertOutcome::DuplicateIgnored(_) => {}
        TimelineInsertOutcome::CapacityExceeded => {
            tracing::error!("The sample subtitle timeline rejected its only cue");
            return None;
        }
    }
    Some(ActiveSubtitleTrack::new(
        SAMPLE_CUE_SOURCE_ID,
        timeline,
        SubtitleSyncOffset::ZERO,
        None,
    ))
}
