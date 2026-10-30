//! Доигрывание уже прочитанного после фатальной ошибки демуксера (общий бэклог п. 24).
//!
//! Раньше любая ошибка `Demuxer::next_event` сразу переводила плеер в `Failed`, и всё,
//! что уже лежало у плеера (пакеты, кадры, звук в выходе), пропадало: обрезанный
//! файл обрывался раньше, чем кончались его данные. Теперь ошибка демуксера — это
//! «конец данных», который плеер обрабатывает ровно как штатный конец файла (EOF
//! drain, состояние `Draining`, в том числе посреди перемотки — там работает тот же
//! EOF fallback seek-а), доигрывает хвост и только вместо `Ended` публикует ту же
//! самую ошибку (`Failed` + `PlayerEvent::FatalError`).
//!
//! Владелец: `PlayerSession`. Хранится только отложенная ошибка и метка «к какому
//! отрезку воспроизведения она относится» (media instance + seek generation).
//! Любая перемотка, scrub, смена дорожки с перемоткой или замена media начинает
//! новую seek generation — отложенная ошибка к ней уже не относится и забывается.
//!
//! Ошибка НЕ откладывается (поведение как раньше — сразу `Failed`), только если
//! доигрывать нечего: из демуксера в текущем отрезке ничего не дошло (ошибка на
//! самом первом чтении после открытия или перемотки) и в звуковом выходе пусто.
//! Во время перемотки хвост звукового выхода не считается: он принадлежит старому
//! отрезку, и ради него seek ждал бы кадра, которого не будет.

use tracing::{debug, warn};

use crate::{MediaInstanceId, PlayerError};

use super::PlayerSession;

/// К какому отрезку воспроизведения относится отложенная ошибка источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DemuxFailureFence {
    /// Media instance, на котором умер источник (`None` у media без install-протокола).
    media_instance_id: Option<MediaInstanceId>,

    /// Seek generation, внутри которой были прочитаны доигрываемые пакеты.
    seek_generation: u64,
}

/// Ошибка источника, публикация которой отложена до конца доигрывания.
#[derive(Debug, Clone)]
struct DeferredDemuxFailure {
    /// Отрезок воспроизведения, которому принадлежит ошибка.
    fence: DemuxFailureFence,

    /// Исходная ошибка без изменений: её `kind` (`DemuxError`/`NetworkError`) и текст
    /// приложение показывает так же, как при немедленном `Failed`.
    error: PlayerError,
}

/// Session-owned runtime «источник умер, доигрываем прочитанное».
#[derive(Debug, Default)]
pub(super) struct DemuxFailurePlayout {
    /// Не больше одной отложенной ошибки: после неё демуксер больше не читается.
    deferred: Option<DeferredDemuxFailure>,
}

impl DemuxFailurePlayout {
    /// Забывает отложенную ошибку на явной lifecycle-границе.
    pub(super) fn forget(&mut self) {
        self.deferred = None;
    }
}

impl PlayerSession {
    /// Решает судьбу фатальной ошибки `Demuxer::next_event`.
    ///
    /// Если у плеера есть прочитанное и не сыгранное, ошибка откладывается, а session
    /// входит в EOF drain (из любого состояния, где читается демуксер, — так же, как на
    /// штатном EOF): дальше хвост играет путь конца файла, а
    /// [`Self::take_demux_failure_after_playout`] вернёт ошибку, когда хвост кончится.
    /// Если играть нечего, ошибка публикуется сразу, как раньше.
    pub(super) fn handle_demux_read_failure(&mut self, error: PlayerError) {
        if !self.has_read_ahead_to_play_out() {
            debug!(
                error_kind = ?error.kind,
                "Demuxer failure is reported immediately: nothing to play out"
            );
            self.mark_fatal_error(error);
            return;
        }

        let fence = self.current_demux_failure_fence();
        warn!(
            error_kind = ?error.kind,
            error = %error.message,
            pending_audio_packets = self.pipeline.pending_audio_packet_len(),
            pending_video_packets = self.pipeline.pending_video_packet_len(),
            queued_video_frames = self.pipeline.video_present_queue_len(),
            "Demuxer failed mid-file; playing out what was already read before reporting the error"
        );
        self.demux_failure_playout.deferred = Some(DeferredDemuxFailure { fence, error });
        self.enter_eof_drain();
    }

    /// Не даёт снова читать умерший демуксер и продолжает доигрывание.
    ///
    /// Нужен после паузы во время доигрывания: `Pause` выводит session из `Draining`,
    /// а `Play` возвращает в `Playing`, где tick обычно читает демуксер. Повторное
    /// чтение демуксера после фатальной ошибки ничего не гарантирует, поэтому
    /// отложенная ошибка играет роль «конца потока»: session снова входит в drain.
    ///
    /// Возвращает `true`, если демуксер читать нельзя (ошибка этого отрезка отложена).
    pub(super) fn resume_playout_instead_of_reading_failed_demuxer(&mut self) -> bool {
        if !self.demux_failure_is_deferred_for_current_segment() {
            return false;
        }
        if !self.is_eof_draining() {
            debug!("Demuxer already failed in this segment; resuming the play-out drain");
            self.enter_eof_drain();
        }
        true
    }

    /// Забирает отложенную ошибку, когда хвост доигран (вызывает EOF drain вместо `Ended`).
    ///
    /// `None` — штатный конец файла либо ошибка относится к уже покинутому отрезку
    /// (была перемотка): тогда drain заканчивается обычным `Ended`.
    pub(super) fn take_demux_failure_after_playout(&mut self) -> Option<PlayerError> {
        if !self.demux_failure_is_deferred_for_current_segment() {
            return None;
        }
        self.demux_failure_playout
            .deferred
            .take()
            .map(|deferred| deferred.error)
    }

    /// Проверяет, отложена ли ошибка именно для текущего отрезка воспроизведения.
    ///
    /// Устаревшая ошибка (после перемотки/замены media) сразу забывается.
    fn demux_failure_is_deferred_for_current_segment(&mut self) -> bool {
        let current_fence = self.current_demux_failure_fence();
        match &self.demux_failure_playout.deferred {
            Some(deferred) if deferred.fence == current_fence => true,
            Some(_) => {
                debug!("Deferred demuxer failure belongs to a left segment; forgetting it");
                self.demux_failure_playout.forget();
                false
            }
            None => false,
        }
    }

    /// Есть ли у плеера прочитанное и не сыгранное, ради которого стоит доигрывать.
    fn has_read_ahead_to_play_out(&self) -> bool {
        if self.pipeline.has_unplayed_demuxed_media() {
            return true;
        }
        // Во время перемотки/scrub хвост выхода — звук старого отрезка: seek его не
        // покажет, а ждал бы кадра текущего отрезка, которого демуксер уже не даст.
        !self.seek_owns_the_timeline() && self.pipeline.has_unplayed_audio_output_tail()
    }

    /// Идёт ли seek/scrub-транзакция, которая сама владеет позицией и посадкой.
    fn seek_owns_the_timeline(&self) -> bool {
        self.has_active_seek_commit()
            || self.seek_landing_decode_active()
            || self.snapshot.timeline.seeking
            || self.snapshot.timeline.scrubbing
    }

    /// Метка текущего отрезка воспроизведения.
    fn current_demux_failure_fence(&self) -> DemuxFailureFence {
        DemuxFailureFence {
            media_instance_id: self.snapshot.media_instance_id,
            seek_generation: self.pipeline.seek_generation(),
        }
    }
}
