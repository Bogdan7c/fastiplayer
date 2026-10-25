//! Отчёт стенда: человекочитаемые строки и одна строка `RESULT {json}` для скриптов.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "стенд замера (пример, не production): отказ окружения прерывает прогон с \
              понятным сообщением"
)]

use std::time::Duration;

use gles2_egui::DmaBufTextureStats;
use gles2_video::{DmaBufImportCacheSnapshot, VideoLayerStats};
use render_gles2_shell::FrameReleaseStats;
use video_frame_contract::DmaBufImageLayout;

use crate::OutcomeCounters;
use crate::metrics::percentile_ms;
use crate::pixel_check::PixelCheck;

/// Всё, что стенд наблюдал за прогон.
pub struct RunSummary<'a> {
    pub layout: DmaBufImageLayout,
    pub pool_slots: usize,
    pub descriptor_description: Option<&'a str>,
    pub shown: usize,
    pub elapsed: Duration,
    pub process_cpu: Duration,
    pub counters: &'a OutcomeCounters,
    pub first_failure: Option<&'a str>,
    /// Время кадра показа по часам (отсортировано).
    pub show_wall_sorted: &'a [Duration],
    /// CPU потока рендера на кадр показа (отсортировано).
    pub show_cpu_sorted: &'a [Duration],
    pub cache: DmaBufImportCacheSnapshot,
    pub texture_stats: DmaBufTextureStats,
    pub release_stats: FrameReleaseStats,
    /// Очередь ожидания fence сразу после остановки показа (до паузы).
    pub pending_right_after_stop: usize,
    pub layer_stats: VideoLayerStats,
    /// `Some`, если запрошена сверка пикселей.
    pub pixel_check: Option<&'a PixelCheck>,
}

impl RunSummary<'_> {
    /// Печатает таблицу и строку `RESULT`.
    pub fn print(&self) {
        let fps = self.shown as f64 / self.elapsed.as_secs_f64();
        let process_cpu_percent =
            self.process_cpu.as_secs_f64() / self.elapsed.as_secs_f64() * 100.0;
        let counters = self.counters;
        println!(
            "first descriptor: {}",
            self.descriptor_description.unwrap_or("none")
        );
        println!(
            "{} frames in {:.2} s ({fps:.1} fps), underruns {}, busy {}, failure {:?}",
            self.shown,
            self.elapsed.as_secs_f64(),
            counters.underruns,
            counters.busy,
            self.first_failure,
        );
        println!(
            "outcomes: cache_hit {} fresh_import {} already_current {} busy {} missing {} fatal {} unsupported {} import_failed {}",
            counters.cache_hit,
            counters.fresh_import,
            counters.already_current,
            counters.busy,
            counters.missing,
            counters.provider_fatal,
            counters.unsupported,
            counters.import_failed,
        );
        if let Some(check) = self.pixel_check {
            println!(
                "pixels: {} distinct frame hashes of {} observed, {} black frames, {} equal-to-previous",
                check.hashes.len(),
                check.observed,
                check.black_frames,
                check.equal_to_previous,
            );
        }
        println!("cache: {:?}", self.cache);
        println!("dma_buf_texture_stats: {:?}", self.texture_stats);
        println!(
            "release: {:?} (pending right after stop {})",
            self.release_stats, self.pending_right_after_stop
        );
        println!(
            "layer: copied_bytes {} upload_calls {} draw_calls {}",
            self.layer_stats.copied_bytes,
            self.layer_stats.upload_calls,
            self.layer_stats.draw_calls
        );
        let wall_p50 = percentile_ms(self.show_wall_sorted, 0.5);
        let wall_p95 = percentile_ms(self.show_wall_sorted, 0.95);
        let cpu_p50 = percentile_ms(self.show_cpu_sorted, 0.5);
        let cpu_p95 = percentile_ms(self.show_cpu_sorted, 0.95);
        println!(
            "show wall ms p50 {wall_p50:.3} p95 {wall_p95:.3} | thread CPU ms p50 {cpu_p50:.3} p95 {cpu_p95:.3} | process CPU {:.3} s over {:.2} s ({process_cpu_percent:.1}%)",
            self.process_cpu.as_secs_f64(),
            self.elapsed.as_secs_f64(),
        );
        let release = &self.release_stats;
        println!(
            "RESULT {{\"layout\":\"{:?}\",\"pool_slots\":{},\"shown\":{},\"fps\":{fps:.2},\"underruns\":{},\"busy\":{},\"cache_hit\":{},\"fresh_import\":{},\"already_current\":{},\"imports\":{},\"hits\":{},\"entries\":{},\"evicted_capacity\":{},\"evicted_format_change\":{},\"images_created\":{},\"images_destroyed\":{},\"image_destroy_failures\":{},\"released_by_waiter\":{},\"released_fast\":{},\"released_without_fence\":{},\"release_failures\":{},\"pending_after_idle\":{},\"copied_bytes\":{},\"upload_calls\":{},\"show_wall_p50_ms\":{wall_p50:.3},\"show_wall_p95_ms\":{wall_p95:.3},\"show_cpu_p50_ms\":{cpu_p50:.3},\"show_cpu_p95_ms\":{cpu_p95:.3},\"process_cpu_pct\":{process_cpu_percent:.1},\"failure\":{:?}}}",
            self.layout,
            self.pool_slots,
            self.shown,
            counters.underruns,
            counters.busy,
            counters.cache_hit,
            counters.fresh_import,
            counters.already_current,
            self.cache.imports,
            self.cache.hits,
            self.cache.entries,
            self.cache.evicted_capacity,
            self.cache.evicted_format_change,
            self.texture_stats.images_created,
            self.texture_stats.images_destroyed,
            self.texture_stats.image_destroy_failures,
            release.released_by_waiter,
            release.released_after_fence_signaled_fast,
            release.released_without_fence,
            release.fence_wait_failures + release.waiter_spawn_failures,
            release.pending_now,
            self.layer_stats.copied_bytes,
            self.layer_stats.upload_calls,
            self.first_failure,
        );
    }
}
