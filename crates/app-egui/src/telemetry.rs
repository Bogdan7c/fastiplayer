/// Модуль shell-телеметрии для UI и диагностики render loop.
///
/// Содержит атомарные счётчики для:
/// - swapchain/render событий вокруг surface present
/// - user-visible video playback drops
/// - ожидаемых seek discard событий
/// - повторного показа уже известного video frame
/// - текущего FPS (кадры в секунду)
/// - времени последнего кадра (frame time в миллисекундах)
///
/// Все счётчики реализованы через AtomicU64, что позволяет безопасно
/// обновлять их из разных потоков (например, из audio callback).
///
/// Почему AtomicU64, а не Mutex<u64>:
/// - счётчики обновляются каждый кадр, contention был бы высоким
/// - AtomicU64 не блокирует, не может вызвать deadlock
/// - точность не критична — допустимы редкие lost updates
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use media_core::TrackKind;

/// Причина, по которой video frame был удалён из playback pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDropReason {
    /// Кадр устарел относительно audio-master media time.
    Late,

    /// Кадр вытеснен из-за переполнения очереди presentation.
    QueueOverflow,

    /// Кадр пришёл после пользовательской паузы и не должен менять картинку.
    Paused,

    /// Playback не смог выдать новый кадр, потому что decoder starving.
    DecoderStarvation,

    /// Остальные typed причины доступны подробно в `PlayerSnapshot::diagnostics`.
    Other,
}

/// Причина ожидаемого удаления кадра вокруг seek/generation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekDiscardReason {
    /// Старый кадр pre-roll нужен decoder-у, но не должен стать видимым output.
    SeekPreroll,

    /// Кадр относится к прошлой render/playback generation после seek/open.
    StaleGeneration,
}

/// Класс события удаления video frame для пользовательской телеметрии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFrameTelemetryEvent {
    /// Playback drop, который относится к video timeline, а не к swapchain.
    PlaybackDrop(VideoDropReason),

    /// Ожидаемый discard вокруг seek, который не является проблемой smooth playback.
    SeekDiscard(SeekDiscardReason),
}

/// Глобальные счётчики телеметрии.
///
/// Синглтон через OnceLock для ленивой инициализации.
/// Доступен из любого места приложения без передачи ссылок.
pub struct Telemetry {
    /// Количество render frames, успешно отправленных в swapchain surface.
    frames_presented_to_surface: AtomicU64,

    /// Количество render frames, которые swapchain/surface не смог принять.
    surface_dropped_frames: AtomicU64,

    /// Текущий FPS, обновляется каждые 500 мс скользящим окном.
    current_fps: AtomicU64,

    /// Время рендеринга последнего кадра в миллисекундах.
    last_frame_time_ms: AtomicU64,

    /// Внутреннее состояние для расчёта FPS.
    fps_tracker: std::sync::Mutex<FpsTracker>,

    /// Общее количество прочитанных packets из demuxer.
    packets_read: AtomicU64,

    /// Количество video packets.
    video_packets: AtomicU64,

    /// Количество audio packets.
    audio_packets: AtomicU64,

    /// Количество декодированных видеокадров.
    video_frames_decoded: AtomicU64,

    /// Количество новых video frames, принятых playback timeline как presented.
    video_frames_presented: AtomicU64,

    /// Количество playback-class drops, включая pause-only drops для legacy UI.
    video_frames_dropped: AtomicU64,

    /// Количество видеокадров, пропущенных из-за опоздания к media time.
    video_late_drops: AtomicU64,

    /// Количество видеокадров, вытесненных из-за переполнения очереди.
    video_queue_drops: AtomicU64,

    /// Количество видеокадров, отброшенных после pause без видимой потери playback.
    video_pause_drops: AtomicU64,

    /// Количество playback drops, связанных с decoder starvation.
    video_decoder_starvation: AtomicU64,

    /// Количество видеокадров с typed причинами, которые UI не раскладывает отдельно.
    video_other_drops: AtomicU64,

    /// Общее количество кадров, ожидаемо отброшенных вокруг seek.
    seek_discarded_frames: AtomicU64,

    /// Количество pre-roll кадров, ожидаемо скрытых во время seek.
    seek_preroll_discarded: AtomicU64,

    /// Количество кадров старой generation, скрытых после seek/open.
    stale_generation_discarded: AtomicU64,

    /// Количество render ticks, где был повторно показан предыдущий video frame.
    repeated_frames: AtomicU64,
}

/// Внутренний трекер для расчёта FPS через скользящее окно.
///
/// Считает кадры за последние 500 мс и выдаёт усреднённый FPS.
struct FpsTracker {
    /// Время последнего обновления FPS.
    last_update: Instant,

    /// Количество кадров с момента последнего обновления.
    frame_count: u64,
}

impl Telemetry {
    /// Создаёт новый экземпляр телеметрии с нулевыми счётчиками.
    pub fn new() -> Self {
        Self {
            frames_presented_to_surface: AtomicU64::new(0),
            surface_dropped_frames: AtomicU64::new(0),
            current_fps: AtomicU64::new(0),
            last_frame_time_ms: AtomicU64::new(0),
            fps_tracker: std::sync::Mutex::new(FpsTracker {
                last_update: Instant::now(),
                frame_count: 0,
            }),
            packets_read: AtomicU64::new(0),
            video_packets: AtomicU64::new(0),
            audio_packets: AtomicU64::new(0),
            video_frames_decoded: AtomicU64::new(0),
            video_frames_presented: AtomicU64::new(0),
            video_frames_dropped: AtomicU64::new(0),
            video_late_drops: AtomicU64::new(0),
            video_queue_drops: AtomicU64::new(0),
            video_pause_drops: AtomicU64::new(0),
            video_decoder_starvation: AtomicU64::new(0),
            video_other_drops: AtomicU64::new(0),
            seek_discarded_frames: AtomicU64::new(0),
            seek_preroll_discarded: AtomicU64::new(0),
            stale_generation_discarded: AtomicU64::new(0),
            repeated_frames: AtomicU64::new(0),
        }
    }

    /// Инкрементирует счётчик кадров, отправленных в swapchain surface.
    ///
    /// Вызывается каждый раз, когда кадр успешно отправлен на экран
    /// через `surface_texture.present()`.
    #[inline]
    pub fn record_frame_presented_to_surface(&self) {
        self.frames_presented_to_surface
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Legacy alias для старого render loop API.
    #[inline]
    #[allow(dead_code)]
    pub fn record_presented_frame(&self) {
        self.record_frame_presented_to_surface();
    }

    /// Инкрементирует счётчик кадров, потерянных на swapchain/surface boundary.
    ///
    /// Этот counter не описывает video playback timeline и не должен участвовать
    /// в claim-е `0 visible playback drops`.
    #[inline]
    pub fn record_surface_dropped_frame(&self) {
        self.surface_dropped_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// Legacy alias для старого render loop API.
    #[inline]
    #[allow(dead_code)]
    pub fn record_dropped_frame(&self) {
        self.record_surface_dropped_frame();
    }

    /// Обновляет FPS на основе прошедшего времени кадра.
    ///
    /// Пересчитывает FPS каждые 500 мс, используя количество кадров
    /// за этот период. Это даёт более стабильное значение, чем
    /// мгновенный 1/delta_time.
    pub fn update_fps(&self, delta_time_ms: f64) {
        let mut tracker = self.fps_tracker.lock().expect("fps tracker mutex poisoned");

        tracker.frame_count += 1;

        let elapsed = tracker.last_update.elapsed();
        if elapsed.as_millis() >= 500 {
            let fps = (tracker.frame_count as f64) / (elapsed.as_secs_f64());
            self.current_fps.store(fps as u64, Ordering::Relaxed);
            tracker.frame_count = 0;
            tracker.last_update = Instant::now();
        }

        // Сохраняем время кадра в миллисекундах
        self.last_frame_time_ms
            .store(delta_time_ms as u64, Ordering::Relaxed);
    }

    /// Возвращает текущий FPS (обновляется каждые 500 мс).
    #[inline]
    pub fn current_fps(&self) -> u64 {
        self.current_fps.load(Ordering::Relaxed)
    }

    /// Возвращает время последнего кадра в миллисекундах.
    #[inline]
    pub fn last_frame_time_ms(&self) -> u64 {
        self.last_frame_time_ms.load(Ordering::Relaxed)
    }

    /// Возвращает количество render frames, успешно отправленных в surface.
    #[inline]
    pub fn frames_presented_to_surface(&self) -> u64 {
        self.frames_presented_to_surface.load(Ordering::Relaxed)
    }

    /// Legacy getter для старого имени surface-present counter-а.
    #[inline]
    #[allow(dead_code)]
    pub fn presented_frames(&self) -> u64 {
        self.frames_presented_to_surface()
    }

    /// Возвращает количество render frames, потерянных на surface boundary.
    #[inline]
    pub fn surface_dropped_frames(&self) -> u64 {
        self.surface_dropped_frames.load(Ordering::Relaxed)
    }

    /// Legacy getter для старого имени surface-drop counter-а.
    #[inline]
    #[allow(dead_code)]
    pub fn dropped_frames(&self) -> u64 {
        self.surface_dropped_frames()
    }

    /// Возвращает процент surface drops от общего числа render attempts.
    ///
    /// Возвращает 0.0, если surface ещё не получал ни presented, ни dropped frame.
    pub fn surface_drop_rate_percent(&self) -> f64 {
        let presented = self.frames_presented_to_surface.load(Ordering::Relaxed);
        let dropped = self.surface_dropped_frames.load(Ordering::Relaxed);
        let total = presented + dropped;
        if total == 0 {
            0.0
        } else {
            (dropped as f64) / (total as f64) * 100.0
        }
    }

    /// Legacy getter для старого имени surface drop rate.
    #[allow(dead_code)]
    pub fn drop_rate_percent(&self) -> f64 {
        self.surface_drop_rate_percent()
    }

    /// Записывает информацию о прочитанном packet.
    #[inline]
    pub fn record_packet(&self, kind: TrackKind) {
        self.packets_read.fetch_add(1, Ordering::Relaxed);
        match kind {
            TrackKind::Video => self.video_packets.fetch_add(1, Ordering::Relaxed),
            TrackKind::Audio => self.audio_packets.fetch_add(1, Ordering::Relaxed),
        };
    }

    #[inline]
    pub fn packets_read(&self) -> u64 {
        self.packets_read.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_packets(&self) -> u64 {
        self.video_packets.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn audio_packets(&self) -> u64 {
        self.audio_packets.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn record_video_frame_decoded(&self) {
        self.video_frames_decoded.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_video_frame_presented(&self) {
        self.video_frames_presented.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_video_frame_dropped(&self, reason: VideoDropReason) {
        self.video_frames_dropped.fetch_add(1, Ordering::Relaxed);
        match reason {
            VideoDropReason::Late => self.video_late_drops.fetch_add(1, Ordering::Relaxed),
            VideoDropReason::QueueOverflow => {
                self.video_queue_drops.fetch_add(1, Ordering::Relaxed)
            }
            VideoDropReason::Paused => self.video_pause_drops.fetch_add(1, Ordering::Relaxed),
            VideoDropReason::DecoderStarvation => self
                .video_decoder_starvation
                .fetch_add(1, Ordering::Relaxed),
            VideoDropReason::Other => self.video_other_drops.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// Учитывает ожидаемый discard video frame/packet-а во время seek.
    #[inline]
    pub fn record_seek_discarded_frame(&self, reason: SeekDiscardReason) {
        self.seek_discarded_frames.fetch_add(1, Ordering::Relaxed);
        match reason {
            SeekDiscardReason::SeekPreroll => {
                self.seek_preroll_discarded.fetch_add(1, Ordering::Relaxed)
            }
            SeekDiscardReason::StaleGeneration => self
                .stale_generation_discarded
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    #[inline]
    pub fn record_video_frame_repeated(&self) {
        self.repeated_frames.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn video_frames_decoded(&self) -> u64 {
        self.video_frames_decoded.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_frames_presented(&self) -> u64 {
        self.video_frames_presented.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_frames_dropped(&self) -> u64 {
        self.video_frames_dropped.load(Ordering::Relaxed)
    }

    /// Возвращает drops, которые могут проявиться как видимая потеря video playback.
    ///
    /// Pause-only drops не включены: при pause новая картинка не должна становиться
    /// видимой, поэтому этот counter подходит для claim-а `0 visible playback drops`.
    #[inline]
    pub fn playback_visible_drops(&self) -> u64 {
        self.video_late_drops()
            + self.video_queue_drops()
            + self.video_decoder_starvation()
            + self.video_other_drops()
    }

    #[inline]
    pub fn video_late_drops(&self) -> u64 {
        self.video_late_drops.load(Ordering::Relaxed)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn video_frames_late_dropped(&self) -> u64 {
        self.video_late_drops()
    }

    #[inline]
    pub fn video_queue_drops(&self) -> u64 {
        self.video_queue_drops.load(Ordering::Relaxed)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn video_frames_queue_dropped(&self) -> u64 {
        self.video_queue_drops()
    }

    #[inline]
    pub fn video_pause_drops(&self) -> u64 {
        self.video_pause_drops.load(Ordering::Relaxed)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn video_frames_pause_dropped(&self) -> u64 {
        self.video_pause_drops()
    }

    #[inline]
    pub fn video_decoder_starvation(&self) -> u64 {
        self.video_decoder_starvation.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_other_drops(&self) -> u64 {
        self.video_other_drops.load(Ordering::Relaxed)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn video_frames_other_dropped(&self) -> u64 {
        self.video_other_drops()
    }

    #[inline]
    pub fn seek_discarded_frames(&self) -> u64 {
        self.seek_discarded_frames.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn seek_preroll_discarded(&self) -> u64 {
        self.seek_preroll_discarded.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn stale_generation_discarded(&self) -> u64 {
        self.stale_generation_discarded.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn repeated_frames(&self) -> u64 {
        self.repeated_frames.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_frames_repeated(&self) -> u64 {
        self.repeated_frames()
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что swapchain counters не загрязняют video playback taxonomy.
    #[test]
    fn surface_counters_are_separate_from_playback_counters() {
        let telemetry = Telemetry::new();

        telemetry.record_frame_presented_to_surface();
        telemetry.record_surface_dropped_frame();

        assert_eq!(telemetry.frames_presented_to_surface(), 1);
        assert_eq!(telemetry.surface_dropped_frames(), 1);
        assert_eq!(telemetry.surface_drop_rate_percent(), 50.0);
        assert_eq!(telemetry.presented_frames(), 1);
        assert_eq!(telemetry.dropped_frames(), 1);
        assert_eq!(telemetry.drop_rate_percent(), 50.0);
        assert_eq!(telemetry.video_frames_dropped(), 0);
        assert_eq!(telemetry.playback_visible_drops(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 0);
    }

    /// Проверяет раздельный accounting playback drops и repeated frames.
    #[test]
    fn playback_taxonomy_splits_visible_pause_and_repeated_frames() {
        let telemetry = Telemetry::new();

        telemetry.record_video_frame_dropped(VideoDropReason::Late);
        telemetry.record_video_frame_dropped(VideoDropReason::QueueOverflow);
        telemetry.record_video_frame_dropped(VideoDropReason::Paused);
        telemetry.record_video_frame_dropped(VideoDropReason::DecoderStarvation);
        telemetry.record_video_frame_dropped(VideoDropReason::Other);
        telemetry.record_video_frame_repeated();

        assert_eq!(telemetry.video_frames_dropped(), 5);
        assert_eq!(telemetry.playback_visible_drops(), 4);
        assert_eq!(telemetry.video_late_drops(), 1);
        assert_eq!(telemetry.video_queue_drops(), 1);
        assert_eq!(telemetry.video_pause_drops(), 1);
        assert_eq!(telemetry.video_decoder_starvation(), 1);
        assert_eq!(telemetry.video_other_drops(), 1);
        assert_eq!(telemetry.repeated_frames(), 1);
        assert_eq!(telemetry.frames_presented_to_surface(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 0);
    }

    /// Проверяет, что seek discard имеет общий counter и typed подкатегории.
    #[test]
    fn seek_discard_taxonomy_keeps_total_and_reason_counters() {
        let telemetry = Telemetry::new();

        telemetry.record_seek_discarded_frame(SeekDiscardReason::SeekPreroll);
        telemetry.record_seek_discarded_frame(SeekDiscardReason::StaleGeneration);

        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
        assert_eq!(telemetry.video_frames_dropped(), 0);
        assert_eq!(telemetry.playback_visible_drops(), 0);
        assert_eq!(telemetry.surface_dropped_frames(), 0);
    }
}
