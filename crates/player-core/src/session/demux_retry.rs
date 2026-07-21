use std::time::{Duration, Instant};

use media_core::DemuxRetryHint;

use crate::seek_state::PlaybackResumeIntent;
use crate::{MediaInstanceId, PlaybackState};

use super::PlayerSession;

/// Точная installed-generation, которой принадлежит ожидание следующего demux read-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InstalledDemuxRetryFence {
    /// Exact media instance не позволяет deadline пережить замену source-а.
    media_instance_id: MediaInstanceId,

    /// Seek generation не позволяет читать по deadline от старой demux position.
    seek_generation: u64,
}

/// Одно bounded ожидание readiness для установленного media.
#[derive(Debug, Clone, Copy)]
struct InstalledDemuxRetry {
    /// Lifecycle fence текущего ожидания.
    fence: InstalledDemuxRetryFence,

    /// Самый ранний момент, когда demuxer разрешил повторный read.
    retry_deadline: Instant,

    /// Пользовательское намерение, которое нельзя потерять при реальном underrun-е.
    resume_intent: PlaybackResumeIntent,

    /// Был ли runtime переведён в существующий `Buffering` именно этим ожиданием.
    entered_buffering: bool,
}

/// Session-owned runtime temporary demux readiness.
#[derive(Debug, Default)]
pub(super) struct DemuxRetryRuntime {
    /// Одновременно допустим только один deadline для exact installed generation.
    pending: Option<InstalledDemuxRetry>,
}

impl DemuxRetryRuntime {
    /// Полностью забывает ожидание на явной lifecycle discontinuity.
    fn clear(&mut self) {
        self.pending = None;
    }
}

impl PlayerSession {
    /// Возвращает fence текущей установленной demux position.
    fn current_demux_retry_fence(&self) -> Option<InstalledDemuxRetryFence> {
        Some(InstalledDemuxRetryFence {
            media_instance_id: self.snapshot.media_instance_id?,
            seek_generation: self.pipeline.seek_generation(),
        })
    }

    /// Запоминает earliest retry, не меняя EOF, tracks, timeline или seek state.
    pub(super) fn schedule_installed_demux_retry(
        &mut self,
        observed_at: Instant,
        hint: DemuxRetryHint,
    ) {
        let Some(fence) = self.current_demux_retry_fence() else {
            self.demux_retry.clear();
            return;
        };
        let retry_deadline = observed_at
            .checked_add(hint.retry_after())
            .unwrap_or(observed_at);
        let resume_intent = self
            .demux_retry
            .pending
            .filter(|pending| pending.fence == fence)
            .map_or_else(
                || PlaybackResumeIntent::from_playback_state(self.playback_state()),
                |pending| pending.resume_intent,
            );
        let entered_buffering = self
            .demux_retry
            .pending
            .is_some_and(|pending| pending.fence == fence && pending.entered_buffering);

        self.demux_retry.pending = Some(InstalledDemuxRetry {
            fence,
            retry_deadline,
            resume_intent,
            entered_buffering,
        });
    }

    /// Проверяет, запрещён ли новый demux read до exact-generation deadline-а.
    #[must_use]
    pub(super) fn installed_demux_read_is_blocked(&self, now: Instant) -> bool {
        let Some(current_fence) = self.current_demux_retry_fence() else {
            return false;
        };
        self.demux_retry
            .pending
            .is_some_and(|pending| pending.fence == current_fence && now < pending.retry_deadline)
    }

    /// Возвращает оставшуюся задержку только для актуальной installed generation.
    #[must_use]
    pub(super) fn installed_demux_retry_delay(&self, now: Instant) -> Option<Duration> {
        let current_fence = self.current_demux_retry_fence()?;
        let pending = self.demux_retry.pending?;
        (pending.fence == current_fence)
            .then(|| pending.retry_deadline.saturating_duration_since(now))
    }

    /// Отмечает успешное продолжение demux stream-а и восстанавливает play intent.
    pub(super) fn complete_installed_demux_retry_after_event(&mut self) {
        let Some(current_fence) = self.current_demux_retry_fence() else {
            self.demux_retry.clear();
            return;
        };
        let Some(pending) = self.demux_retry.pending.take() else {
            return;
        };
        if pending.fence != current_fence {
            return;
        }
        if !pending.entered_buffering || self.playback_state() != PlaybackState::Buffering {
            return;
        }

        match pending.resume_intent {
            PlaybackResumeIntent::Play => self.set_playback_state(PlaybackState::Playing),
            PlaybackResumeIntent::Pause => self.set_playback_state(PlaybackState::Paused),
        }
    }

    /// Завершает terminal/non-packet ожидание без восстановления playback intent.
    pub(super) fn clear_installed_demux_retry_after_terminal_event(&mut self) {
        self.demux_retry.clear();
    }

    /// Переводит только реально осушенный playing pipeline в существующий Buffering.
    pub(super) fn enter_buffering_for_demux_underrun_if_needed(&mut self) {
        let Some(current_fence) = self.current_demux_retry_fence() else {
            self.demux_retry.clear();
            return;
        };
        let Some(mut pending) = self.demux_retry.pending else {
            return;
        };
        if pending.fence != current_fence || pending.entered_buffering {
            return;
        }
        if self.playback_state() != PlaybackState::Playing || !self.demux_downstream_is_drained() {
            return;
        }

        pending.entered_buffering = true;
        self.demux_retry.pending = Some(pending);
        self.set_playback_state(PlaybackState::Buffering);
    }

    /// Проверяет отсутствие способной продолжить playback downstream работы.
    fn demux_downstream_is_drained(&self) -> bool {
        let audio_drained = !self.pipeline.has_selected_audio_track()
            || (self.pipeline.pending_audio_packet_is_empty()
                && self
                    .audio_buffer_level_ms()
                    .is_none_or(|buffer_ms| buffer_ms <= f64::EPSILON));
        let video_drained = !self.pipeline.has_selected_video_track()
            || (self.pipeline.pending_video_packet_len() == 0
                && self
                    .pipeline
                    .video_decoder_packet_queue_depth()
                    .unwrap_or(0)
                    == 0
                && self.pipeline.video_decode_in_flight_packets() == 0
                && self.pipeline.video_present_queue_is_empty());

        audio_drained && video_drained
    }

    /// Явно инвалидирует deadline при install/stop/shutdown boundary.
    pub(super) fn clear_installed_demux_retry(&mut self) {
        self.demux_retry.clear();
    }
}
