//! Снимки состояния декодера с учётом кадров «в полёте» (gles2-renderer, сессия 09a).
//!
//! Очередь «отправлено в GPU, ещё не отдано» живёт на decoder thread-е
//! (`decoder::gpu_pending_frames`); handle видит её только через общие атомарные
//! счётчики. Здесь — методы handle-а, которые эти счётчики учитывают.

use super::VideoDecodeThread;

impl VideoDecodeThread {
    /// Снимок ресурсов декодера для решения плеера «подавать ли следующий packet».
    ///
    /// Кадры очереди «в полёте» (сессия 09a) уже держат VA surfaces, которые
    /// декодер не может переиспользовать, но ещё не зарегистрированы в пуле
    /// импорта. До 09a каждый кадр регистрировался сразу после `FrameReady`,
    /// и плеер видел его в `in_use`; без этой поправки он подавал лишние
    /// packet-ы и упирал декодер в исчерпанный пул выходных surfaces.
    #[must_use]
    pub fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
        let mut snapshot = video_core::DecoderResourceSnapshot::from(self.resource_pool_stats()?);
        let frames_in_flight = self.gpu_pending_frame_counters.snapshot().current_depth;
        snapshot.in_use = snapshot.in_use.saturating_add(frames_in_flight);
        Some(snapshot)
    }

    /// Снимок счётчиков очереди «отправлено в GPU, ещё не отдано» (backend-local).
    #[must_use]
    pub fn gpu_pending_frame_stats(&self) -> crate::gpu_pending_stats::GpuPendingFrameStats {
        self.gpu_pending_frame_counters.snapshot()
    }
}
