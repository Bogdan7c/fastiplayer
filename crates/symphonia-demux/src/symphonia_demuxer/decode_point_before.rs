use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekResult, MediaTime, Packet as OurPacket, PacketKeyframe, TrackId,
    TrackInfo, TrackKind, TrackTimestamp,
};
use tracing::{debug, warn};

use crate::error::DemuxError;
use crate::matroska_metadata::MatroskaCueIndex;

use super::SymphoniaDemuxer;

/// Лимит повторов для восстановления before-or-at-target semantics после backend overshoot.
pub(super) const DECODE_POINT_BEFORE_MAX_RETRIES: usize = 6;

/// Минимальный отступ назад, чтобы retry не попал в ту же after-target packet boundary.
const DECODE_POINT_BEFORE_RETRY_MARGIN: Duration = Duration::from_millis(1);

/// Отступ initial backend seek-а перед requested target.
///
/// RC1: раньше initial seek стартовал на целый `decode_point_before_preroll` (5 c) раньше
/// target, из-за чего container (даже со `stss`/cues) приземлялся на keyframe за несколько
/// GOP до цели. Это раздувало и demux-scan (особенно dense PCM), и decode-chain до target.
/// Теперь initial seek целится практически в сам target (минус 1 ms, чтобы не уйти после
/// цели на coarse backend-ах), и `stss`/cues снапают на БЛИЖАЙШИЙ decode-safe keyframe <= target.
/// `decode_point_before_preroll` остаётся шагом backoff-а для retry, когда первая попытка
/// приземлилась на non-keyframe или после target.
pub(super) const DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN: Duration = Duration::from_millis(1);

/// Допустимый lead первого стартового keyframe-а после zero seek.
///
/// MP4 с B-frames может иметь первый display keyframe на PTS 0 и отрицательный DTS.
/// Backend seek по неотрицательной decode timeline тогда возвращает первый packet
/// после нуля. Окно остаётся маленьким, чтобы не принимать настоящий late seek.
const DECODE_POINT_BEFORE_STARTUP_LEAD_TOLERANCE: Duration = Duration::from_millis(250);

/// Максимальный дрейф persisted/render position, который всё ещё означает начало media.
///
/// Позиция `0` после пересчёта через container time base или сохранения состояния может
/// вернуться как несколько микросекунд. Это не должно превращать стартовый keyframe в
/// нарушение `DecodePointBefore`, но миллисекундный предел не позволяет ослабить контракт
/// для обычного пользовательского seek-а внутри первого GOP.
const DECODE_POINT_BEFORE_NEAR_ZERO_TARGET_TOLERANCE: Duration = Duration::from_millis(1);

/// Packet-level наблюдение первого selected video packet-а после backend seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DecodePointBeforeVideoPacket {
    /// Нормализованный PTS, который увидит player pipeline.
    pub(super) pts: Duration,

    /// Raw PTS selected video track-а, если container сообщил time base.
    pub(super) track_pts: Option<TrackTimestamp>,

    /// Codec-aware keyframe-классификация packet-а.
    pub(super) keyframe: PacketKeyframe,
}

impl DecodePointBeforeVideoPacket {
    /// Снимает только metadata, нужную seek verification-у, не забирая ownership packet-а.
    fn from_packet(packet: &OurPacket) -> Self {
        Self {
            pts: packet.pts,
            track_pts: packet.track_pts,
            keyframe: packet.keyframe,
        }
    }
}

/// Результат проверки одной backend seek-попытки на packet boundary.
pub(super) struct DecodePointBeforeAttemptVerification {
    /// Events, которые verification прочитал и должен вернуть pipeline при успехе.
    pub(super) buffered_events: VecDeque<DemuxReadEvent>,

    /// Сколько supported packets было проверено в этой попытке.
    pub(super) packets_checked: usize,

    /// Первый packet выбранного video track-а, который можно использовать как decode-start.
    pub(super) accepted_video_packet: Option<DecodePointBeforeVideoPacket>,

    /// Причина retry/error, если попытка не доказала decode-safe старт.
    pub(super) issue: Option<DecodePointBeforeVerificationIssue>,
}

/// Почему packet-level проверка не приняла текущую backend seek-попытку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodePointBeforeVerificationIssue {
    /// Первый selected video packet находится после пользовательской цели.
    FirstVideoAfterTarget {
        /// Metadata первого selected video packet-а.
        packet: DecodePointBeforeVideoPacket,
    },

    /// Packet до target найден, но он точно не является decode-start keyframe.
    FirstVideoNotKeyframe {
        /// Metadata первого selected video packet-а.
        packet: DecodePointBeforeVideoPacket,
    },

    /// В bounded prefix не встретился packet выбранного video track-а.
    NoVideoPacket {
        /// Сколько supported packets уже пришлось prebuffer-нуть.
        packets_checked: usize,
    },

    /// Первый selected video packet слишком далеко до пользовательской цели.
    FirstVideoTooFarBeforeTarget {
        /// Metadata первого selected video packet-а.
        packet: DecodePointBeforeVideoPacket,
    },
}

impl DecodePointBeforeVerificationIssue {
    /// Стабильная причина для diagnostics и typed demux error-а.
    pub(super) const fn reason(self) -> &'static str {
        match self {
            Self::FirstVideoAfterTarget { .. } => "first_video_after_target",
            Self::FirstVideoNotKeyframe { .. } => "first_video_not_keyframe",
            Self::NoVideoPacket { .. } => "no_video_packet_in_verification_window",
            Self::FirstVideoTooFarBeforeTarget { .. } => "first_video_too_far_before_target",
        }
    }

    /// PTS первого selected video packet-а, если он был найден.
    pub(super) const fn first_video_pts(self) -> Option<Duration> {
        match self {
            Self::FirstVideoAfterTarget { packet } | Self::FirstVideoNotKeyframe { packet } => {
                Some(packet.pts)
            }
            Self::FirstVideoTooFarBeforeTarget { packet } => Some(packet.pts),
            Self::NoVideoPacket { .. } => None,
        }
    }

    /// Keyframe-классификация первого selected video packet-а, если он был найден.
    pub(super) const fn first_video_keyframe(self) -> Option<PacketKeyframe> {
        match self {
            Self::FirstVideoAfterTarget { packet } | Self::FirstVideoNotKeyframe { packet } => {
                Some(packet.keyframe)
            }
            Self::FirstVideoTooFarBeforeTarget { packet } => Some(packet.keyframe),
            Self::NoVideoPacket { .. } => None,
        }
    }
}

impl SymphoniaDemuxer {
    /// Выбирает Matroska/WebM cue anchor для первого `DecodePointBefore` backend seek-а.
    pub(super) fn matroska_decode_point_before_anchor(
        &self,
        video_track_id: TrackId,
        initial_backend_timestamp: Duration,
    ) -> (Duration, bool) {
        let Some(cue_timestamp) = self
            .matroska_cue_index
            .nearest_cue_before_or_at(video_track_id, initial_backend_timestamp)
        else {
            return (initial_backend_timestamp, false);
        };
        debug!(
            source = %self.source_label,
            track = video_track_id.get(),
            initial_backend_ms = initial_backend_timestamp.as_millis(),
            cue_anchor_ms = cue_timestamp.as_millis(),
            "Matroska/WebM DecodePointBefore использует ближайший video cue перед target"
        );

        (cue_timestamp, true)
    }

    /// Проверяет, что после seek-а первый selected video packet является decode-start до target.
    pub(super) fn verify_decode_point_before_attempt(
        &mut self,
        requested_timestamp: Duration,
        initial_video_track_id: TrackId,
        minimum_video_timestamp: Option<Duration>,
    ) -> Result<DecodePointBeforeAttemptVerification> {
        let mut buffered_events = VecDeque::new();
        let mut packets_checked = 0_usize;
        let mut video_track_id = initial_video_track_id;
        let packet_limit = self.options.decode_point_before_verification_packet_limit();
        let max_accepted_preroll = self.options.decode_point_before_max_accepted_preroll();
        let mut unresolved_video_issue = None;

        while packets_checked < packet_limit {
            let event = self.read_next_event_from_format()?;

            match &event {
                DemuxReadEvent::Packet(packet) => {
                    packets_checked = packets_checked.saturating_add(1);

                    if minimum_video_timestamp
                        .is_some_and(|minimum_timestamp| packet.pts < minimum_timestamp)
                    {
                        continue;
                    }

                    if packet.kind == TrackKind::Video && packet.track_id == video_track_id {
                        let video_packet = DecodePointBeforeVideoPacket::from_packet(packet);
                        let packet_issue = decode_point_before_packet_issue(
                            requested_timestamp,
                            video_packet,
                            max_accepted_preroll,
                        );

                        buffered_events.push_back(event);

                        match packet_issue {
                            None => {
                                return Ok(DecodePointBeforeAttemptVerification {
                                    buffered_events,
                                    packets_checked,
                                    accepted_video_packet: Some(video_packet),
                                    issue: None,
                                });
                            }
                            Some(DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
                                packet,
                            }) => {
                                return Ok(DecodePointBeforeAttemptVerification {
                                    buffered_events,
                                    packets_checked,
                                    accepted_video_packet: None,
                                    issue: Some(
                                        DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
                                            packet,
                                        },
                                    ),
                                });
                            }
                            Some(issue) => {
                                unresolved_video_issue =
                                    Some(decode_point_before_preferred_unresolved_issue(
                                        unresolved_video_issue,
                                        issue,
                                    ));
                                continue;
                            }
                        }
                    }
                }
                DemuxReadEvent::TracksChanged(_) => {
                    if let Some(updated_video_track_id) = selected_video_track_id(&self.tracks) {
                        video_track_id = updated_video_track_id;
                    }
                }
                DemuxReadEvent::MediaMetadataChanged(_) => continue,
                DemuxReadEvent::TemporarilyUnavailable(_) => {
                    return Err(
                        DemuxError::UnexpectedTemporaryReadinessDuringSeekVerification.into(),
                    );
                }
                DemuxReadEvent::EndOfStream => {
                    buffered_events.push_back(event);

                    return Ok(DecodePointBeforeAttemptVerification {
                        buffered_events,
                        packets_checked,
                        accepted_video_packet: None,
                        issue: Some(decode_point_before_unresolved_issue(
                            unresolved_video_issue,
                            packets_checked,
                        )),
                    });
                }
            }

            buffered_events.push_back(event);
        }

        Ok(DecodePointBeforeAttemptVerification {
            buffered_events,
            packets_checked,
            accepted_video_packet: None,
            issue: Some(decode_point_before_unresolved_issue(
                unresolved_video_issue,
                packets_checked,
            )),
        })
    }
}

/// Выбирает Matroska/WebM cue-retry только для ошибок, где более ранний cue может помочь.
pub(super) fn matroska_decode_point_before_retry_timestamp(
    cue_index: &MatroskaCueIndex,
    video_track_id: TrackId,
    backend_timestamp: Duration,
    issue: DecodePointBeforeVerificationIssue,
) -> Option<Duration> {
    if matches!(
        issue,
        DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { .. }
    ) {
        return None;
    }

    cue_index.nearest_cue_before(video_track_id, backend_timestamp)
}

/// Оставляет только lifecycle-события из rejected verification prefix-а.
pub(super) fn retain_tracks_changed_events_from_failed_verification(
    retained_lifecycle_events: &mut VecDeque<DemuxReadEvent>,
    rejected_buffered_events: VecDeque<DemuxReadEvent>,
) {
    for event in rejected_buffered_events {
        if matches!(event, DemuxReadEvent::TracksChanged(_)) {
            retained_lifecycle_events.push_back(event);
        }
    }
}

/// Возвращает успешный verification prefix после lifecycle событий прошлых retry.
pub(super) fn prepend_retained_lifecycle_events(
    mut retained_lifecycle_events: VecDeque<DemuxReadEvent>,
    successful_buffered_events: VecDeque<DemuxReadEvent>,
) -> VecDeque<DemuxReadEvent> {
    retained_lifecycle_events.extend(successful_buffered_events);
    retained_lifecycle_events
}

/// Выбирает самую полезную причину, если bounded prefix ещё не дал decode-start.
fn decode_point_before_preferred_unresolved_issue(
    current: Option<DecodePointBeforeVerificationIssue>,
    incoming: DecodePointBeforeVerificationIssue,
) -> DecodePointBeforeVerificationIssue {
    match (current, incoming) {
        (
            Some(DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet }),
            DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { .. },
        ) => DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet },
        (_, issue) => issue,
    }
}

/// Завершает bounded verification typed причиной, не теряя уже найденный video context.
fn decode_point_before_unresolved_issue(
    unresolved_video_issue: Option<DecodePointBeforeVerificationIssue>,
    packets_checked: usize,
) -> DecodePointBeforeVerificationIssue {
    unresolved_video_issue
        .unwrap_or(DecodePointBeforeVerificationIssue::NoVideoPacket { packets_checked })
}

/// Выбирает initial backend-цель так, чтобы container приземлился на ближайший
/// decode-safe keyframe непосредственно перед requested target (RC1), а не на GOP
/// за несколько секунд раньше. `initial_margin` - лишь маленький отступ против coarse
/// backend overshoot, а не полноценный pre-roll.
pub(super) fn decode_point_before_initial_timestamp(
    requested_timestamp: Duration,
    initial_margin: Duration,
) -> Duration {
    requested_timestamp.saturating_sub(initial_margin)
}

/// Считает следующую backend-цель по величине overshoot относительно исходного target-а.
pub(super) fn decode_point_before_retry_timestamp(
    backend_timestamp: Duration,
    requested_timestamp: Duration,
    actual_timestamp: Duration,
    retry_index: usize,
) -> Option<Duration> {
    let overshoot = actual_timestamp.checked_sub(requested_timestamp)?;
    let base_backoff = overshoot
        .checked_add(DECODE_POINT_BEFORE_RETRY_MARGIN)
        .unwrap_or(Duration::MAX);
    let retry_multiplier = 1_u32
        .checked_shl(retry_index.min(31) as u32)
        .unwrap_or(u32::MAX);
    let retry_backoff = base_backoff
        .checked_mul(retry_multiplier)
        .unwrap_or(Duration::MAX);

    Some(backend_timestamp.saturating_sub(retry_backoff))
}

/// Выбирает video track, для которого `DecodePointBefore` должен доказать packet-level старт.
pub(super) fn selected_video_track_id(tracks: &[TrackInfo]) -> Option<TrackId> {
    tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
}

/// Классифицирует первый selected video packet относительно `DecodePointBefore` contract-а.
fn decode_point_before_packet_issue(
    requested_timestamp: Duration,
    packet: DecodePointBeforeVideoPacket,
    max_accepted_preroll: Duration,
) -> Option<DecodePointBeforeVerificationIssue> {
    if packet.pts > requested_timestamp {
        if decode_point_before_startup_lead_is_accepted(requested_timestamp, packet) {
            return None;
        }

        return Some(DecodePointBeforeVerificationIssue::FirstVideoAfterTarget { packet });
    }

    if requested_timestamp.saturating_sub(packet.pts) > max_accepted_preroll {
        return Some(DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { packet });
    }

    if packet.keyframe == PacketKeyframe::NotKeyframe {
        return Some(DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet });
    }

    None
}

/// Проверяет единственное допустимое нарушение "packet <= target": старт media после
/// zero/near-zero seek, пережившего округление persisted или container time base.
fn decode_point_before_startup_lead_is_accepted(
    requested_timestamp: Duration,
    packet: DecodePointBeforeVideoPacket,
) -> bool {
    requested_timestamp <= DECODE_POINT_BEFORE_NEAR_ZERO_TARGET_TOLERANCE
        && packet.keyframe != PacketKeyframe::NotKeyframe
        && packet.pts <= DECODE_POINT_BEFORE_STARTUP_LEAD_TOLERANCE
}

/// Считает retry target для packet-level failure без смешивания разных причин.
pub(super) fn decode_point_before_retry_timestamp_for_issue(
    backend_timestamp: Duration,
    requested_timestamp: Duration,
    issue: DecodePointBeforeVerificationIssue,
    retry_index: usize,
    preroll: Duration,
    max_accepted_preroll: Duration,
) -> Option<Duration> {
    match issue {
        DecodePointBeforeVerificationIssue::FirstVideoAfterTarget { packet } => {
            if backend_timestamp < requested_timestamp {
                // Если backend уже искал раньше requested, но первый video packet всё равно
                // оказался после target, маленький отступ на величину packet overshoot-а
                // обычно остаётся внутри того же cue/cluster. Расширяем pre-roll окно, чтобы
                // действительно перейти к предыдущей decode-точке.
                Some(decode_point_before_expanding_retry_timestamp(
                    backend_timestamp,
                    retry_index,
                    preroll,
                ))
            } else {
                decode_point_before_retry_timestamp(
                    backend_timestamp,
                    requested_timestamp,
                    packet.pts,
                    retry_index,
                )
            }
        }
        DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { .. } => {
            decode_point_before_rescue_retry_timestamp(
                backend_timestamp,
                requested_timestamp,
                max_accepted_preroll,
            )
        }
        DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { .. }
        | DecodePointBeforeVerificationIssue::NoVideoPacket { .. } => Some(
            decode_point_before_expanding_retry_timestamp(backend_timestamp, retry_index, preroll),
        ),
    }
}

/// Пробует rescue seek ближе к target, если backend прыгнул слишком далеко назад.
fn decode_point_before_rescue_retry_timestamp(
    backend_timestamp: Duration,
    requested_timestamp: Duration,
    max_accepted_preroll: Duration,
) -> Option<Duration> {
    if backend_timestamp >= requested_timestamp {
        return None;
    }

    // Когда backend target ушёл раньше допустимого окна, возвращаемся к началу
    // этого окна, а не в сам target. Для WebM/VP9 seek в target может стартовать
    // уже после нужного keyframe-а, а bounded scan тогда снова уйдёт в after-target retry.
    let accepted_window_start = requested_timestamp.saturating_sub(max_accepted_preroll);

    if backend_timestamp < accepted_window_start {
        Some(accepted_window_start)
    } else {
        Some(requested_timestamp)
    }
}

/// Отодвигает backend target назад, когда packet prefix не дал usable video decode-start.
fn decode_point_before_expanding_retry_timestamp(
    backend_timestamp: Duration,
    retry_index: usize,
    preroll: Duration,
) -> Duration {
    let base_backoff = if preroll.is_zero() {
        DECODE_POINT_BEFORE_RETRY_MARGIN
    } else {
        preroll
    };
    let retry_multiplier = 1_u32
        .checked_shl(retry_index.min(31) as u32)
        .unwrap_or(u32::MAX);
    let retry_backoff = base_backoff
        .checked_mul(retry_multiplier)
        .unwrap_or(Duration::MAX);

    backend_timestamp.saturating_sub(retry_backoff)
}

/// Подменяет backend `SeekedTo.actual_ts` packet-level video timestamp-ом успешной проверки.
pub(super) fn seek_result_with_verified_video_packet(
    mut seek_result: DemuxSeekResult,
    first_video_packet: DecodePointBeforeVideoPacket,
) -> DemuxSeekResult {
    seek_result.actual_position = MediaTime::from_duration(first_video_packet.pts);
    seek_result.actual_track_timestamp = first_video_packet.track_pts;
    seek_result
}

/// Логирует случаи, где PTS contract доказан, а keyframe-классификация осталась неопределённой.
pub(super) fn log_decode_point_before_uncertainty(
    requested_timestamp: Duration,
    first_video_packet: DecodePointBeforeVideoPacket,
) {
    if first_video_packet.keyframe != PacketKeyframe::Unknown {
        return;
    }

    warn!(
        target_ms = requested_timestamp.as_millis(),
        first_video_pts_ms = first_video_packet.pts.as_millis(),
        first_video_track_timestamp = ?first_video_packet.track_pts,
        "DecodePointBefore accepted first video packet with unknown keyframe status"
    );
}

/// Создаёт typed ошибку packet-level проверки `DecodePointBefore`.
pub(super) fn decode_point_before_verification_error(
    requested_timestamp: Duration,
    issue: DecodePointBeforeVerificationIssue,
    packets_checked: usize,
    retry_index: usize,
) -> anyhow::Error {
    let effective_packets_checked = match issue {
        DecodePointBeforeVerificationIssue::NoVideoPacket { packets_checked } => packets_checked,
        _ => packets_checked,
    };

    DemuxError::DecodePointBeforeVerificationFailed {
        reason: issue.reason(),
        requested_position: requested_timestamp,
        attempts: retry_index + 1,
        packets_checked: effective_packets_checked,
        first_video_pts: issue.first_video_pts(),
        first_video_keyframe: issue.first_video_keyframe(),
    }
    .into()
}

/// Создаёт ошибку, если backend не смог честно выполнить before-or-at-target seek.
pub(super) fn decode_point_before_after_target_error(
    requested_timestamp: Duration,
    _actual_timestamp: Duration,
    retry_index: usize,
) -> anyhow::Error {
    DemuxError::DecodePointBeforeVerificationFailed {
        reason: "backend_actual_after_target",
        requested_position: requested_timestamp,
        attempts: retry_index + 1,
        packets_checked: 0,
        first_video_pts: None,
        first_video_keyframe: None,
    }
    .into()
}
