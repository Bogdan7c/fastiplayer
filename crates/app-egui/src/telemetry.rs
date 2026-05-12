/// Модуль телеметрии для отслеживания производительности рендеринга.
///
/// Содержит атомарные счётчики для:
/// - количества представленных кадров (presented frames)
/// - количества пропущенных кадров (dropped frames)
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
}

/// Глобальные счётчики телеметрии.
///
/// Синглтон через OnceLock для ленивой инициализации.
/// Доступен из любого места приложения без передачи ссылок.
pub struct Telemetry {
    /// Общее количество представленных кадров на экран.
    presented_frames: AtomicU64,

    /// Общее количество пропущенных кадров (не успели к vsync).
    dropped_frames: AtomicU64,

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

    /// Количество отображённых видеокадров.
    video_frames_presented: AtomicU64,

    /// Количество пропущенных видеокадров (A/V sync drop).
    video_frames_dropped: AtomicU64,

    /// Количество видеокадров, пропущенных из-за опоздания к media time.
    video_frames_late_dropped: AtomicU64,

    /// Количество видеокадров, вытесненных из-за переполнения очереди.
    video_frames_queue_dropped: AtomicU64,

    /// Количество видеокадров, отброшенных во время pause.
    video_frames_pause_dropped: AtomicU64,

    /// Количество render ticks, где был повторно показан предыдущий video frame.
    video_frames_repeated: AtomicU64,
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
            presented_frames: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
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
            video_frames_late_dropped: AtomicU64::new(0),
            video_frames_queue_dropped: AtomicU64::new(0),
            video_frames_pause_dropped: AtomicU64::new(0),
            video_frames_repeated: AtomicU64::new(0),
        }
    }

    /// Инкрементирует счётчик представленных кадров.
    ///
    /// Вызывается каждый раз, когда кадр успешно отправлен на экран
    /// через `surface_texture.present()`.
    #[inline]
    pub fn record_presented_frame(&self) {
        self.presented_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// Инкрементирует счётчик пропущенных кадров.
    ///
    /// Вызывается, когда кадр не успел к текущему vsync
    /// и был пропущен для поддержания синхронизации.
    #[inline]
    pub fn record_dropped_frame(&self) {
        self.dropped_frames.fetch_add(1, Ordering::Relaxed);
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

    /// Возвращает общее количество представленных кадров.
    #[inline]
    pub fn presented_frames(&self) -> u64 {
        self.presented_frames.load(Ordering::Relaxed)
    }

    /// Возвращает общее количество пропущенных кадров.
    #[inline]
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// Возвращает процент пропущенных кадров от общего числа.
    ///
    /// Возвращает 0.0, если ещё не было представленных кадров.
    pub fn drop_rate_percent(&self) -> f64 {
        let presented = self.presented_frames.load(Ordering::Relaxed);
        let dropped = self.dropped_frames.load(Ordering::Relaxed);
        let total = presented + dropped;
        if total == 0 {
            0.0
        } else {
            (dropped as f64) / (total as f64) * 100.0
        }
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
            VideoDropReason::Late => self
                .video_frames_late_dropped
                .fetch_add(1, Ordering::Relaxed),
            VideoDropReason::QueueOverflow => self
                .video_frames_queue_dropped
                .fetch_add(1, Ordering::Relaxed),
            VideoDropReason::Paused => self
                .video_frames_pause_dropped
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    #[inline]
    pub fn record_video_frame_repeated(&self) {
        self.video_frames_repeated.fetch_add(1, Ordering::Relaxed);
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

    #[inline]
    pub fn video_frames_late_dropped(&self) -> u64 {
        self.video_frames_late_dropped.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_frames_queue_dropped(&self) -> u64 {
        self.video_frames_queue_dropped.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_frames_pause_dropped(&self) -> u64 {
        self.video_frames_pause_dropped.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn video_frames_repeated(&self) -> u64 {
        self.video_frames_repeated.load(Ordering::Relaxed)
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}
