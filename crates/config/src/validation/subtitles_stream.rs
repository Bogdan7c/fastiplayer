//! Validation вложенной секции `[subtitles.stream]`: диапазоны лимитов потоковых субтитров.
//!
//! Ошибки называют полный путь поля (`subtitles.stream.<field>`), сообщения английские.

use crate::{
    ConfigResult, MAX_STREAM_AHEAD_S, MAX_STREAM_BEHIND_S, MAX_STREAM_FAILURE_THRESHOLD,
    MAX_STREAM_LIVE_RETENTION_S, MAX_STREAM_MAX_PENDING_SEGMENTS, MAX_STREAM_MAX_SEGMENT_KB,
    MIN_STREAM_AHEAD_S, MIN_STREAM_BEHIND_S, MIN_STREAM_FAILURE_THRESHOLD,
    MIN_STREAM_LIVE_RETENTION_S, MIN_STREAM_MAX_PENDING_SEGMENTS, MIN_STREAM_MAX_SEGMENT_KB,
    SubtitleStreamConfig,
};

use super::{invalid_value, validate_u16_range};

/// Проверяет все лимиты потоковых субтитров и их согласованность.
pub(super) fn validate_stream_limits(stream: &SubtitleStreamConfig) -> ConfigResult<()> {
    validate_u16_range(
        "subtitles.stream.ahead_s",
        stream.ahead_s,
        MIN_STREAM_AHEAD_S,
        MAX_STREAM_AHEAD_S,
    )?;
    validate_u16_range(
        "subtitles.stream.behind_s",
        stream.behind_s,
        MIN_STREAM_BEHIND_S,
        MAX_STREAM_BEHIND_S,
    )?;
    validate_u16_range(
        "subtitles.stream.max_segment_kb",
        stream.max_segment_kb,
        MIN_STREAM_MAX_SEGMENT_KB,
        MAX_STREAM_MAX_SEGMENT_KB,
    )?;
    validate_u16_range(
        "subtitles.stream.max_pending_segments",
        stream.max_pending_segments,
        MIN_STREAM_MAX_PENDING_SEGMENTS,
        MAX_STREAM_MAX_PENDING_SEGMENTS,
    )?;
    validate_u16_range(
        "subtitles.stream.failure_threshold",
        stream.failure_threshold,
        MIN_STREAM_FAILURE_THRESHOLD,
        MAX_STREAM_FAILURE_THRESHOLD,
    )?;
    validate_u16_range(
        "subtitles.stream.live_retention_s",
        stream.live_retention_s,
        MIN_STREAM_LIVE_RETENTION_S,
        MAX_STREAM_LIVE_RETENTION_S,
    )?;
    // История live короче окна «назад» бессмысленна: докачанные назад реплики тут же
    // вытеснялись бы как устаревшие.
    if stream.live_retention_s < stream.behind_s {
        return Err(invalid_value(
            "subtitles.stream.live_retention_s",
            format!(
                "the history must not be shorter than subtitles.stream.behind_s ({}); got {}",
                stream.behind_s, stream.live_retention_s
            ),
        ));
    }
    Ok(())
}
