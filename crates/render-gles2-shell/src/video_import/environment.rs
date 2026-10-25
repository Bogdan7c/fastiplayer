//! Подготовка импорта: обёртка декодера по fence и ожидающий импорт.
//!
//! [`ImportEnvironment`] — всё, что нужно подготовить импорт и НЕ является GL-объектом: паспорт
//! рендера, привязка освобождения и признак аппаратного zero-copy. Поэтому её можно отдать
//! переносимому подготовщику ([`Gles2VideoImportPreparer`]), который работает в потоке плеера,
//! пока рендер живёт в потоке окна (решение владельца 20 плана).

use render_shell_api::{
    PendingVideoImport, PreparedVideoImport, RendererInstanceId, VideoImportPath, VideoImportPlan,
    VideoImportPrepareError, VideoImportPreparer,
};
use video_backend_api::StartedVideoBackend;

use super::handoff::RecreatedImportParts;
use super::prepared::FrameSource;
use crate::capabilities::HardwareZeroCopy;
use crate::frame_release::{Gles2FrameReleaseBinding, wrap_video_backend_for_gles2_release};

/// Откуда взялся подготовленный импорт: это решает, нужно ли принять привязку предшественника.
pub(crate) enum PendingOrigin {
    /// Новый декодер: обёртка создана на привязке `wrapped_with`. Установка обязана убедиться,
    /// что это привязка устанавливающего рендера: иначе кадры декодера вернулись бы по fence
    /// чужого рендера (а fence этого рендера никого бы не держал).
    FreshDecoder {
        /// Привязка освобождения, на которой сделана обёртка декодера.
        wrapped_with: Gles2FrameReleaseBinding,
    },
    /// Пересоздание рендера: привязку старого рендера надо принять.
    RecreatedRenderer(RecreatedImportParts),
}

/// Содержимое `PendingVideoImport` этого рендера.
pub(crate) struct PendingGles2Import {
    /// План, под который подготовлен импорт.
    pub(crate) plan: VideoImportPlan,
    /// Источник ресурсов кадров (обёрнутый провайдер).
    pub(crate) source: FrameSource,
    /// Откуда импорт.
    pub(crate) origin: PendingOrigin,
}

/// Не графическая часть рендера, нужная для подготовки импорта.
#[derive(Clone)]
pub(crate) struct ImportEnvironment {
    owner: RendererInstanceId,
    binding: Gles2FrameReleaseBinding,
    hardware: HardwareZeroCopy,
}

impl ImportEnvironment {
    /// Окружение рендера `owner` с привязкой `binding`; `hardware` — умеет ли драйвер zero-copy.
    pub(crate) const fn new(
        owner: RendererInstanceId,
        binding: Gles2FrameReleaseBinding,
        hardware: HardwareZeroCopy,
    ) -> Self {
        Self {
            owner,
            binding,
            hardware,
        }
    }

    /// Можно ли подготовить импорт по пути `path` на этом драйвере.
    ///
    /// Аппаратный путь доступен только при уровне `zero_copy` (иначе обещанный импорт без копий
    /// невозможен, а копий вместо него у GLES2 нет никогда).
    pub(crate) fn ensure_path_supported(
        &self,
        path: VideoImportPath,
    ) -> Result<(), VideoImportPrepareError> {
        match (path, self.hardware) {
            (VideoImportPath::HardwareZeroCopy, HardwareZeroCopy::Unavailable) => {
                Err(VideoImportPrepareError::PathUnsupported { path })
            }
            _ => Ok(()),
        }
    }

    /// Оборачивает декодер так, что кадр возвращается ему только после fence показа, и готовит
    /// импорт. Путь проверяется ДО обёртки; при отказе декодер `backend` освобождается (его
    /// забрал вызов), как и во всех отказах подготовки контракта.
    pub(crate) fn prepare(
        &self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        self.ensure_path_supported(plan.path)?;
        let (player_backend, provider) =
            wrap_video_backend_for_gles2_release(backend, &self.binding);
        let pending = PendingVideoImport::new(
            self.owner,
            plan.path,
            PendingGles2Import {
                plan,
                source: FrameSource::new(plan.path, provider),
                origin: PendingOrigin::FreshDecoder {
                    wrapped_with: self.binding.clone(),
                },
            },
        );
        Ok(PreparedVideoImport {
            backend: player_backend,
            pending,
        })
    }
}

/// Подготовщик импорта для потока плеера: кандидат нового файла готовит обёртку декодера и
/// импорт там, где живёт плеер, не трогая GL (контракт `RenderShell::video_import_preparer`).
///
/// Держит только паспорт рендера, клон привязки освобождения и признак zero-copy: `Send`, без
/// GL-объектов. Импорт ставится на выдавший рендер через `install_video_import`.
pub struct Gles2VideoImportPreparer {
    environment: ImportEnvironment,
}

impl Gles2VideoImportPreparer {
    /// Подготовщик над окружением рендера.
    pub(crate) const fn new(environment: ImportEnvironment) -> Self {
        Self { environment }
    }
}

impl VideoImportPreparer for Gles2VideoImportPreparer {
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        self.environment.prepare(backend, plan)
    }
}
