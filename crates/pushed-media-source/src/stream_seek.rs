//! Перемотка демуксера потока на стороне плеера: «можно ли» и сам переход.
//!
//! **Путь в `player-core`.** Источник открывается как обычный `PreparedMedia`
//! без порта перемотки по квитанциям, поэтому плеер зовёт
//! `Demuxer::seek_with_request` синхронно, на ходе владельца сессии (видео —
//! `DecodePointBefore`, только звук — `Accurate`). Это допустимо, потому что
//! перемотка источника не ждёт ничего: короткий замок буфера, запуск эпизода
//! (создание потока демуксера, без ожидания его работы), уничтожение старых
//! эпизодов (их `Drop` только будит поток — `ProgressiveDemuxer::drop` не делает
//! `join`). Байтов перемотка не ждёт: если цели нет на полке, демуксер отвечает
//! `TemporarilyUnavailable`, пока страница не пришлёт сегменты с цели, а плеер
//! всё это время стоит в «перемотке» (крутилка) с обычным сроком перемотки.
//!
//! **Порядок.** Сначала буфер потока перематывает все полосы разом (одно решение
//! «на полке / просить страницу» на поток — иначе запрос ушёл бы от каждой
//! полосы), затем внутренний демуксер (полоса или сборка «видео + звук»)
//! сбрасывает своё прочитанное и отвечает, откуда пойдут пакеты.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaDemuxError, MediaTime,
    TimelineNotSeekableReason,
};

use crate::stream_buffer::{StreamBuffer, StreamSeekError};
use crate::stream_state::StreamState;
use crate::time::PushedDuration;

/// Можно ли перематывать поток.
///
/// - полоса без меток времени (MPEG-TS) — нет: на полке нечего искать;
/// - конечная длительность — да (как у MSE `seekable` = `[0, duration]`);
/// - эфир — шкалу перемотки даёт порт живой шкалы, а не демуксер (плеер в этом
///   режиме `seekability` демуксера не спрашивает), здесь — «окно эфира
///   недоступно»;
/// - длительность неизвестна — нет: плеер читает `seekability` один раз при
///   подготовке, и поток с неизвестной длительностью на момент открытия
///   остаётся неперематываемым (ограничение; см. отчёт пакета F).
pub(crate) fn stream_seekability(buffer: &StreamBuffer, state: &StreamState) -> DemuxSeekability {
    if !buffer.all_lanes_timed() {
        return DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        };
    }
    match state.duration() {
        PushedDuration::Finite(_) => DemuxSeekability::Seekable,
        PushedDuration::Live => DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::LiveWindowUnavailable,
        },
        PushedDuration::Unknown => DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::UnknownTimeline,
        },
    }
}

/// Перематывает поток: буфер, затем внутренний демуксер.
///
/// `requested_position` всегда равна цели запроса; `actual_position` — откуда
/// пойдут пакеты (не позже цели): начало сегмента с целью на полке или сама
/// цель, если байтов с неё ещё нет (плеер декодирует вперёд до цели и прячет
/// всё более раннее).
pub(crate) fn seek_stream(
    buffer: &Arc<StreamBuffer>,
    state: &StreamState,
    inner: &mut (dyn Demuxer + Send),
    request: DemuxSeekRequest,
) -> Result<DemuxSeekResult> {
    if let Some(failure) = state.failure() {
        return Err(failure.into());
    }
    let repositioned = buffer
        .seek(request.timestamp)
        .map_err(|error| match error {
            StreamSeekError::UntimedLane { lane } => {
                anyhow::Error::new(MediaDemuxError::SeekUnavailable {
                    reason: format!("pushed lane {lane} carries no timestamps to seek by"),
                })
            }
            StreamSeekError::Stream(failure) => state.record_failure(failure).into(),
        })?;
    tracing::debug!(
        target_us = request.timestamp.as_micros(),
        page = ?repositioned.page,
        stale_episodes = repositioned.stale.len(),
        "Pushed media stream repositioned for a seek"
    );
    // Эпизоды старой позиции уничтожаются здесь, уже вне замка буфера.
    drop(repositioned.stale);
    let landed = inner.seek_with_request(request)?;
    Ok(DemuxSeekResult {
        requested_position: requested_position(request.timestamp),
        ..landed
    })
}

/// Цель запроса на шкале плеера.
const fn requested_position(target: Duration) -> MediaTime {
    MediaTime::from_duration(target)
}

#[cfg(test)]
#[path = "stream_seek_tests.rs"]
mod tests;
