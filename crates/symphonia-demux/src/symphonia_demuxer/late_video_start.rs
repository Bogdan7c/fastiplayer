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
//!
//! Память и время ограничены отдельно. Доказательство НЕ держит прочитанный prefix: звук до
//! первого кадра может длиться минуты (видео с 30-й секунды и дальше), поэтому packet-ы только
//! считаются (бюджет `DemuxerOptions::late_video_start_proof_packet_limit`), а после
//! доказательства reader перезапускается с начала ещё раз и отдаёт тот же звук обычным чтением.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use media_core::{DemuxReadEvent, DemuxSeekResult, MediaTime, PacketKeyframe, TrackId};
use tracing::debug;

use super::SymphoniaDemuxer;
use super::decode_point_before::{DecodePointBeforeVideoPacket, prepend_retained_lifecycle_events};
use crate::error::DemuxError;

/// Что нашёл просмотр источника с начала в поисках первого кадра видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstVideoScan {
    /// Первый packet выбранной видеодорожки.
    Found(DecodePointBeforeVideoPacket),
    /// Бюджет packet-ов кончился раньше, чем встретилось видео.
    BudgetExhausted { packets_scanned: usize },
    /// Видео нет до конца источника.
    EndOfStream { packets_scanned: usize },
    /// Состав дорожек сменился по дороге: перезапуск с начала вернул бы прежний состав, и
    /// доказательство про «первый кадр» перестало бы относиться к той же дорожке.
    TrackLayoutChanged,
}

impl SymphoniaDemuxer {
    /// Пробует принять seek как «цель раньше начала видеодорожки».
    ///
    /// Вызывается только там, где `DecodePointBefore` иначе завершился бы ошибкой
    /// `FirstVideoAfterTarget`, поэтому обычные seek-и не платят за перечитывание начала.
    /// `Ok(None)` — случай не доказан (источник без reprobe, видео есть до цели, первый
    /// video packet не найден в бюджете проверки); вызывающий возвращает прежнюю ошибку.
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
        let (scan, scan_lifecycle_events) = self.scan_to_first_video_packet(video_track_id)?;

        let Some(first_video_frame) = proven_late_video_start(requested_timestamp, scan) else {
            debug!(
                target_ms = requested_timestamp.as_millis(),
                ?scan,
                "Late video start is not proven; DecodePointBefore keeps its error"
            );
            return Ok(None);
        };

        // Просмотр выбросил звук до первого кадра, чтобы не держать его в памяти. Отдаём его
        // обычным чтением: ещё один перезапуск с начала того же источника.
        self.rebuild_format_reader_from_source_start()?;
        debug!(
            target_ms = requested_timestamp.as_millis(),
            first_video_pts_ms = first_video_frame.pts.as_millis(),
            "DecodePointBefore target precedes the video track start; landing at media start"
        );
        // До следующего чтения доходят только lifecycle-события: прошлых retry и metadata,
        // замеченная просмотром (повторное чтение не сообщит о ней второй раз).
        self.pending_events =
            prepend_retained_lifecycle_events(retained_lifecycle_events, scan_lifecycle_events);

        Ok(Some(DemuxSeekResult {
            requested_position: MediaTime::from_duration(requested_timestamp),
            // Reader стоит в начале медиа: всё от начала до цели отдаётся и обрезается выше.
            actual_position: MediaTime::from_duration(Duration::ZERO),
            // Отдельного packet-а seek-дорожки на этой позиции нет: track timestamp не выдумываем.
            actual_track_timestamp: None,
        }))
    }

    /// Читает с текущей позиции до первого packet-а видеодорожки, не сохраняя packet-ы.
    ///
    /// Возвращает итог просмотра и встреченные metadata-события (их немного, и потерять их
    /// нельзя: при повторном чтении reader уже не сочтёт metadata новой).
    fn scan_to_first_video_packet(
        &mut self,
        video_track_id: TrackId,
    ) -> Result<(FirstVideoScan, VecDeque<DemuxReadEvent>)> {
        let packet_budget = self.options.late_video_start_proof_packet_limit();
        let mut lifecycle_events = VecDeque::new();
        let mut packets_scanned = 0_usize;

        while packets_scanned < packet_budget {
            match self.read_next_event_from_format()? {
                DemuxReadEvent::Packet(packet) => {
                    packets_scanned = packets_scanned.saturating_add(1);
                    if packet.track_id == video_track_id {
                        let first_video = DecodePointBeforeVideoPacket {
                            pts: packet.pts,
                            track_pts: packet.track_pts,
                            keyframe: packet.keyframe,
                        };
                        return Ok((FirstVideoScan::Found(first_video), lifecycle_events));
                    }
                }
                event @ DemuxReadEvent::MediaMetadataChanged(_) => {
                    lifecycle_events.push_back(event);
                }
                DemuxReadEvent::TracksChanged(_) => {
                    return Ok((FirstVideoScan::TrackLayoutChanged, lifecycle_events));
                }
                DemuxReadEvent::TemporarilyUnavailable(_) => {
                    return Err(
                        DemuxError::UnexpectedTemporaryReadinessDuringSeekVerification.into(),
                    );
                }
                DemuxReadEvent::EndOfStream => {
                    return Ok((
                        FirstVideoScan::EndOfStream { packets_scanned },
                        lifecycle_events,
                    ));
                }
            }
        }

        Ok((
            FirstVideoScan::BudgetExhausted { packets_scanned },
            lifecycle_events,
        ))
    }
}

/// Выделяет первый кадр видео, если просмотр с начала файла доказал поздний старт видео.
///
/// Доказательство — только первый video packet позже цели, с которого можно начать
/// декодирование (keyframe или неизвестно). Видео до цели, отсутствие видео в бюджете,
/// смена дорожек или non-keyframe первым кадром — не поздний старт.
fn proven_late_video_start(
    requested_timestamp: Duration,
    scan: FirstVideoScan,
) -> Option<DecodePointBeforeVideoPacket> {
    match scan {
        FirstVideoScan::Found(packet)
            if packet.pts > requested_timestamp
                && packet.keyframe != PacketKeyframe::NotKeyframe =>
        {
            Some(packet)
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "late_video_start/tests.rs"]
mod tests;
