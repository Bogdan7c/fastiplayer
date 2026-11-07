//! Продолжение воспроизведения после перемотки, чью цель окно эфира «съело» во время посадки.
//!
//! Решение владельца 233: перемотка эфира в место, которого в окне уже нет, — мягкий отказ,
//! плеер продолжает в прежнем состоянии воспроизведения (играл — играет, стоял — стоит).
//! Отказ при ПРИЁМЕ команды ничего не меняет (`seek_admission.rs`). Но если окно уехало,
//! когда посадка уже началась, прежнего места больше нет: декодер и demux уже сброшены под
//! цель. Тогда честное продолжение — тот же механизм, которым ядро уже восстанавливает
//! протухшую паузу на Play (`expired_live_resume_target`): один seek к безопасному краю
//! эфира (live edge в пределах authoritative availability) с ПРЕЖНИМ resume intent-ом.
//!
//! Инварианты:
//! - терминальная очистка протухшей посадки (receipts → Expired ровно один раз,
//!   recoverable `SeekTargetExpired`, никакого `SeekCommitted` для старой цели) остаётся
//!   у владельцев в `seek_completion.rs` / `dynamic_timeline.rs`; здесь только решение
//!   «продолжать ли» и запуск нового seek-а;
//! - продолжение одно: если протух уже сам seek продолжения (`LiveAvailability`), второго
//!   автоматического прыжка нет — прежнее терминальное `Paused` (без бесконечной петли);
//! - восстановление позиции после установки media (staged/installed restore) принадлежит
//!   app: при таком restore автоматического продолжения нет, итог решает его receipt;
//! - во время scrub-жеста продолжения нет: протухший preview закрывает прежняя очистка.

use media_core::MediaTime;
use tracing::{info, warn};

use crate::seek_state::{PlaybackResumeIntent, SeekTargetRetention};
use crate::{PlayerEvent, SeekRequest};

use super::super::PlayerSession;
use super::super::seek_admission::SeekTimelineAdmission;

/// Что делать после терминальной очистки протухшей посадки.
///
/// Решается ДО очистки: она закрывает receipts и restore-состояние, по которым видно,
/// чья это перемотка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum ExpiredLiveSeekContinuation {
    /// Запустить seek к безопасному краю эфира и вернуть прежнее Playing/Paused.
    ResumeAtLiveEdge {
        /// Безопасный край на момент решения (live edge в пределах availability).
        target: MediaTime,
        /// Состояние, в которое посадка продолжения вернёт playback.
        resume_intent: PlaybackResumeIntent,
    },
    /// Автоматического продолжения нет: остаётся прежнее терминальное `Paused`.
    StopAtExpiry,
}

impl PlayerSession {
    /// Безопасный край эфира для автоматического seek-а продолжения.
    ///
    /// Live edge, прижатый к authoritative availability текущего installed port-а; `None`,
    /// если media не live, binding чужой или availability неизвестна.
    pub(in crate::session) fn live_recovery_seek_target(&self) -> Option<MediaTime> {
        let availability_range = self.live_availability_range()?;
        self.snapshot
            .timeline
            .live_edge
            .map(|live_edge| availability_range.clamp(live_edge))
    }

    /// Решает, продолжать ли воспроизведение после протухшей посадки с этими параметрами.
    pub(in crate::session) fn expired_live_seek_continuation(
        &self,
        target_retention: SeekTargetRetention,
        resume_intent: PlaybackResumeIntent,
    ) -> ExpiredLiveSeekContinuation {
        // Протух сам seek продолжения (или recovery паузы на Play): второй прыжок не делаем,
        // иначе медленная сеть дала бы бесконечную петлю «край → протух → край».
        if target_retention == SeekTargetRetention::LiveAvailability {
            return ExpiredLiveSeekContinuation::StopAtExpiry;
        }
        // Восстановление позиции после установки media решает app по своему receipt-у.
        if self.pending_installed_position_restore.is_some()
            || self.installed_staged_position.is_some()
        {
            return ExpiredLiveSeekContinuation::StopAtExpiry;
        }
        // Идёт жест scrub (или его preview-посадка): зритель ещё тянет ползунок, прыжок к
        // краю у него под пальцем был бы чужим решением. Жест закрывает прежняя очистка.
        if self.seek_runtime.simple_scrub_active()
            || self.seek_runtime.active_seek_landing_is_live_scrub()
        {
            return ExpiredLiveSeekContinuation::StopAtExpiry;
        }
        match self.live_recovery_seek_target() {
            Some(target) => ExpiredLiveSeekContinuation::ResumeAtLiveEdge {
                target,
                resume_intent,
            },
            None => ExpiredLiveSeekContinuation::StopAtExpiry,
        }
    }

    /// Выполняет решённое продолжение после того, как протухшая посадка уже закрыта.
    ///
    /// Вызывается строго после терминальной очистки: старые receipts уже разрешены как
    /// истечение, recoverable-ошибка опубликована, playback `Paused`.
    /// Настоящая ошибка запуска seek-а продолжения становится фатальной (как у команды
    /// seek); отказ его admission-а — recoverable, и playback остаётся `Paused`.
    pub(in crate::session) fn continue_after_expired_live_seek(
        &mut self,
        continuation: ExpiredLiveSeekContinuation,
    ) {
        let ExpiredLiveSeekContinuation::ResumeAtLiveEdge {
            target,
            resume_intent,
        } = continuation
        else {
            return;
        };
        info!(
            resume_target_ms = target.as_duration().as_millis(),
            resume_intent = ?resume_intent,
            "Live seek target expired during landing; continuing at the live edge"
        );
        let request = SeekRequest::absolute(target);
        self.push_player_event(PlayerEvent::SeekRequested(request));
        if let Err(error) = self.start_seek_transaction(
            target,
            request.mode,
            resume_intent,
            SeekTimelineAdmission::ExpiredSeekContinuation,
        ) {
            // Настоящая поломка (например, декодер не сбросился) не прячется: как и у
            // обычной команды seek, она фатальна.
            warn!(error = %error, "Live continuation seek failed to start");
            self.mark_fatal_error(error);
        }
        // Отказ admission-а (край успел уехать) уже опубликован recoverable-ошибкой, а
        // playback остался `Paused` терминальной очисткой — второго прыжка нет.
    }
}
