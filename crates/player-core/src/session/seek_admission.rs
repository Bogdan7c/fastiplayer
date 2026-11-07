//! Typed admission обычного seek-а и recovery протухшей live-позиции.
//!
//! Здесь же — граница мягкого отказа команды перемотки эфира (решение владельца 233):
//! цель вне окна эфира или эфир без DVR — это «команда не выполнима», а не «плеер сломан».
//! Отказ происходит ДО любых изменений session: идущий seek, scrub-жест, Playing/Paused,
//! позиция и receipts остаются как были; зритель получает recoverable-ошибку (временное
//! уведомление), вызывающий — typed [`PlayerCommandReject::SeekTargetUnavailable`].

use media_core::{MediaTime, TimelineNotSeekableReason};

use crate::seek_state::SeekTargetRetention;
use crate::{
    PlayerCommandOutcome, PlayerCommandReject, PlayerError, PlayerErrorKind,
    SeekTargetUnavailableReason,
};

use super::PlayerSession;

/// Причина, по которой owner просит войти в общий seek lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeekTimelineAdmission {
    /// Пользовательский seek требует packet-proven public range.
    PublicSeekableRange,
    /// Play відновлює позицію, яку authoritative availability уже витіснила.
    ExpiredLiveAvailability,
    /// Продолжение у безопасного края после посадки, чью цель окно эфира съело на лету
    /// (решение владельца 233, `dynamic_timeline/expired_seek_recovery.rs`).
    ExpiredSeekContinuation,
}

impl SeekTimelineAdmission {
    /// Не теряет semantic intent после initial admission и worker enqueue.
    pub(super) const fn target_retention(self) -> SeekTargetRetention {
        match self {
            Self::PublicSeekableRange => SeekTargetRetention::ExactPublicRange,
            // Продолжение держится за availability, как recovery паузы: протухнуть оно
            // может только вместе с самим окном, и тогда второго прыжка нет.
            Self::ExpiredLiveAvailability | Self::ExpiredSeekContinuation => {
                SeekTargetRetention::LiveAvailability
            }
        }
    }
}

/// Итог приёма команды перемотки (seek/scrub) на границе session.
///
/// Разводит «цель принята — дальше работает обычный seek lifecycle» и «цель отклонена
/// мягко» без `bool`: вызывающему (dispatch) важно отличать отказ от применения.
#[must_use = "отказ перемотки должен дойти до PlayerCommandOutcome::Rejected"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeekCommandAdmission {
    /// Цель принята. Прежние recoverable-отказы внутри lifecycle (нет demuxer-а,
    /// не-seekable timeline) по-прежнему публикуются ошибкой и сюда не попадают.
    Admitted,
    /// Цель отклонена до изменения session; recoverable-ошибка уже опубликована.
    Refused(SeekTargetUnavailableReason),
}

impl SeekCommandAdmission {
    /// Переводит итог приёма в public результат команды.
    pub(super) const fn into_command_outcome(self) -> PlayerCommandOutcome {
        match self {
            Self::Admitted => PlayerCommandOutcome::Applied,
            Self::Refused(reason) => {
                PlayerCommandOutcome::Rejected(PlayerCommandReject::SeekTargetUnavailable {
                    reason,
                })
            }
        }
    }
}

/// Recoverable-ошибка, которой зритель узнаёт о мягком отказе.
///
/// Вид ошибки сохранён прежним (`SeekTargetExpired` / `SeekUnavailable`): по нему app
/// выбирает текст временного уведомления (`player_error_message.rs`), журнал браузерного
/// режима — «перемотка не состоялась».
pub(super) fn unavailable_seek_target_error(reason: SeekTargetUnavailableReason) -> PlayerError {
    match reason {
        SeekTargetUnavailableReason::LiveWindowUnavailable => PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "Live source does not currently expose a DVR window",
        ),
        SeekTargetUnavailableReason::OutsideLiveWindow {
            requested_position,
            available_range,
        } => PlayerError::new(
            PlayerErrorKind::SeekTargetExpired,
            format!(
                "Live seek target {} ms is outside latest DVR range {available_range:?}",
                requested_position.as_duration().as_millis(),
            ),
        ),
    }
}

impl PlayerSession {
    /// Мягко отклоняет команду перемотки, цели которой нет в эфире.
    ///
    /// Публикует только recoverable-ошибку (временное уведомление зрителю). Намеренно НЕ
    /// трогает playback state, seek/scrub lifecycle и pending receipts: идущая перемотка
    /// (если была) продолжается, плеер играет/стоит как до команды. `SeekRequested` не
    /// публикуется — команда не принята.
    pub(super) fn refuse_unavailable_seek_target(
        &mut self,
        reason: SeekTargetUnavailableReason,
    ) -> SeekCommandAdmission {
        self.announce_unavailable_seek_target(reason);
        SeekCommandAdmission::Refused(reason)
    }

    /// Только публикует зрителю recoverable-ошибку мягкого отказа.
    ///
    /// Для команд с собственным typed итогом (`EndScrub` → `ScrubCommitOutcome`), которые
    /// сами закрывают свой жест; остальное — через [`Self::refuse_unavailable_seek_target`].
    pub(super) fn announce_unavailable_seek_target(&mut self, reason: SeekTargetUnavailableReason) {
        self.record_recoverable_error(unavailable_seek_target_error(reason));
    }

    /// Возвращает typed rejection, не смешивая две разные гарантии timeline.
    pub(super) fn seek_timeline_admission_error(
        &self,
        target_position: MediaTime,
        admission: SeekTimelineAdmission,
    ) -> Option<PlayerError> {
        match admission {
            SeekTimelineAdmission::PublicSeekableRange => {
                if self.snapshot.timeline.seekable {
                    return None;
                }
                let reason = self
                    .snapshot
                    .timeline
                    .not_seekable_reason
                    .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
                Some(PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    format!("Seek impossible: timeline not seekable ({reason:?})"),
                ))
            }
            SeekTimelineAdmission::ExpiredLiveAvailability => {
                if self.expired_live_resume_target() == Some(target_position) {
                    return None;
                }
                Some(PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    "Expired live recovery target no longer matches authoritative availability",
                ))
            }
            SeekTimelineAdmission::ExpiredSeekContinuation => {
                if self.live_recovery_seek_target() == Some(target_position) {
                    return None;
                }
                Some(PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    "Live continuation target no longer matches the safe live edge",
                ))
            }
        }
    }
}
