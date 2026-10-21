//! Счётчики горячего пути видео-прохода для трассы кадра приложения.
//!
//! Вынесено из `shell.rs` (лимит размера модуля): shell только пробрасывает накопленные
//! счётчики своего `WgpuVideoRenderer`, сам ничего не считает.

use render_wgpu_video::VideoHotPathStats;

use super::Renderer;

impl Renderer {
    /// Накопленные за жизнь текущего video renderer-а счётчики bind group и записей uniform
    /// (cpu-parity сессия 05). После пересоздания renderer-а счётчики начинаются с нуля.
    #[must_use]
    pub fn video_hot_path_stats(&self) -> VideoHotPathStats {
        self.video_renderer.hot_path_stats()
    }
}
