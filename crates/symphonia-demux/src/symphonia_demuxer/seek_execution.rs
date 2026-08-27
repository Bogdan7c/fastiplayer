use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use media_core::{DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, TrackId};
use symphonia::core::units::Timestamp;
use tracing::{debug, warn};

use crate::seek_mapper::{
    preferred_seek_track_id, seeked_to_timeline_result, symphonia_seek_error_to_demux_error,
    symphonia_seek_mode, symphonia_seek_target,
};
use crate::symphonia_api::{SeekErrorKind, SeekedTo, SymphoniaError, SymphoniaSeekMode};

use super::SymphoniaDemuxer;
use super::decode_point_before::{
    DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN, DECODE_POINT_BEFORE_MAX_RETRIES,
    decode_point_before_after_target_error, decode_point_before_initial_timestamp,
    decode_point_before_retry_timestamp, decode_point_before_retry_timestamp_for_issue,
    decode_point_before_verification_error, log_decode_point_before_uncertainty,
    matroska_decode_point_before_retry_timestamp, prepend_retained_lifecycle_events,
    retain_tracks_changed_events_from_failed_verification, seek_result_with_verified_video_packet,
    selected_video_track_id,
};

/// Первый шаг назад для поиска рабочей позиции, когда Symphonia считает in-range цель концом stream-а.
const IN_RANGE_OUT_OF_RANGE_SEEK_INITIAL_RETRY_OFFSET: Duration = Duration::from_millis(10);

/// Ограничивает число дорогих reprobe/seek попыток при повреждённых или неполных Matroska cues.
const IN_RANGE_OUT_OF_RANGE_SEEK_MAX_EXPONENTIAL_RETRIES: usize = 32;

/// Уточнение ближе миллисекунды не даёт практической пользы для текущих container timebase-ов.
const IN_RANGE_OUT_OF_RANGE_SEEK_REFINEMENT_EPSILON: Duration = Duration::from_millis(1);

/// Ограничивает binary refinement после того, как найден первый рабочий timestamp перед целью.
const IN_RANGE_OUT_OF_RANGE_SEEK_MAX_REFINEMENT_RETRIES: usize = 10;

/// Private owner bounded seek-исполнения. Он меняет reader state через authoritative demuxer,
/// но не владеет public track/event contract-ом, который остаётся в parent module.
impl SymphoniaDemuxer {
    /// Выполняет один backend seek и возвращает result относительно исходной пользовательской цели.
    pub(super) fn seek_symphonia_once(
        &mut self,
        request: DemuxSeekRequest,
        seek_mode: SymphoniaSeekMode,
        seek_track_id: Option<TrackId>,
        backend_timestamp: Duration,
        reprobe_before_seek: bool,
    ) -> Result<DemuxSeekResult> {
        let backend_request = DemuxSeekRequest {
            timestamp: backend_timestamp,
            mode: request.mode,
        };
        let mut rebuilt_before_seek = false;
        if reprobe_before_seek && self.can_reprobe_current_source() {
            self.rebuild_format_reader_from_source_start()?;
            rebuilt_before_seek = true;
        }

        let seeked_to = match self.seek_symphonia_with_in_range_retry(
            seek_mode,
            backend_request,
            seek_track_id,
            "seek",
        ) {
            Ok(seeked_to) => seeked_to,
            Err(error)
                if reprobe_before_seek
                    && self.can_reprobe_current_source()
                    && !rebuilt_before_seek =>
            {
                warn!(
                    source = %self.source_label,
                    error = %error,
                    "Symphonia seek failed; rebuilding FormatReader and retrying once"
                );
                self.rebuild_format_reader_from_source_start()?;
                self.seek_symphonia_with_in_range_retry(
                    seek_mode,
                    backend_request,
                    seek_track_id,
                    "seek_after_reprobe",
                )?
            }
            Err(error) => return Err(error),
        };

        Ok(seeked_to_timeline_result(
            request.timestamp,
            seeked_to,
            &self.track_map,
        ))
    }

    /// Выполняет Symphonia seek и чинит in-range цели, которые backend считает концом stream-а.
    fn seek_symphonia_with_in_range_retry(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        operation: &'static str,
    ) -> Result<SeekedTo> {
        match self.seek_symphonia_raw(seek_mode, backend_request, seek_track_id, operation)? {
            Ok(seeked_to) => Ok(seeked_to),
            Err(error)
                if is_symphonia_out_of_range_seek_error(&error)
                    && backend_request.mode == DemuxSeekMode::DecodePointBefore
                    && backend_request.timestamp.is_zero()
                    && self.can_reprobe_current_source() =>
            {
                self.reset_decode_point_before_to_source_start(seek_track_id)
            }
            Err(error)
                if is_symphonia_out_of_range_seek_error(&error)
                    && self.can_reprobe_current_source()
                    && in_range_out_of_range_seek_can_retry(
                        backend_request.timestamp,
                        self.duration,
                    ) =>
            {
                self.retry_in_range_out_of_range_seek(
                    seek_mode,
                    backend_request,
                    seek_track_id,
                    error,
                )
            }
            Err(error) => Err(symphonia_seek_error_to_demux_error(error)),
        }
    }

    /// Один raw seek без адаптации ошибок; нужен, чтобы не потерять `SeekErrorKind`.
    fn seek_symphonia_raw(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        operation: &'static str,
    ) -> Result<std::result::Result<SeekedTo, SymphoniaError>> {
        let seek_target = symphonia_seek_target(backend_request, seek_track_id);
        Ok(self.format_mut(operation)?.seek(seek_mode, seek_target))
    }

    /// Для целей внутри public duration пробует ближайшие packet-safe позиции перед концом stream-а.
    fn retry_in_range_out_of_range_seek(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        original_error: SymphoniaError,
    ) -> Result<SeekedTo> {
        let mut failed_timestamp = backend_request.timestamp;
        let mut retry_offset = IN_RANGE_OUT_OF_RANGE_SEEK_INITIAL_RETRY_OFFSET;

        for retry_index in 0..IN_RANGE_OUT_OF_RANGE_SEEK_MAX_EXPONENTIAL_RETRIES {
            let retry_timestamp = backend_request.timestamp.saturating_sub(retry_offset);

            if retry_timestamp == failed_timestamp {
                break;
            }

            match self.attempt_in_range_out_of_range_retry(
                seek_mode,
                backend_request,
                seek_track_id,
                retry_timestamp,
                retry_index,
            )? {
                Ok(_) => {
                    return self.refine_in_range_out_of_range_seek(
                        seek_mode,
                        backend_request,
                        seek_track_id,
                        retry_timestamp,
                        failed_timestamp,
                    );
                }
                Err(error) if is_symphonia_out_of_range_seek_error(&error) => {
                    failed_timestamp = retry_timestamp;

                    if retry_timestamp.is_zero() {
                        break;
                    }

                    retry_offset = retry_offset
                        .checked_mul(2)
                        .unwrap_or(backend_request.timestamp);
                }
                Err(error) => return Err(symphonia_seek_error_to_demux_error(error)),
            }
        }

        Err(symphonia_seek_error_to_demux_error(original_error))
    }

    /// Уточняет найденный working timestamp вверх, чтобы не делать лишний audio/video pre-roll.
    fn refine_in_range_out_of_range_seek(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        mut accepted_timestamp: Duration,
        mut failed_timestamp: Duration,
    ) -> Result<SeekedTo> {
        for retry_index in 0..IN_RANGE_OUT_OF_RANGE_SEEK_MAX_REFINEMENT_RETRIES {
            let search_window = failed_timestamp.saturating_sub(accepted_timestamp);
            if search_window <= IN_RANGE_OUT_OF_RANGE_SEEK_REFINEMENT_EPSILON {
                break;
            }

            let retry_timestamp = accepted_timestamp + search_window / 2;

            match self.attempt_in_range_out_of_range_retry(
                seek_mode,
                backend_request,
                seek_track_id,
                retry_timestamp,
                retry_index,
            )? {
                Ok(_) => {
                    accepted_timestamp = retry_timestamp;
                }
                Err(error) if is_symphonia_out_of_range_seek_error(&error) => {
                    failed_timestamp = retry_timestamp;
                }
                Err(error) => return Err(symphonia_seek_error_to_demux_error(error)),
            }
        }

        match self.attempt_in_range_out_of_range_retry(
            seek_mode,
            backend_request,
            seek_track_id,
            accepted_timestamp,
            IN_RANGE_OUT_OF_RANGE_SEEK_MAX_REFINEMENT_RETRIES,
        )? {
            Ok(seeked_to) => Ok(seeked_to),
            Err(error) => {
                warn!(
                    source = %self.source_label,
                    accepted_retry_ms = accepted_timestamp.as_millis(),
                    error = %error,
                    "Принятый fallback seek Symphonia не удалось повторить после уточнения"
                );
                Err(symphonia_seek_error_to_demux_error(error))
            }
        }
    }

    /// Делает одну retry-попытку из чистого reader-а, если source позволяет reprobe.
    fn attempt_in_range_out_of_range_retry(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        retry_timestamp: Duration,
        retry_index: usize,
    ) -> Result<std::result::Result<SeekedTo, SymphoniaError>> {
        if self.can_reprobe_current_source() {
            self.rebuild_format_reader_from_source_start()?;
        }

        debug!(
            source = %self.source_label,
            target_ms = backend_request.timestamp.as_millis(),
            retry_ms = retry_timestamp.as_millis(),
            retry_index,
            demux_mode = ?backend_request.mode,
            seek_track_id = ?seek_track_id,
            "Цель seek внутри public duration, но вне выбранного Symphonia stream; пробуем раньше"
        );

        let retry_request = DemuxSeekRequest {
            timestamp: retry_timestamp,
            mode: backend_request.mode,
        };

        self.seek_symphonia_raw(
            seek_mode,
            retry_request,
            seek_track_id,
            "seek_in_range_out_of_range_retry",
        )
    }

    /// Возвращает seekable source к физическому началу для `DecodePointBefore(0)`.
    ///
    /// Некоторые Matroska reader-ы Symphonia отклоняют timestamp `0` как out-of-range,
    /// когда первый cluster/track timestamp начинается чуть позже нуля. Reprobe из
    /// source start выражает нужное намерение без выдуманного положительного timestamp;
    /// последующая packet verification по-прежнему проверяет keyframe и startup lead.
    fn reset_decode_point_before_to_source_start(
        &mut self,
        seek_track_id: Option<TrackId>,
    ) -> Result<SeekedTo> {
        self.rebuild_format_reader_from_source_start()?;
        Ok(SeekedTo {
            track_id: seek_track_id.map_or(0, TrackId::get),
            required_ts: Timestamp::ZERO,
            actual_ts: Timestamp::ZERO,
        })
    }

    /// Восстанавливает `DecodePointBefore`: успешный result не должен быть после requested target.
    pub(super) fn seek_decode_point_before(
        &mut self,
        request: DemuxSeekRequest,
        reprobe_before_first_seek: bool,
    ) -> Result<DemuxSeekResult> {
        let requested_timestamp = request.timestamp;
        // RC1: целимся в сам target (минус крошечный margin), а не в target − preroll,
        // чтобы stss/cues приземлились на ближайший keyframe ≤ target. 5-секундный
        // `decode_point_before_preroll` ниже остаётся только шагом retry-backoff-а.
        let mut backend_timestamp = decode_point_before_initial_timestamp(
            requested_timestamp,
            DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN,
        );
        let mut backend_seek_mode = symphonia_seek_mode(request.mode);
        if let Some(video_track_id) = selected_video_track_id(&self.tracks) {
            let (matroska_backend_timestamp, uses_matroska_cue_anchor) =
                self.matroska_decode_point_before_anchor(video_track_id, backend_timestamp);
            backend_timestamp = matroska_backend_timestamp;
            if uses_matroska_cue_anchor {
                backend_seek_mode = SymphoniaSeekMode::Coarse;
            }
        }
        let mut retained_lifecycle_events = VecDeque::new();
        let mut minimum_video_timestamp = None;

        for retry_index in 0..=DECODE_POINT_BEFORE_MAX_RETRIES {
            let seek_track_id = preferred_seek_track_id(&self.tracks);
            let seek_result = self.seek_symphonia_once(
                request,
                backend_seek_mode,
                seek_track_id,
                backend_timestamp,
                retry_index == 0 && reprobe_before_first_seek,
            )?;

            if let Some(video_track_id) = selected_video_track_id(&self.tracks) {
                let verification = self.verify_decode_point_before_attempt(
                    requested_timestamp,
                    video_track_id,
                    minimum_video_timestamp,
                )?;

                if let Some(issue) = verification.issue {
                    let matroska_cue_retry_timestamp = matroska_decode_point_before_retry_timestamp(
                        &self.matroska_cue_index,
                        video_track_id,
                        backend_timestamp,
                        issue,
                    );
                    let retry_timestamp =
                        if let Some(cue_retry_timestamp) = matroska_cue_retry_timestamp {
                            minimum_video_timestamp.get_or_insert(backend_timestamp);
                            Some(cue_retry_timestamp)
                        } else {
                            decode_point_before_retry_timestamp_for_issue(
                                backend_timestamp,
                                requested_timestamp,
                                issue,
                                retry_index,
                                self.options.decode_point_before_preroll(),
                                self.options.decode_point_before_max_accepted_preroll(),
                            )
                        };
                    let Some(retry_timestamp) = retry_timestamp else {
                        return Err(decode_point_before_verification_error(
                            requested_timestamp,
                            issue,
                            verification.packets_checked,
                            retry_index,
                        ));
                    };

                    if retry_index == DECODE_POINT_BEFORE_MAX_RETRIES
                        || retry_timestamp == backend_timestamp
                    {
                        return Err(decode_point_before_verification_error(
                            requested_timestamp,
                            issue,
                            verification.packets_checked,
                            retry_index,
                        ));
                    }

                    let retry_uses_matroska_cue = matroska_cue_retry_timestamp.is_some();
                    debug!(
                        target_ms = requested_timestamp.as_millis(),
                        retry_ms = retry_timestamp.as_millis(),
                        retry_index,
                        reason = issue.reason(),
                        packets_checked = verification.packets_checked,
                        first_video_pts_ms = issue.first_video_pts().map(|pts| pts.as_millis()),
                        first_video_keyframe = ?issue.first_video_keyframe(),
                        retry_uses_matroska_cue,
                        "Post-seek verification rejected DecodePointBefore; retrying earlier"
                    );

                    retain_tracks_changed_events_from_failed_verification(
                        &mut retained_lifecycle_events,
                        verification.buffered_events,
                    );
                    backend_timestamp = retry_timestamp;
                    backend_seek_mode = if retry_uses_matroska_cue {
                        SymphoniaSeekMode::Coarse
                    } else {
                        symphonia_seek_mode(request.mode)
                    };
                    continue;
                }

                if let Some(accepted_video_packet) = verification.accepted_video_packet {
                    log_decode_point_before_uncertainty(requested_timestamp, accepted_video_packet);
                    self.pending_events = prepend_retained_lifecycle_events(
                        retained_lifecycle_events,
                        verification.buffered_events,
                    );
                    return Ok(seek_result_with_verified_video_packet(
                        seek_result,
                        accepted_video_packet,
                    ));
                }

                self.pending_events = prepend_retained_lifecycle_events(
                    retained_lifecycle_events,
                    verification.buffered_events,
                );
                return Ok(seek_result);
            }

            let actual_timestamp = seek_result.actual_position.as_duration();
            if actual_timestamp <= requested_timestamp {
                return Ok(seek_result);
            }

            let Some(retry_timestamp) = decode_point_before_retry_timestamp(
                backend_timestamp,
                requested_timestamp,
                actual_timestamp,
                retry_index,
            ) else {
                return Err(decode_point_before_after_target_error(
                    requested_timestamp,
                    actual_timestamp,
                    retry_index,
                ));
            };

            if retry_index == DECODE_POINT_BEFORE_MAX_RETRIES
                || retry_timestamp == backend_timestamp
            {
                return Err(decode_point_before_after_target_error(
                    requested_timestamp,
                    actual_timestamp,
                    retry_index,
                ));
            }

            debug!(
                target_ms = requested_timestamp.as_millis(),
                actual_ms = actual_timestamp.as_millis(),
                retry_ms = retry_timestamp.as_millis(),
                retry_index,
                "Symphonia seek returned after target; retrying DecodePointBefore earlier"
            );

            backend_timestamp = retry_timestamp;
        }

        unreachable!("bounded DecodePointBefore retry loop always returns")
    }
}

/// Отличает конец конкретного Symphonia stream-а от других seek failures.
fn is_symphonia_out_of_range_seek_error(error: &SymphoniaError) -> bool {
    matches!(error, SymphoniaError::SeekError(SeekErrorKind::OutOfRange))
}

/// Retry допустим только для цели, которую public timeline уже объявил достижимой.
fn in_range_out_of_range_seek_can_retry(
    backend_timestamp: Duration,
    duration: Option<Duration>,
) -> bool {
    duration.is_some_and(|duration| !backend_timestamp.is_zero() && backend_timestamp <= duration)
}
