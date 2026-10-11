//! Кадр-«постер» на паузе: пауза не должна оставлять экран чёрным, если ни одного кадра ещё нет.
//!
//! Откуда берётся проблема. Новое media устанавливается сразу в `Paused` (см.
//! `staged_media_install`), а декодер получает пакеты только в «активных» состояниях
//! (`Playing`/`Buffering`/`Seeking`/`Draining`). Если после установки пользователь (или
//! настройка `player.start_paused`) оставляет паузу, ни один кадр не декодируется, и окно
//! остаётся чёрным, пока не нажмут Play или не перемотают. При возобновлении «из середины»
//! кадр есть только потому, что позиция восстанавливается обычным seek-ом.
//!
//! Решение. Единственный вход в `Paused` по команде/intent-у — `pause()`. После него, если
//! показывать нечего, мы запускаем ровно тот же one-shot seek на текущую позицию, что и
//! обычная перемотка на паузе (`paused_commit_behavior = stay_paused`): он декодирует кадр,
//! публикует его и возвращает плеер в `Paused`. Отдельного «режима декода на паузе» не
//! вводим, чтобы не плодить второй путь декодирования рядом с seek-ом.

use tracing::{debug, warn};

use crate::{PlaybackState, SeekRequest};

use super::PlayerSession;

impl PlayerSession {
    /// Запускает seek на текущую позицию, если после паузы на экране нечего показывать.
    ///
    /// Best-effort: пауза уже успешно применена, поэтому отказ seek-а не превращается в ошибку
    /// команды (пользователь просто не увидит кадр), но и не глотается молча — пишется в лог.
    pub(super) fn request_paused_poster_frame_if_missing(&mut self) {
        if !self.needs_paused_poster_frame() {
            return;
        }
        let position = self.snapshot.current_position;
        debug!(
            position_ms = position.as_millis(),
            "Пауза без кадра на экране: запрашиваем кадр-постер через seek на текущую позицию"
        );
        if let Err(error) = self.seek(SeekRequest::absolute(position.into())) {
            warn!(
                error = %error,
                position_ms = position.as_millis(),
                "Не удалось запросить кадр-постер на паузе"
            );
        }
    }

    /// Нужен ли кадр-постер: пауза, видео выбрано, кадра нет, и seek для его получения возможен.
    fn needs_paused_poster_frame(&self) -> bool {
        // Ended/Failed/Stopped/Idle не являются «паузой по просьбе пользователя»: кадр там
        // либо уже есть, либо media сброшено.
        if self.snapshot.playback_state != PlaybackState::Paused {
            return false;
        }
        if !self.pipeline.has_demuxer()
            || !self.pipeline.has_selected_video_track()
            || self.pipeline.has_present_video_frame()
        {
            return false;
        }
        // Уже идёт seek/scrub — кадр на паузе придёт от него, второй seek только перебил бы его.
        if self.seek_runtime.has_active_commit()
            || self.seek_runtime.seek_landing_active()
            || self.seek_runtime.simple_scrub_active()
        {
            return false;
        }
        // Live не имеет «текущей позиции» для перемотки, а несикабельный источник отказал бы
        // в seek-е пользовательской ошибкой, которой здесь быть не должно.
        self.snapshot.timeline.mode != media_core::TimelineMode::Live
            && self.snapshot.timeline.seekable
    }
}

#[cfg(test)]
impl PlayerSession {
    /// Имитирует «на экране уже есть кадр» для worker-level тестов, которые не владеют pipeline.
    pub(crate) fn show_frame_on_screen_for_tests(&mut self, frame: video_core::DecodedFrame) {
        self.pipeline.set_present_video_frame(frame);
    }
}
