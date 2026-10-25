//! Исходы показа аппаратного кадра: что произошло и во что это превратит сессия 09.
//!
//! Исходы НЕ слиты в один `bool`/`Result`: занятость провайдера, отсутствие кадра, фатальный
//! отказ, «этот кадр GLES2 не показывает» и «драйвер отказал на валидном запросе» требуют от
//! приложения разных действий.
//!
//! # Соответствие `render_shell_api::FrameMaterializationOutcome` (подключает сессия 09)
//!
//! | исход здесь                                   | в приложении                                         |
//! |-----------------------------------------------|------------------------------------------------------|
//! | [`DmaBufMaterializeOutcome::Shown`]           | кадр готов к рисованию (`Ready`)                     |
//! | [`DmaBufMaterializeOutcome::Busy`]            | `Busy`: повторить без ожидания                       |
//! | [`DmaBufMaterializeOutcome::Missing`]         | `Missing`                                            |
//! | [`DmaBufMaterializeOutcome::ProviderFatal`]   | `Error` (фатальное состояние провайдера)             |
//! | [`DmaBufMaterializeOutcome::Unsupported`]     | `Unsupported(DmaBufDescriptorRejected \| RendererCannotShowHardwareFrame)` — приложение запускает программный фолбэк |
//! | [`DmaBufMaterializeOutcome::ImportFailed`]    | `Error`: драйвер отказал на валидном запросе         |
//!
//! `Unsupported` — это «ограничение, а не поломка»: HostPlanar-дескриптор на DMA-BUF пути,
//! противоречие контракту, P010, драйвер без нужных расширений или без `sampler2D` для формата и
//! модификатора. Копии вместо импорта нет никогда: приложение само переключается на программный
//! путь, а этот крейт честно говорит «нет».

use std::time::Duration;

use gles2_egui::DmaBufTextureError;
use gles2_video::{DmaBufImportReport, VideoImportError};
use video_core::DmaBufDescriptorRejection;

use super::error::Nv12DmaBufError;

/// Итог показа кадра: исход и суммарное ожидание lock-ов пула ресурсов провайдера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufMaterialization {
    /// Что произошло.
    pub outcome: DmaBufMaterializeOutcome,
    /// Сколько поток рендера ждал lock-и пула декодера (identity + descriptor lookup), для
    /// диагностики. Заполнено при любом исходе.
    pub resource_pool_lock_wait: Duration,
}

/// Что произошло с аппаратным кадром.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmaBufMaterializeOutcome {
    /// Кадр стал текущим в видеослое: его можно рисовать.
    Shown(DmaBufShownFrom),
    /// Провайдер занят: повторить на следующем кадре без ожидания.
    Busy,
    /// Провайдер не знает такого кадра.
    Missing,
    /// Провайдер в фатальном состоянии.
    ProviderFatal,
    /// GLES2 этот кадр не показывает (ограничение, не поломка): нужен программный фолбэк.
    Unsupported(DmaBufUnsupportedReason),
    /// Драйвер отказал на валидном запросе (кроме ограничений драйвера, они в `Unsupported`).
    ImportFailed(VideoImportError),
}

/// Как кадр стал текущим.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufShownFrom {
    /// Буфер уже был импортирован: ни dup fd, ни GL-вызовов.
    CacheHit,
    /// Буфер импортирован этим вызовом (две текстуры на `EGLImage`).
    FreshImport(DmaBufImportReport),
    /// Этот кадр (ключ) уже был текущим: ничего не изменилось.
    AlreadyCurrent,
}

/// Почему GLES2 этот аппаратный кадр не показывает.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmaBufUnsupportedReason {
    /// Провайдер отдал дескриптор памяти процесса (HostPlanar) на DMA-BUF пути.
    HostPlanarDescriptor,
    /// Дескриптор или идентичность противоречат контракту кадра (проверка `video-core`).
    DescriptorRejected(DmaBufDescriptorRejection),
    /// Формат вне объявленного: P010 (решение владельца 33), не NV12, непонятные слои.
    FrameFormat(Nv12DmaBufError),
    /// Драйвер не объявил, что умеет это импортировать: нет расширений zero-copy, нет
    /// `sampler2D` для формата и модификатора. Фолбэк включает приложение.
    DriverCannotImport(VideoImportError),
}

impl DmaBufMaterializeOutcome {
    /// Отказ импорта видеослоя: ограничение драйвера (`Unsupported`) либо сбой (`ImportFailed`).
    ///
    /// Ограничение — то, что драйвер сам объявил невозможным: нет расширений, нет `sampler2D`
    /// для пары «формат + модификатор», нет функции привязки. Всё остальное (отказ
    /// `eglCreateImage` на валидном запросе, ошибка GL, потеря контекста) — сбой.
    pub(super) fn from_import_error(error: VideoImportError) -> Self {
        let is_driver_limitation = matches!(
            &error,
            VideoImportError::Attach {
                source: DmaBufTextureError::ZeroCopyUnsupported { .. }
                    | DmaBufTextureError::ImageTargetFunctionMissing
                    | DmaBufTextureError::FormatModifierNotSampleable { .. },
                ..
            }
        );
        if is_driver_limitation {
            Self::Unsupported(DmaBufUnsupportedReason::DriverCannotImport(error))
        } else {
            Self::ImportFailed(error)
        }
    }
}
