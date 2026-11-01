use std::time::{Duration, Instant};

use media_core::{DemuxDataEnd, MediaTime, TimelineNotSeekableReason};
use tracing::{debug, trace, warn};

use crate::pipeline::AudioEofDrainState;
use crate::seek_state::PlaybackResumeIntent;
use crate::{PlaybackState, PlayerError, PlayerErrorKind, PlayerResult};

use super::PlayerSession;
use super::seek_admission::SeekTimelineAdmission;

// Остановка показа на конце доступного у обрезанных данных (решение 133).
mod cut_short_stop;

/// Runtime EOF-drain состояния, которым владеет только `PlayerSession`.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct EofDrainRuntime {
    /// Активен ли режим дожатия уже накопленных audio/video хвостов после EOF.
    active: bool,

    /// Drain начался из идущего воспроизведения (`Playing`/`Buffering`), а не из паузы:
    /// хвост ролика сейчас играет, и намерение «играть» уже выполняется.
    continues_started_playback: bool,
}

impl EofDrainRuntime {
    /// Возвращает активность EOF-drain без раскрытия внутреннего хранения.
    pub(super) const fn is_active(&self) -> bool {
        self.active
    }

    /// Возвращает, что активный drain доигрывает начатое воспроизведение.
    pub(super) const fn continues_started_playback(&self) -> bool {
        self.active && self.continues_started_playback
    }

    /// Запоминает, из какого состояния начался drain, до перехода в `Draining`.
    pub(super) fn record_drain_origin(&mut self, previous_state: PlaybackState) {
        // Повторный вход в drain из `Draining` сохраняет исходное происхождение.
        if previous_state == PlaybackState::Draining {
            return;
        }
        self.continues_started_playback = matches!(
            previous_state,
            PlaybackState::Playing | PlaybackState::Buffering
        );
    }

    /// Синхронизирует runtime с high-level playback state.
    pub(super) fn sync_from_playback_state(&mut self, playback_state: PlaybackState) {
        self.active = playback_state == PlaybackState::Draining;
        if !self.active {
            self.continues_started_playback = false;
        }
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

    /// Decoder backend ещё дожимает codec tail/DPB frames.
    DecoderEndOfStreamDrain,
}

impl EofDrainBlocker {
    /// Возвращает `true`, если drain ждёт ещё не показанный хвост видео.
    ///
    /// Только такие blocker-ы могут потребовать передачи времени после конца звука:
    /// seek-blocker-ы владеет seek pipeline, а audio-blocker-ы означают, что звук ещё играет.
    const fn waits_for_video_tail(self) -> bool {
        matches!(
            self,
            Self::PendingVideoPackets { .. }
                | Self::VideoPresentQueue { .. }
                | Self::VideoDecodeInFlight { .. }
                | Self::DecoderEndOfStreamDrain
        )
    }
}

impl PlayerSession {
    /// Возвращает `true`, если session дожимает накопленные хвосты после EOF.
    #[must_use]
    pub const fn is_eof_draining(&self) -> bool {
        self.eof_drain.is_active()
    }

    /// Demux дошёл до EOF, и все уже прочитанные audio packets переданы decoder-у:
    /// больше звука для seek preroll-а не появится.
    pub(super) fn seek_audio_tail_exhausted_at_eof(&self) -> bool {
        self.is_eof_draining() && self.pipeline.pending_audio_packet_len() == 0
    }

    /// Проверяет, должен ли `Play` запускать replay через seek-to-zero.
    ///
    /// Drain первого просмотра (EOF пришёл раньше первого autoplay-старта, короткий файл) —
    /// не «конец просмотренного файла»: там Play присоединяется к старту, а не повторяет файл.
    pub(super) fn should_replay_from_eof_on_play(&self) -> bool {
        let drain_after_started_playback =
            self.is_eof_draining() && !self.eof_drain_belongs_to_startup_playthrough();
        drain_after_started_playback || self.snapshot.playback_state == PlaybackState::Ended
    }

    /// Переводит session в EOF-drain, сохраняя demuxer открытым для replay/seek.
    pub fn enter_eof_drain(&mut self) {
        let previous_state = self.playback_state();
        // На EOF следующего recovery keyframe уже не будет. Сохранённый backlog
        // вместе со staged continuation целиком проходит обычный drain.
        let recovery_report = self.pipeline.finish_video_backlog_recovery_scan_at_eof();
        if recovery_report.restored_staged_packets > 0 {
            debug!(
                restored_staged_video_packets = recovery_report.restored_staged_packets,
                pending_video_packets = self.pipeline.pending_video_packet_len(),
                "EOF restores unfinished video recovery continuation"
            );
        }
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
        // `Buffering` уже начатого playback (не первый старт) заморозил часы и ждал
        // источник. После EOF ждать больше нечего: время обязано пойти снова.
        let resumes_frozen_playback =
            previous_state == PlaybackState::Buffering && !self.startup_autoplay_preroll_pending();
        self.eof_drain.record_drain_origin(previous_state);
        self.set_playback_state(PlaybackState::Draining);
        self.start_video_decoder_eof_drain_if_ready();

        if matches!(
            previous_state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.start_eof_audio_tail_if_needed();
        }
        if resumes_frozen_playback {
            // Звук размораживает хвост сам (`start_eof_audio_tail_if_needed`); без звука
            // время ведёт monotonic clock, а freeze его снял — без нового якоря кадры
            // хвоста не стали бы due никогда (вечный `Draining`, сессия 03e моста).
            self.anchor_monotonic_media_clock_if_needed(Instant::now());
        }
    }

    /// Проверяет, должен ли worker продолжать bounded wakeup для EOF-drain state machine.
    #[must_use]
    pub(crate) fn eof_drain_needs_progress(&self) -> bool {
        self.is_eof_draining() && !self.has_active_seek_commit()
    }

    /// Запускает audio output для EOF tail-а, который был накоплен до завершения autoplay preroll.
    ///
    /// Если EOF наступил раньше первого старта (короткий файл), звук сам не стартует:
    /// решают ворота старта (`complete_startup_autoplay_during_eof_drain_if_ready` —
    /// первый кадр или отказ от него, звук до preroll target или ничего больше не
    /// придёт), и они вызывают этот же метод уже после смены стадии старта. Так звук
    /// не уходит раньше картинки и не стартует на тонком буфере в обход ворот.
    ///
    /// Хвост запускается, как только в output-е есть незапущенный звук, даже если в
    /// очереди ещё лежит сжатый: этот остаток декодируется только когда уровень output-а
    /// опустится ниже предела, а без `play` он не опускается никогда. Раньше здесь ждали
    /// пустую очередь (`DrainingOutput`) — и получали вечный `Draining` после старта в
    /// EOF drain с output-ом на потолке и после заморозки ожиданием источника.
    pub(crate) fn start_eof_audio_tail_if_needed(&mut self) {
        if !self.is_eof_draining()
            || self.has_active_seek_commit()
            || self.startup_autoplay_pending_during_eof_drain()
            || !self.pipeline.audio_output_has_unstarted_tail()
        {
            return;
        }

        if let Some(play_result) = self.play_audio_output_with_resume_event() {
            if let Err(error) = play_result {
                warn!(error = %error, "Failed to start audio tail after EOF");
                self.set_runtime_error(format!("Audio EOF drain play error: {error}"));
                self.pipeline.clear_audio_output();
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

        self.start_video_decoder_eof_drain_if_ready();
        if !self.is_eof_draining() {
            return false;
        }

        if let Err(error) = self.flush_audio_tempo_processor_for_eof() {
            warn!(error = %error, "Failed to drain audio tempo tail after EOF");
            // После committed tempo processing backend уже нельзя откатить к
            // direct PCM. Потеря EOF tail-а — fatal session failure, а не
            // разрешение молча завершить оставшееся видео без audio.
            self.mark_fatal_error(error);
            return false;
        }

        if let Some(blocker) = self.eof_drain_blocker(now, audio_stall_timeout) {
            if blocker.waits_for_video_tail() {
                // Звук мог уже доиграть раньше видео: тогда хвост видео дальше идёт по
                // monotonic clock, иначе его кадры никогда не станут due (`Draining` навсегда).
                self.hand_over_presentation_clock_for_video_tail(now);
            }
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
                presentation_clock_source = ?self.pipeline.presentation_clock_source(),
                "EOF drain waiting"
            );
            return false;
        }

        // Хвост доиграл после смерти демуксера: вместо `Ended` публикуется отложенная
        // ошибка. Позиция остаётся там, где кончились данные, а не прыгает к duration.
        if let Some(source_failure) = self.take_demux_failure_after_playout() {
            debug!(
                position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
                error_kind = ?source_failure.kind,
                "Read-ahead played out after a demuxer failure; reporting the error"
            );
            self.park_audio_output_after_eof();
            self.mark_fatal_error(source_failure);
            return true;
        }

        let data_end = self.pipeline.demux_data_end();
        let end_position = self.position_at_ended(now, data_end);
        debug!(
            end_position_ms = end_position.as_secs_f64() * 1000.0,
            previous_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            ?data_end,
            audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
            "EOF drain finished; entering Ended"
        );
        self.update_current_position(end_position);
        self.park_audio_output_after_eof();
        self.set_playback_state(PlaybackState::Ended);
        true
    }

    /// Где встаёт позиция на `Ended`.
    ///
    /// - конец данных настоящий (обычный файл, честный конец потока) или demuxer-а
    ///   уже нет — позиция в длительности, как всегда: последний кадр может кончаться
    ///   чуть раньше заявленной длительности, а ползунок должен дойти до конца;
    ///   без длительности — там, где встали часы показа;
    /// - источник обрезал данные раньше конца медиа (вкладка браузера ушла) — позиция
    ///   там, где встали часы показа: досмотрено только полученное, и прыжок ползунка
    ///   в длительность фильма соврал бы зрителю (решение владельца 133). Известный
    ///   конец доступного и длительность — верхние границы: дальше них показ не шёл
    ///   (обычно до конца доступного показ останавливает `cut_short_stop`, сюда
    ///   доходит хвост, кончившийся чуть раньше него).
    fn position_at_ended(&self, now: Instant, data_end: Option<DemuxDataEnd>) -> Duration {
        let stopped_presentation = self.presentation_clock_position_at(now);
        match data_end {
            Some(DemuxDataEnd::CutShort { playable_end }) => [playable_end, self.snapshot.duration]
                .into_iter()
                .flatten()
                .fold(stopped_presentation, Duration::min),
            Some(DemuxDataEnd::Complete) | None => {
                self.snapshot.duration.unwrap_or(stopped_presentation)
            }
        }
    }

    /// Ставит audio output на backend-паузу, когда хвост звука уже доигран до конца.
    ///
    /// Почему здесь: после `Ended` играть больше нечему, но cpal stream продолжал бы
    /// крутить data callback с пустым ring-ом (каждые ~10-30 мс), копя ложные
    /// «underrun-ы» и будя CPU. Пауза берёт тот же путь, что и пользовательский
    /// `Pause` (`pause_audio_output_and_capture_clock`), поэтому clock замораживается
    /// атомарно, а `Play`/seek после `Ended` возобновляют вывод обычным resume-путём
    /// (флаг play-request сброшен).
    ///
    /// Вызывается строго после того, как `eof_drain_blocker` вернул `None`: хвост
    /// (включая submitted DAC tail) уже доигран, поэтому пауза его не обрезает. Ошибка паузы не фатальна: звука
    /// больше не будет, а fatal-ошибка на корректном конце файла была бы хуже
    /// лишних callback-ов; причина остаётся в логе.
    fn park_audio_output_after_eof(&mut self) {
        if let Some(Err(error)) = self.pipeline.pause_audio_output_and_capture_clock() {
            warn!(error = %error, "Could not pause audio output after EOF");
        }
    }

    /// Запускает decoder-side EOF/DPB drain без seek flush и без generation advance.
    ///
    /// H.264 B-frames могут остаться в DPB только после того, как все уже
    /// считанные packets дошли до decoder-а. Поэтому drain стартует не в момент
    /// container EOF, а когда player-side pending queue и decoder in-flight уже
    /// пусты.
    fn start_video_decoder_eof_drain_if_ready(&mut self) {
        if !self.video_decoder_eof_drain_is_required()
            || self.video_decoder_eof_drain_start_blocker().is_some()
        {
            return;
        }

        let generation = self.pipeline.seek_generation();
        match self.pipeline.video_decoder_end_of_stream_drain_state() {
            video_core::VideoDecoderEndOfStreamDrainState::Draining {
                generation: active_generation,
            }
            | video_core::VideoDecoderEndOfStreamDrainState::Drained {
                generation: active_generation,
            } if active_generation == generation => return,
            video_core::VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                self.fail_video_decoder_eof_drain(error);
                return;
            }
            video_core::VideoDecoderEndOfStreamDrainState::Idle
            | video_core::VideoDecoderEndOfStreamDrainState::Draining { .. }
            | video_core::VideoDecoderEndOfStreamDrainState::Drained { .. } => {}
        }

        self.begin_video_decoder_eof_drain();
    }

    /// Проверяет, нужен ли selected video track-у decoder-side EOF drain.
    fn video_decoder_eof_drain_is_required(&self) -> bool {
        self.pipeline.has_selected_video_track() && self.pipeline.has_active_video_decoder()
    }

    /// Возвращает blocker, который не даёт безопасно начать decoder-side EOF drain.
    fn video_decoder_eof_drain_start_blocker(&self) -> Option<EofDrainBlocker> {
        let pending_video_packets = self.pipeline.pending_video_packet_len();
        if pending_video_packets > 0 {
            return Some(EofDrainBlocker::PendingVideoPackets {
                queued_packets: pending_video_packets,
            });
        }

        let in_flight_packets = self.pipeline.video_decode_in_flight_packets();
        if in_flight_packets > 0 {
            return Some(EofDrainBlocker::VideoDecodeInFlight { in_flight_packets });
        }

        None
    }

    /// Переводит fatal EOF-drain state в player error без смешивания с seek flush.
    fn fail_video_decoder_eof_drain(&mut self, error: video_core::DecodeThreadError) {
        warn!(
            error = %error,
            generation = self.pipeline.seek_generation(),
            "Decoder EOF drain failed"
        );
        self.mark_fatal_error(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Video decoder EOF drain failed: {error}"),
        ));
    }

    /// Запускает decoder-side EOF/DPB drain без seek flush и без generation advance.
    fn begin_video_decoder_eof_drain(&mut self) {
        let generation = self.pipeline.seek_generation();
        match self
            .pipeline
            .begin_video_decoder_end_of_stream_drain(generation)
        {
            video_core::VideoDecoderEndOfStreamDrainResult::AbsentDecoder => {}
            video_core::VideoDecoderEndOfStreamDrainResult::Started(state)
            | video_core::VideoDecoderEndOfStreamDrainResult::Unchanged(state) => {
                debug!(
                    state = ?state,
                    generation,
                    "Decoder EOF drain state updated"
                );
            }
            video_core::VideoDecoderEndOfStreamDrainResult::Backpressure(reason) => {
                warn!(
                    reason = %reason,
                    generation,
                    "Decoder EOF drain command hit control-channel backpressure"
                );
            }
            video_core::VideoDecoderEndOfStreamDrainResult::Fatal(error) => {
                self.fail_video_decoder_eof_drain(error);
            }
        }
    }

    /// Запускает повторное воспроизведение после штатного EOF через обычный seek pipeline.
    pub(super) fn restart_playback_after_eof(&mut self) -> PlayerResult<()> {
        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Replay is not possible: the media pipeline is closed.",
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
                format!("Replay impossible: timeline not seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let source_start = self.absolute_position_for_relative(MediaTime::ZERO);
        self.start_seek_transaction(
            source_start,
            crate::SeekMode::Accurate,
            PlaybackResumeIntent::Play,
            SeekTimelineAdmission::PublicSeekableRange,
        )
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

        if let Some(video_tail_blocker) = self.eof_video_tail_blocker() {
            return Some(video_tail_blocker);
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

    /// `true`, если в EOF drain уже считанное видео больше не может дать ни одного кадра.
    ///
    /// Те же условия, по которым EOF drain считает хвост видео доигранным: нет ни
    /// ожидающих packets, ни кадров в очереди, ни decode in flight, а decoder дожал
    /// DPB текущего поколения. Intent-метод для отказа от ожидания первого кадра
    /// (`session/video_preroll_wait.rs`): после этого картинка уже не придёт.
    pub(super) fn eof_video_tail_is_exhausted(&self) -> bool {
        self.is_eof_draining() && self.eof_video_tail_blocker().is_none()
    }

    /// Первый blocker уже считанного видео: packets, кадры, decode in flight, DPB drain.
    fn eof_video_tail_blocker(&self) -> Option<EofDrainBlocker> {
        let pending_video_packets = self.pipeline.pending_video_packet_len();
        if pending_video_packets > 0 {
            return Some(EofDrainBlocker::PendingVideoPackets {
                queued_packets: pending_video_packets,
            });
        }

        let queued_video_frames = self.pipeline.video_present_queue_len();
        if queued_video_frames > 0 && !self.queued_video_frames_unreachable_after_audio_end() {
            return Some(EofDrainBlocker::VideoPresentQueue {
                queued_frames: queued_video_frames,
            });
        }

        let in_flight_packets = self.pipeline.video_decode_in_flight_packets();
        if in_flight_packets > 0 {
            return Some(EofDrainBlocker::VideoDecodeInFlight { in_flight_packets });
        }

        if self.video_decoder_eof_drain_is_required() {
            match self.pipeline.video_decoder_end_of_stream_drain_state() {
                video_core::VideoDecoderEndOfStreamDrainState::Idle
                | video_core::VideoDecoderEndOfStreamDrainState::Draining { .. }
                | video_core::VideoDecoderEndOfStreamDrainState::Fatal { .. } => {
                    return Some(EofDrainBlocker::DecoderEndOfStreamDrain);
                }
                video_core::VideoDecoderEndOfStreamDrainState::Drained { generation }
                    if generation != self.pipeline.seek_generation() =>
                {
                    return Some(EofDrainBlocker::DecoderEndOfStreamDrain);
                }
                video_core::VideoDecoderEndOfStreamDrainState::Drained { .. } => {}
            }
        }

        None
    }

    /// Звук доигран до конца, а очередь держит только кадры за концом static media.
    ///
    /// Кадр с PTS за duration (H.264 с B-кадрами: последний кадр 20.04 с у 20 с ролика)
    /// лежит вне публичной timeline. С передачей времени после конца звука он стал бы due
    /// по monotonic clock, но ждать его незачем: позиция и так клампится к duration, а
    /// битый PTS далеко за концом задержал бы `Ended` на произвольное время. Поэтому такие
    /// кадры не держат drain, и seek/reopen ровно на конец ролика сразу доходит до `Ended`.
    fn queued_video_frames_unreachable_after_audio_end(&self) -> bool {
        let audio_finished = matches!(
            self.pipeline.audio_eof_drain_state(),
            AudioEofDrainState::DrainedOutput { .. }
        );
        let Some(public_duration) = self.snapshot.duration else {
            return false;
        };
        if !audio_finished || self.snapshot.timeline.mode != media_core::TimelineMode::Static {
            return false;
        }
        let media_end =
            self.absolute_position_for_relative(MediaTime::from_duration(public_duration));
        self.pipeline
            .front_queued_video_frame()
            .is_some_and(|frame| MediaTime::from_duration(frame.pts) >= media_end)
    }

    /// Проверяет зависший audio output после EOF без чтения concrete CPAL state-а.
    fn eof_audio_output_stalled(&self, now: Instant, audio_stall_timeout: Duration) -> bool {
        self.pipeline.audio_clock_stalled_for(now) >= audio_stall_timeout
    }
}
