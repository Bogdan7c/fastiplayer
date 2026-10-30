//! `DecodePointBefore` для цели раньше начала видео (видео начинается позже звука).
//!
//! Обычная проверка `DecodePointBefore` требует, чтобы первый video packet после seek-а лежал
//! не позже цели: иначе decoder не сможет показать кадр цели. Но если видеодорожка в файле
//! вообще начинается позже цели (например, видео с 2 с, звук с 0), keyframe-а ≤ цели не
//! существует, и retry назад упирается в начало файла с той же ошибкой
//! `first_video_after_target` — seek в первые секунды такого файла отказывал.
//!
//! Здесь это доказывается, а не угадывается: reader перезапускается с физического начала
//! источника и читает до первого video packet-а. Если даже он позже цели и не помечен как
//! non-keyframe, значит видео до цели нет. Тогда честная точка приземления — начало медиа:
//! звук отдаётся с начала (player-core обрезает его до цели, политика `DecodeForwardToTarget`),
//! а видео начинается со своего первого кадра. `actual_position` = 0 ≤ цели, поэтому контракт
//! «приземление не позже цели» сохраняется.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use media_core::{DemuxReadEvent, DemuxSeekResult, MediaTime, PacketKeyframe, TrackId};
use tracing::debug;

use super::SymphoniaDemuxer;
use super::decode_point_before::{
    DecodePointBeforeVerificationIssue, DecodePointBeforeVideoPacket,
    prepend_retained_lifecycle_events,
};

impl SymphoniaDemuxer {
    /// Пробует принять seek как «цель раньше начала видеодорожки».
    ///
    /// Вызывается только там, где `DecodePointBefore` иначе завершился бы ошибкой
    /// `FirstVideoAfterTarget`, поэтому обычные seek-и не платят за перечитывание начала.
    /// `Ok(None)` — случай не доказан (источник без reprobe, видео есть до цели, первый
    /// video packet не найден в пределе проверки); вызывающий возвращает прежнюю ошибку.
    pub(super) fn accept_decode_point_before_late_video_start(
        &mut self,
        requested_timestamp: Duration,
        video_track_id: TrackId,
        retained_lifecycle_events: VecDeque<DemuxReadEvent>,
    ) -> Result<Option<DemuxSeekResult>> {
        if !self.can_reprobe_current_source() {
            return Ok(None);
        }

        // Чтение с физического начала: первый video packet здесь — первый кадр дорожки.
        self.rebuild_format_reader_from_source_start()?;
        let verification = self.verify_decode_point_before_attempt(
            requested_timestamp,
            video_track_id,
            None, // minimum_video_timestamp: с начала файла отсекать нечего.
        )?;

        let Some(packet) = first_video_frame_after_target(verification.issue) else {
            return Ok(None);
        };

        debug!(
            target_ms = requested_timestamp.as_millis(),
            first_video_pts_ms = packet.pts.as_millis(),
            packets_checked = verification.packets_checked,
            "DecodePointBefore target precedes the video track start; landing at media start"
        );
        // Прочитанный для проверки prefix (звук с начала и первый video packet) не теряется:
        // он уходит в pipeline первым, как и в обычной успешной проверке.
        self.pending_events = prepend_retained_lifecycle_events(
            retained_lifecycle_events,
            verification.buffered_events,
        );

        Ok(Some(DemuxSeekResult {
            requested_position: MediaTime::from_duration(requested_timestamp),
            // Reader стоит в начале медиа: всё от начала до цели отдаётся и обрезается выше.
            actual_position: MediaTime::from_duration(Duration::ZERO),
            // Отдельного packet-а seek-дорожки на этой позиции нет: track timestamp не выдумываем.
            actual_track_timestamp: None,
        }))
    }
}

/// Выделяет первый кадр видео, если проверка с начала файла доказала поздний старт видео.
///
/// Доказательство — только `FirstVideoAfterTarget` с packet-ом, с которого можно начать
/// декодирование. Видео до цели (проверка прошла), отсутствие видео в пределе проверки или
/// non-keyframe первым кадром — не поздний старт, вызывающий сохраняет прежнюю ошибку.
fn first_video_frame_after_target(
    issue: Option<DecodePointBeforeVerificationIssue>,
) -> Option<DecodePointBeforeVideoPacket> {
    match issue {
        Some(DecodePointBeforeVerificationIssue::FirstVideoAfterTarget { packet })
            if packet.keyframe != PacketKeyframe::NotKeyframe =>
        {
            Some(packet)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::PacketKeyframe;

    use super::first_video_frame_after_target;
    use crate::symphonia_demuxer::decode_point_before::{
        DecodePointBeforeVerificationIssue, DecodePointBeforeVideoPacket,
    };

    fn video_packet(keyframe: PacketKeyframe) -> DecodePointBeforeVideoPacket {
        DecodePointBeforeVideoPacket {
            pts: Duration::from_secs(2),
            track_pts: None,
            keyframe,
        }
    }

    #[test]
    fn keyframe_or_unknown_after_target_proves_late_video_start() {
        for keyframe in [PacketKeyframe::Keyframe, PacketKeyframe::Unknown] {
            let packet = video_packet(keyframe);
            let issue = DecodePointBeforeVerificationIssue::FirstVideoAfterTarget { packet };

            assert_eq!(first_video_frame_after_target(Some(issue)), Some(packet));
        }
    }

    #[test]
    fn non_keyframe_first_frame_is_not_a_decode_start() {
        let issue = DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
            packet: video_packet(PacketKeyframe::NotKeyframe),
        };

        assert_eq!(first_video_frame_after_target(Some(issue)), None);
    }

    #[test]
    fn other_outcomes_do_not_prove_late_video_start() {
        let packet = video_packet(PacketKeyframe::Keyframe);
        for issue in [
            None,
            Some(DecodePointBeforeVerificationIssue::NoVideoPacket { packets_checked: 3 }),
            Some(DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet }),
            Some(DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { packet }),
        ] {
            assert_eq!(first_video_frame_after_target(issue), None);
        }
    }
}
