//! Способы запуска decoder thread: без такта показа и с ним (gles2-renderer 12a.2).

use video_core::PresentPassSubscription;

use super::present_pacing::PresentPacing;
use super::{VideoDecodeThread, VideoDecodeThreadConfig};

impl VideoDecodeThread {
    /// Создаёт decoder thread с явно заданными bounded queue/runtime limits.
    ///
    /// Без такта показа: декодер подаёт пакеты, как только они есть (рендер wgpu, тесты).
    pub fn new_with_config(config: VideoDecodeThreadConfig) -> anyhow::Result<Self> {
        Self::spawn(config, PresentPacing::unpaced())
    }

    /// Создаёт decoder thread, который при запасе кадров подаёт следующую картинку только
    /// после сигнала рендера «проход показа отправлен» (`decoder_thread::present_pacing`).
    pub fn new_with_present_pass_pacing(
        config: VideoDecodeThreadConfig,
        present_pass: PresentPassSubscription,
    ) -> anyhow::Result<Self> {
        Self::spawn(config, PresentPacing::paced(present_pass))
    }
}
