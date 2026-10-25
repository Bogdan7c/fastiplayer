//! Рецепт кадра внутри ключа: что нужно, чтобы вернуть картинку кадра на видеослой.

use super::state::ImportEpoch;
use gles2_video::VideoFrameKey;
use video_backend_api::PresentFrameResourceProviderHandle;
use video_core::DecodedFrame;

use render_shell_api::VideoImportPath;

/// Откуда брать ресурсы кадров: путь импорта и провайдер подключённого декодера.
///
/// Провайдер — тот самый, обёрнутый `wrap_video_backend_for_gles2_release`: через него и
/// читаются ресурсы кадра, и (через аренду кадра) идёт возврат кадра декодеру после fence.
#[derive(Clone)]
pub(crate) struct FrameSource {
    path: VideoImportPath,
    provider: PresentFrameResourceProviderHandle,
}

impl FrameSource {
    /// Источник кадров импорта `path` с провайдером `provider`.
    pub(crate) const fn new(
        path: VideoImportPath,
        provider: PresentFrameResourceProviderHandle,
    ) -> Self {
        Self { path, provider }
    }

    /// Путь импорта (аппаратный DMA-BUF или программная загрузка).
    pub(crate) const fn path(&self) -> VideoImportPath {
        self.path
    }

    /// Провайдер ресурсов кадров декодера.
    pub(crate) const fn provider(&self) -> &PresentFrameResourceProviderHandle {
        &self.provider
    }
}

/// Содержимое `RenderableFrameKey` этого рендера (открывает только он, по паспорту).
///
/// Это рецепт, а не текстуры: см. документацию модуля. Эпоха отделяет кадры разных
/// подключений декодера — у нового декодера номера ресурсов начинаются заново и совпадают с
/// номерами старого, поэтому ключ видеослоя без эпохи был бы неоднозначным.
pub(crate) struct Gles2PreparedFrame {
    epoch: ImportEpoch,
    layer_key: VideoFrameKey,
    source: FrameSource,
}

impl Gles2PreparedFrame {
    /// Рецепт кадра `frame`, подготовленного в эпоху `epoch` из источника `source`.
    pub(crate) fn new(epoch: ImportEpoch, frame: &DecodedFrame, source: FrameSource) -> Self {
        Self {
            epoch,
            layer_key: layer_key_of(frame),
            source,
        }
    }

    /// Эпоха подключения декодера, в которую подготовлен кадр.
    pub(crate) const fn epoch(&self) -> ImportEpoch {
        self.epoch
    }

    /// Ключ кадра на видеослое (номер ресурса декодера).
    pub(crate) const fn layer_key(&self) -> VideoFrameKey {
        self.layer_key
    }

    /// Источник ресурсов кадра.
    pub(crate) const fn source(&self) -> &FrameSource {
        &self.source
    }
}

/// Ключ кадра на видеослое: тот же номер ресурса, что использует загрузка и DMA-BUF путь.
pub(crate) const fn layer_key_of(frame: &DecodedFrame) -> VideoFrameKey {
    VideoFrameKey(frame.resource_handle.0)
}
