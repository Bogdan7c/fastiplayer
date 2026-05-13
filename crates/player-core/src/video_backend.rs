use std::sync::Arc;

mod private {
    /// Sealed marker: внешние crates не должны создавать backend wrapper напрямую.
    pub trait Sealed {}
}

/// Фабрика video backend-а, которую session вызывает без знания деталей конкретного backend init.
pub trait VideoBackendFactory: private::Sealed {
    /// Стартует backend и возвращает уже готовый decoder thread wrapper.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend>;
}

/// Запущенный video backend, подготовленный фабрикой для установки в playback pipeline.
pub struct StartedVideoBackend {
    /// Decoder thread остаётся внутренней деталью player-core pipeline.
    pub(crate) decoder_thread: video_vaapi::VideoDecodeThread,
}

impl StartedVideoBackend {
    /// Создаёт backend wrapper вокруг decoder thread, который уже прошёл init handshake.
    fn from_decoder_thread(decoder_thread: video_vaapi::VideoDecodeThread) -> Self {
        Self { decoder_thread }
    }
}

/// WGPU-backed factory для текущего hardware decode backend-а.
///
/// Название намеренно не содержит VA-API: public caller передаёт GPU handles,
/// а конкретный backend остаётся внутренним выбором player-core/video layer.
pub struct WgpuVideoBackendFactory<'a> {
    /// WGPU instance для zero-copy import path.
    instance: &'a wgpu::Instance,

    /// WGPU adapter для backend capability matching.
    adapter: &'a wgpu::Adapter,

    /// WGPU device для texture allocation.
    device: &'a wgpu::Device,

    /// WGPU queue для texture upload/release callbacks.
    queue: &'a wgpu::Queue,
}

impl<'a> WgpuVideoBackendFactory<'a> {
    /// Создаёт factory из GPU handles, которыми владеет shell/render layer.
    #[must_use]
    pub fn new(
        instance: &'a wgpu::Instance,
        adapter: &'a wgpu::Adapter,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Self {
        Self {
            instance,
            adapter,
            device,
            queue,
        }
    }
}

impl private::Sealed for WgpuVideoBackendFactory<'_> {}

impl VideoBackendFactory for WgpuVideoBackendFactory<'_> {
    /// Запускает текущий hardware decoder backend за factory boundary.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
        let device = Arc::new(self.device.clone());
        let queue = Arc::new(self.queue.clone());
        let decoder_thread = video_vaapi::VideoDecodeThread::new(
            device,
            queue,
            self.instance.clone(),
            self.adapter.clone(),
        )?;

        Ok(StartedVideoBackend::from_decoder_thread(decoder_thread))
    }
}
