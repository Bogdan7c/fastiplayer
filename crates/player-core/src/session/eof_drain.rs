use std::time::{Duration, Instant};

use media_core::{MediaTime, TimelineNotSeekableReason};
use tracing::{debug, trace, warn};

use crate::pipeline::AudioEofDrainState;
use crate::seek_state::PlaybackResumeIntent;
use crate::{PlaybackState, PlayerError, PlayerErrorKind, PlayerResult};

use super::PlayerSession;

/// Runtime EOF-drain состояния, которым владеет только `PlayerSession`.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct EofDrainRuntime {
    /// Активен ли режим дожатия уже накопленных audio/video хвостов после EOF.
    active: bool,
}

impl EofDrainRuntime {
    /// Возвращает активность EOF-drain без раскрытия внутреннего хранения.
    pub(super) const fn is_active(&self) -> bool {
        self.active
    }

    /// Синхронизирует runtime с high-level playback state.
    pub(super) fn sync_from_playback_state(&mut self, playback_state: PlaybackState) {
        self.active = playback_state == PlaybackState::Draining;
    }
}

/// Точная причина, по которой EOF-drain ещё не может перейти в `Ended`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EofDrainBlocker {
    /// Активный seek commit владеет текущим lifecycle, EOF не должен его перебивать.
    SeekCommit,

    /// Seek near EOF ждёт fallback video frame, которым владеет seek pipeline.
    SeekEofFallbackVideo,

    /// В pipeline ещё лежит preroll fallback frame, который должен быть обработан владельцем seek.
    SeekPrerollFallbackFrame,

    /// Уже считанные video packets ещё не отправлены или не отброшены decoder boundary.
    PendingVideoPackets { queued_packets: usize },

    /// Готовые video frames ещё ждут presentation/release.
    VideoPresentQueue { queued_frames: usize },

    /// Decoder thread ещё не подтвердил завершение ранее отправленных packets.
    VideoDecodeInFlight { in_flight_packets: usize },

    /// Уже считанные audio packets ещё не декодированы и не записаны в output.
    PendingAudioPackets { queued_packets: usize },

    /// Audio output ещё сообщает buffered tail.
    DrainingAudioOutput {
        /// Текущий уровень output buffer-а в миллисекундах.
        buffer_level_ms: f64,

        /// Был ли уже успешно запрошен запуск output stream-а.
        playback_requested: bool,

        /// Сколько времени audio clock не показывал нового progress-а.
        stalled_for: Duration,

        /// Порог, после которого stale output buffer считается зависшим.
        stall_timeout: Duration,
    },
}

impl PlayerSession {
    /// Возвращает `true`, если session дожимает накопленные хвосты после EOF.
    #[must_use]
    pub const fn is_eof_draining(&self) -> bool {
        self.eof_drain.is_active()
    }

    /// Проверяет, должен ли `Play` запускать replay через seek-to-zero.
    pub(super) fn should_replay_from_eof_on_play(&self) -> bool {
        self.is_eof_draining() || self.snapshot.playback_state == PlaybackState::Ended
    }

    /// Переводит session в EOF-drain, сохраняя demuxer открытым для replay/seek.
    pub fn enter_eof_drain(&mut self) {
        let previous_state = self.playback_state();
        debug!(
            previous_state = ?previous_state,
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            pending_audio_packets = self.pipeline.pending_audio_packet_len(),
            pending_video_packets = self.pipeline.pending_video_packet_len(),
            queued_video_frames = self.pipeline.video_present_queue_len(),
            video_decode_in_flight = self.pipeline.video_decode_in_flight_packets(),
            audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
            "Entering EOF drain"
        );
        self.set_playback_state(PlaybackState::Draining);

        if matches!(
            previous_state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.start_eof_audio_tail_if_needed();
        }
    }

    /// Проверяет, должен ли worker продолжать bounded wakeup для EOF-drain state machine.
    #[must_use]
    pub(crate) fn eof_drain_needs_progress(&self) -> bool {
        self.is_eof_draining() && !self.has_active_seek_commit()
    }

    /// Запускает audio output для EOF tail-а, который был накоплен до завершения autoplay preroll.
    pub(crate) fn start_eof_audio_tail_if_needed(&mut self) {
        if !self.is_eof_draining() || self.has_active_seek_commit() {
            return;
        }

        let AudioEofDrainState::DrainingOutput {
            playback_requested: false,
            ..
        } = self.pipeline.audio_eof_drain_state()
        else {
            return;
        };

        if let Some(play_result) = self.pipeline.play_audio_output() {
            if let Err(error) = play_result {
                warn!(error = %error, "Не удалось запустить audio tail после EOF");
                self.set_runtime_error(format!("Audio EOF drain play error: {error}"));
                self.pipeline.clear_audio_output();
                self.pipeline.clear_audio_clock();
                return;
            }

            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }
    }

    /// Завершает EOF-drain, когда audio/video очереди полностью отдали buffered tail.
    pub(crate) fn finish_eof_drain_if_ready(
        &mut self,
        now: Instant,
        audio_stall_timeout: Duration,
    ) -> bool {
        if !self.is_eof_draining() {
            return false;
        }

        if !self.eof_drain_ready_to_end(now, audio_stall_timeout) {
            let blocker = self.eof_drain_blocker(now, audio_stall_timeout);
            trace!(
                blocker = ?blocker,
                playback_state = ?self.playback_state(),
                current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
                duration_ms = ?self
                    .snapshot
                    .duration
                    .map(|duration| duration.as_secs_f64() * 1000.0),
                pending_audio_packets = self.pipeline.pending_audio_packet_len(),
                pending_video_packets = self.pipeline.pending_video_packet_len(),
                queued_video_frames = self.pipeline.video_present_queue_len(),
                video_decode_in_flight = self.pipeline.video_decode_in_flight_packets(),
                seek_commit_active = self.has_active_seek_commit(),
                seek_eof_fallback_video = self.seek_runtime.has_eof_fallback_video_position(),
                audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
                audio_clock_now_ms = self.audio_clock_now().as_secs_f64() * 1000.0,
                audio_clock_stalled_for_ms =
                    self.pipeline.audio_clock_stalled_for(now).as_secs_f64() * 1000.0,
                "EOF drain waiting"
            );
            return false;
        }

        let end_position = self
            .snapshot
            .duration
            .unwrap_or_else(|| self.presentation_clock_position_at(now));
        debug!(
            end_position_ms = end_position.as_secs_f64() * 1000.0,
            previous_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
            "EOF drain finished; entering Ended"
        );
        self.update_current_position(end_position);
        self.set_playback_state(PlaybackState::Ended);
        true
    }

    /// Запускает повторное воспроизведение после штатного EOF через обычный seek pipeline.
    pub(super) fn restart_playback_after_eof(&mut self) -> PlayerResult<()> {
        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Replay невозможен: media pipeline уже закрыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Replay невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        self.start_seek_transaction(
            MediaTime::ZERO,
            crate::SeekMode::Accurate,
            PlaybackResumeIntent::Play,
        )
    }

    /// Проверяет только условия завершения drain; сам state меняет `finish_eof_drain_if_ready`.
    fn eof_drain_ready_to_end(&self, now: Instant, audio_stall_timeout: Duration) -> bool {
        self.eof_drain_blocker(now, audio_stall_timeout).is_none()
    }

    /// Возвращает первый blocker EOF-drain в порядке ownership/lifecycle зависимостей.
    fn eof_drain_blocker(
        &self,
        now: Instant,
        audio_stall_timeout: Duration,
    ) -> Option<EofDrainBlocker> {
        if self.has_active_seek_commit() {
            return Some(EofDrainBlocker::SeekCommit);
        }

        if self.seek_runtime.has_eof_fallback_video_position() {
            return Some(EofDrainBlocker::SeekEofFallbackVideo);
        }

        if self.pipeline.has_seek_preroll_fallback_video_frame() {
            return Some(EofDrainBlocker::SeekPrerollFallbackFrame);
        }

        let pending_video_packets = self.pipeline.pending_video_packet_len();
        if pending_video_packets > 0 {
            return Some(EofDrainBlocker::PendingVideoPackets {
                queued_packets: pending_video_packets,
            });
        }

        let queued_video_frames = self.pipeline.video_present_queue_len();
        if queued_video_frames > 0 {
            return Some(EofDrainBlocker::VideoPresentQueue {
                queued_frames: queued_video_frames,
            });
        }

        let in_flight_packets = self.pipeline.video_decode_in_flight_packets();
        if in_flight_packets > 0 {
            return Some(EofDrainBlocker::VideoDecodeInFlight { in_flight_packets });
        }

        match self.pipeline.audio_eof_drain_state() {
            AudioEofDrainState::PendingPackets { queued_packets } => {
                Some(EofDrainBlocker::PendingAudioPackets { queued_packets })
            }
            AudioEofDrainState::DrainingOutput {
                buffer_level_ms,
                playback_requested,
            } if !self.eof_audio_output_stalled(now, audio_stall_timeout) => {
                Some(EofDrainBlocker::DrainingAudioOutput {
                    buffer_level_ms,
                    playback_requested,
                    stalled_for: self.pipeline.audio_clock_stalled_for(now),
                    stall_timeout: audio_stall_timeout,
                })
            }
            AudioEofDrainState::DrainingOutput { .. }
            | AudioEofDrainState::NoSelectedAudio
            | AudioEofDrainState::NoOutput
            | AudioEofDrainState::DrainedOutput { .. } => None,
        }
    }

    /// Проверяет зависший audio output после EOF без чтения concrete CPAL state-а.
    fn eof_audio_output_stalled(&self, now: Instant, audio_stall_timeout: Duration) -> bool {
        self.pipeline.audio_clock_stalled_for(now) >= audio_stall_timeout
    }
}
