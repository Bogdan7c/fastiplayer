//! Остановка показа на конце обрезанных данных (решение владельца 133).
//!
//! Источник, чьи данные обрезаны раньше конца медиа (браузерная вкладка ушла посреди
//! скачивания), сообщает, до какого времени у него есть данные всех дорожек сразу:
//! `DemuxDataEnd::CutShort { playable_end: Some(end) }`. Дальше `end` источник пакетов
//! не выдаёт, но плеер мог прочитать их ВПРОК ещё до обрыва: пока страница жива, а
//! видео ждёт байтов, звук выдаётся на несколько секунд вперёд. Без остановки звук и
//! ползунок шли бы дальше застывшей картинки.
//!
//! Правило: когда часы показа дошли до `end` во время игры или EOF drain-а, показ
//! кончается — как будто медиа кончилось здесь:
//! - уже прочитанные, но не сыгранные пакеты звука и видео выбрасываются (их время —
//!   после конца доступного);
//! - выход звука паркуется тем же путём, что после обычного конца (`Ended`): записанный
//!   впрок PCM не звучит, часы замирают;
//! - позиция = `end` (не больше длительности), состояние — `Ended`.
//!
//! Кадры, уже декодированные за `end`, остаются в очереди показа до следующей
//! перемотки или нового медиа: часы стоят на `end`, и эти кадры не становятся «пора
//! показывать», а освобождение кадров — забота перемотки, которая их и так сбрасывает.
//!
//! Чтобы остановка не опоздала на грубый интервал worker-а (видео уже кончилось, будить
//! некому), план wakeup-а берёт и дедлайн `end` (`tick/cut_short_wakeup.rs`).
//!
//! Без известного `end` (`playable_end: None`, файл, обрезанный посреди байтов) правило
//! не действует: плеер доигрывает прочитанное, а позицию на `Ended` ставят часы показа
//! (`position_at_ended`).

use std::time::{Duration, Instant};

use media_core::DemuxDataEnd;
use tracing::debug;

use crate::PlaybackState;
use crate::session::PlayerSession;

impl PlayerSession {
    /// Конец доступного у обрезанных данных, если источник его знает.
    pub(super) fn cut_short_playable_end(&self) -> Option<Duration> {
        match self.pipeline.demux_data_end()? {
            DemuxDataEnd::CutShort { playable_end } => playable_end,
            DemuxDataEnd::Complete => None,
        }
    }

    /// Можно ли сейчас кончить показ по концу доступного.
    ///
    /// Только во время игры и EOF drain-а: на паузе часы стоят, перемотка и скраб
    /// владеют своим lifecycle-ом (их цель может быть до конца доступного — тогда
    /// показ продолжится оттуда).
    fn cut_short_stop_applies(&self) -> bool {
        matches!(
            self.playback_state(),
            PlaybackState::Playing | PlaybackState::Draining
        ) && !self.has_active_seek_commit()
            && !self.prepared_demux_seek.receipt_pending()
    }

    /// Кончает показ, если часы показа дошли до конца доступного обрезанных данных.
    ///
    /// Возвращает `true`, если показ кончился на этом шаге (правило — в описании модуля).
    pub(in crate::session) fn stop_at_cut_short_playable_end_if_reached(
        &mut self,
        now: Instant,
    ) -> bool {
        if !self.cut_short_stop_applies() {
            return false;
        }
        let Some(playable_end) = self.cut_short_playable_end() else {
            return false;
        };
        let presentation_position = self.presentation_clock_position_at(now);
        if presentation_position < playable_end {
            return false;
        }
        let dropped_audio_packets = self.pipeline.pending_audio_packet_len();
        let dropped_video_packets = self.pipeline.pending_video_packet_len();
        // Прочитанное впрок за концом доступного не играется: его время — после конца.
        self.pipeline.clear_pending_audio_packets();
        self.pipeline.clear_pending_video_packets();
        // Записанный впрок PCM не звучит: выход замирает, как после обычного конца.
        self.park_audio_output_after_eof();
        let end_position = self
            .snapshot
            .duration
            .map_or(playable_end, |duration| playable_end.min(duration));
        debug!(
            playable_end_ms = playable_end.as_secs_f64() * 1000.0,
            presentation_position_ms = presentation_position.as_secs_f64() * 1000.0,
            dropped_audio_packets,
            dropped_video_packets,
            "Presentation reached the end of the cut-short data; entering Ended"
        );
        self.update_current_position(end_position);
        self.set_playback_state(PlaybackState::Ended);
        true
    }

    /// Через сколько настоящего времени часы показа дойдут до конца доступного
    /// обрезанных данных (`None` — правило сейчас не действует).
    pub(in crate::session) fn cut_short_playable_end_wall_delay(
        &self,
        now: Instant,
    ) -> Option<Duration> {
        if !self.cut_short_stop_applies() {
            return None;
        }
        let playable_end = self.cut_short_playable_end()?;
        Some(self.wall_delay_until_media_deadline(now, playable_end))
    }
}
