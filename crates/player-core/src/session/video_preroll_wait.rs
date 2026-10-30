//! Ожидание первого видеокадра в preroll-е и честный отказ от него (сессия 03e моста).
//!
//! Preroll (`Buffering`) запускает звук только вместе с первым кадром. Если видео в
//! потоке начинается заметно позже звука (отложенная видеодорожка, вход в живой поток,
//! длинный GOP), demux сначала отдаёт секунды звука. Пока кадра нет, tick читает дальше,
//! откладывая сжатый звук (`tick/audio_read_ahead.rs`), но этот запас ограничен. Если
//! он исчерпан, а кадра так и нет, ждать картинку дальше нельзя: память кончится или
//! чтение снова встанет навсегда. Тогда session отказывается от ожидания кадра для
//! текущего preroll-а: звук стартует первым, а кадры подключатся в свой PTS, когда придут.
//! Правило одно для стартового autoplay и для buffering-resume, где кадры пропали:
//! оба — `Buffering` с голодным по видео preroll-ом (`video_preroll_is_starved`).
//!
//! Отказ — защёлка, а не вывод из текущих очередей: после старта очередь звука
//! начинает убывать, и пересчёт по ней снова «закрыл» бы уже открытые ворота.
//!
//! Второй повод отказаться — видео кончилось без единого кадра (2026-10-30). Короткий
//! файл демуксится до EOF раньше, чем запас звука исчерпан, и старт ждёт кадр уже в
//! `Draining`, где запас не проверяется (он для `Buffering`). Если decoder дожал всё
//! считанное, а кадра нет, картинка не придёт никогда: без отказа это вечный
//! `Draining` с позицией 0 и без звука. Решение владельца: картинки нет — играет звук.

use tracing::info;

use crate::PlaybackState;
use crate::pipeline::PendingAudioBacklog;

use super::PlayerSession;

/// Ждёт ли текущий preroll первый видеокадр.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum VideoPrerollWait {
    /// Обычное правило: звук стартует только вместе с первым кадром.
    #[default]
    AwaitingFirstFrame,

    /// Запас отложенного звука исчерпан без кадра: старт со звуком первым.
    AudioFirstAfterBacklogExhausted,

    /// EOF drain: видео отдало всё считанное без единого кадра, звук стартует один.
    AudioFirstAfterVideoEnded,
}

impl VideoPrerollWait {
    /// Синхронизирует защёлку с playback state.
    ///
    /// Отказ относится только к текущему preroll-у: он живёт в `Buffering` и в
    /// `Draining` (EOF пришёл раньше старта, старт выполнит EOF drain). Любой другой
    /// переход — старт выполнен, seek, пауза, смена media — возвращает обычное правило.
    fn sync_from_playback_state(&mut self, playback_state: PlaybackState) {
        if !matches!(
            playback_state,
            PlaybackState::Buffering | PlaybackState::Draining
        ) {
            *self = Self::AwaitingFirstFrame;
        }
    }
}

/// Итог запроса «стартовать со звуком первым».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioFirstStartDecision {
    /// Отказ от ожидания кадра записан этим вызовом.
    Latched,

    /// Отказ уже был записан раньше для этого preroll-а.
    AlreadyLatched,

    /// Preroll не ждёт кадр (нет видео, кадр уже есть, это не голодный `Buffering`
    /// и не старт, отложенный в EOF drain): no-op.
    NotWaitingForVideo,

    /// Старт ждёт кадр, и он ещё может прийти (decoder не дожал считанное): no-op.
    VideoFrameMayStillArrive,
}

impl PlayerSession {
    /// `true`, если текущий preroll отказался ждать первый кадр (звук стартует первым).
    #[must_use]
    pub(super) fn video_preroll_starts_audio_first(&self) -> bool {
        self.video_preroll_wait != VideoPrerollWait::AwaitingFirstFrame
    }

    /// Отказывается от ожидания первого кадра: запас отложенного звука исчерпан.
    ///
    /// Вызывает tick, когда его read-ahead policy доказала, что дальше читать без
    /// старта нельзя. Сам старт выполняет обычный `finish_autoplay_preroll_if_ready`:
    /// здесь только открываются видео-ворота preroll-а; audio gate, retry fence и
    /// очереди не трогаются.
    pub(crate) fn start_audio_first_after_audio_backlog_exhausted(
        &mut self,
        audio_backlog: PendingAudioBacklog,
    ) -> AudioFirstStartDecision {
        if self.video_preroll_starts_audio_first() {
            return AudioFirstStartDecision::AlreadyLatched;
        }
        if !self.video_preroll_is_starved() {
            return AudioFirstStartDecision::NotWaitingForVideo;
        }
        self.video_preroll_wait = VideoPrerollWait::AudioFirstAfterBacklogExhausted;
        info!(
            pending_audio_packets = audio_backlog.packets,
            pending_audio_bytes = audio_backlog.encoded_bytes,
            pending_audio_span_ms = audio_backlog.media_span.as_millis(),
            audio_buffer_ms = ?self.audio_buffer_level_ms(),
            "No video frame within the startup audio backlog; audio starts first, \
             video joins at its PTS"
        );
        AudioFirstStartDecision::Latched
    }

    /// Отказывается от ожидания первого кадра: в EOF drain видео больше нечего отдать.
    ///
    /// Вызывает tick на каждом шаге. Срабатывает только для старта, отложенного в EOF
    /// drain (короткий файл), когда кадра нет, а уже считанное видео исчерпано
    /// (`eof_video_tail_is_exhausted`: ни packets, ни decode in flight, decoder дожал
    /// DPB текущего поколения). Пока decoder ещё может отдать кадр, звук ждёт его, как
    /// и раньше. Сам старт выполняет обычный `finish_autoplay_preroll_if_ready`.
    ///
    /// Почему `Drained` достаточно: backend-ы показывают player-у `Drained`, только когда
    /// канал кадров пуст (`VideoDecoderEndOfStreamDrainState::visible_to_player`), а tick
    /// забирает принятые кадры в очередь показа до этой проверки. Видеодорожку без
    /// decoder-а pipeline не выбирает (сначала смена backend-а), поэтому «decoder ещё
    /// ставится» сюда не попадает; упавший decoder кадров уже не даст. Смена backend-а
    /// при живом decoder-е идёт через seek, а он сбрасывает и старт, и эту защёлку.
    pub(crate) fn start_audio_first_when_video_ended_in_eof_drain(
        &mut self,
    ) -> AudioFirstStartDecision {
        if self.video_preroll_starts_audio_first() {
            return AudioFirstStartDecision::AlreadyLatched;
        }
        if !self.startup_autoplay_waits_for_first_video_frame_in_eof_drain() {
            return AudioFirstStartDecision::NotWaitingForVideo;
        }
        if !self.eof_video_tail_is_exhausted() {
            return AudioFirstStartDecision::VideoFrameMayStillArrive;
        }
        self.video_preroll_wait = VideoPrerollWait::AudioFirstAfterVideoEnded;
        info!(
            pending_audio_packets = self.pipeline.pending_audio_packet_len(),
            audio_buffer_ms = ?self.audio_buffer_level_ms(),
            "Video ended without a single frame before the start; audio plays alone"
        );
        AudioFirstStartDecision::Latched
    }

    /// Возвращает обычное правило ожидания кадра (новый preroll, новое media).
    pub(super) fn reset_video_preroll_wait(&mut self) {
        self.video_preroll_wait = VideoPrerollWait::AwaitingFirstFrame;
    }

    /// Вызывается из единственной точки смены playback state (`set_playback_state`).
    pub(super) fn sync_video_preroll_wait_from_playback_state(
        &mut self,
        playback_state: PlaybackState,
    ) {
        self.video_preroll_wait
            .sync_from_playback_state(playback_state);
    }
}
