//! Признак «demux упёрся в лимит, ожидая звук» для gap-политики session-а.
//!
//! Владелец audio-priority admission — `demux_admission`; здесь его состояние
//! переводится в intent-сигнал [`DemuxAudioPriorityWait`], не раскрывая лимиты наружу.

use super::super::PlayerSession;
use super::super::audio_gap::DemuxAudioPriorityWait;
use super::PlayerTickConfig;
use super::demux_admission::{
    audio_demux_catchup_needed, audio_priority_pending_video_limit_allows_demux,
};

/// Возвращает, упёрся ли audio-priority demux в лимит video packets.
///
/// В этом состоянии demux не прочитает больше ни одного packet-а, пока видео не
/// уйдёт в decoder, а decoder ждёт звук — без вмешательства прогресса нет.
pub(super) fn demux_audio_priority_wait(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> DemuxAudioPriorityWait {
    if audio_demux_catchup_needed(session, tick_config)
        && !audio_priority_pending_video_limit_allows_demux(session, tick_config)
    {
        DemuxAudioPriorityWait::SaturatedWithoutAudio
    } else {
        DemuxAudioPriorityWait::NotSaturated
    }
}
