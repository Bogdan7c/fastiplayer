//! Склад кадров рендера: импорт кадров декодера и их освобождение после GPU.
//!
//! # Кто чем владеет
//!
//! - **Рендер** владеет способом импорта (DMA-BUF → Vulkan image / EGLImage, загрузка
//!   программного кадра в текстуру, завтра DXGI shared handle), своими кэшами и тем, *когда*
//!   кадр можно вернуть декодеру: только после того, как GPU дочитал его (wgpu —
//!   `on_submitted_work_done`, GLES2 — EGL fence, DX — fence). Освобождение — ровно один раз.
//! - **Приложение** владеет решениями о показе и жизненным циклом кандидатов: какой декодер
//!   стартовать (выбор пайплайна), когда переключить (поколение рендера), показать ли прошлый
//!   кадр при `Busy`. Оно держит только непрозрачные [`RenderableFrameKey`] и
//!   [`PendingVideoImport`], а не текстуры.
//!
//! # Подключение декодера в два шага
//!
//! Как и нынешние кандидаты видео-пайплайна: сначала [`VideoFrameImport::prepare_video_import`]
//! «в стороне» (активный импорт не тронут, можно отменить простым drop-ом), потом
//! [`VideoFrameImport::install_video_import`] разом переключает активный импорт.
//!
//! Кандидат нового файла запускается в потоке плеера, а рендер живёт в потоке окна. Для
//! этого рендер выдаёт переносимый [`VideoImportPreparer`] (`RenderShell::video_import_preparer`,
//! решение владельца в сессии 02):
//! первый шаг выполняется им в потоке плеера, установка — по-прежнему на рендере в потоке окна.
//!
//! # Пересоздание рендера того же семейства
//!
//! Старый рендер отдаёт [`VideoImportHandoff`] (provider декодера + план + привязка отложенных
//! освобождений), новый готовит из него [`PendingVideoImport`] на своих GPU-ресурсах,
//! установка на новом рендере перепривязывает отложенные освобождения к его очереди.
//! Переход между семействами (wgpu → GLES2) так не делается: смена семейства — только после
//! перезапуска приложения (решение 5 плана).

mod materialization;

use std::any::Any;
use std::fmt;

use video_backend_api::StartedVideoBackend;
use video_present_core::VideoFrameLease;

use crate::{RendererFamily, RendererInstanceId};

pub use materialization::{
    FrameImportCacheSnapshot, FrameImportError, FrameImportStats, FrameImportUnsupportedReason,
    FrameMaterialization, FrameMaterializationOutcome, HardwareFrameRefusal, RenderableFrameKey,
};

/// Склад кадров рендера. Получается через `RenderShell::video_frames()`.
pub trait VideoFrameImport {
    /// Готовит импорт для только что запущенного декодера, не трогая активный импорт.
    ///
    /// Рендер оборачивает provider декодера так, что освобождение кадра, отправленного в GPU,
    /// дойдёт до декодера только после завершения работы GPU. Возвращённый
    /// [`PreparedVideoImport::backend`] приложение отдаёт плееру вместо исходного.
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError>;

    /// Делает подготовленный импорт активным; прежний активный импорт снимается.
    ///
    /// Кадры, которые прежний импорт уже отправил в GPU, всё равно освобождаются после GPU:
    /// их отложенные освобождения держит обёртка provider-а, а не склад.
    fn install_video_import(
        &mut self,
        pending: PendingVideoImport,
    ) -> Result<(), VideoImportInstallError>;

    /// Путь передачи активного импорта; `None` — декодер не подключён.
    fn active_video_import_path(&self) -> Option<VideoImportPath>;

    /// Превращает аренду кадра в ключ, не блокируясь (горячий путь кадра).
    ///
    /// Порядок проверок фиксирован контрактом: сначала «умеет ли рендер этот контракт вообще»
    /// (`Unsupported`), потом «подключён ли декодер» (`NoVideoBackendBound`), потом
    /// provider/импорт (`Busy`/`Missing`/`Error`/`Ready`).
    fn materialize(&mut self, lease: &VideoFrameLease) -> FrameMaterialization;

    /// Счётчики импорта для трассы кадра; `None` — у активного импорта нет кэша.
    fn import_stats(&self) -> Option<FrameImportStats>;

    /// Отдаёт состояние активного импорта для пересоздания рендера того же семейства.
    ///
    /// Активный импорт не снимается: до установки на новом рендере старый продолжает работать.
    fn export_video_import_for_recreation(&self) -> Option<VideoImportHandoff>;

    /// Готовит импорт на ресурсах этого (нового) рендера из состояния старого.
    fn prepare_recreated_video_import(
        &mut self,
        handoff: VideoImportHandoff,
    ) -> Result<PendingVideoImport, VideoImportPrepareError>;

    /// Устройство потеряно: освобождает все кадры, ждущие завершения GPU, ровно один раз.
    ///
    /// После вызова поздние сигналы старого GPU не могут освободить кадр повторно, а новые
    /// освобождения до установки нового импорта проходят сразу.
    fn release_after_device_lost(&mut self) -> Result<ReleasedFrameCount, VideoImportReleaseError>;
}

/// Подготовщик импорта, который можно отдать в другой поток (например, в поток плеера,
/// где запускается кандидат нового файла).
///
/// Выдаёт `RenderShell::video_import_preparer`. Смысл [`Self::prepare_video_import`] тот же,
/// что у [`VideoFrameImport::prepare_video_import`]:
/// активный импорт рендера не меняется, `PendingVideoImport` устанавливается потом на выдавшем
/// подготовщик рендере. Пересоздание рендера делает импорты старого подготовщика чужими
/// (`PreparedByAnotherRenderer`).
pub trait VideoImportPreparer: Send {
    /// Оборачивает запущенный декодер и готовит импорт для рендера, выдавшего подготовщик.
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError>;
}

/// Путь передачи кадров, под который готовится импорт.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoImportPath {
    /// Аппаратный декод без копий: буфер декодера импортируется в GPU (DMA-BUF и т.п.).
    HardwareZeroCopy,

    /// Программный декод: плоскости кадра в памяти процесса загружаются в текстуры.
    SoftwareHostUpload,
}

/// Сколько поверхностей держит пул декодера; рендер согласует с ним свои кэши импорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecoderSurfacePoolSlots(usize);

impl DecoderSurfacePoolSlots {
    /// Размер пула из конфигурации потока декодера.
    #[must_use]
    pub const fn new(slots: usize) -> Self {
        Self(slots)
    }

    /// Число поверхностей.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Что приложение выбрало для нового декодера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoImportPlan {
    /// Путь передачи кадров.
    pub path: VideoImportPath,

    /// Размер пула поверхностей декодера (предел кэша импорта DMA-BUF).
    pub decoder_surface_pool: DecoderSurfacePoolSlots,
}

/// Подготовленный, но ещё не установленный импорт.
pub struct PreparedVideoImport {
    /// Обёрнутый рендером декодер: его получает плеер вместо исходного.
    pub backend: StartedVideoBackend,

    /// Половина, которую приложение держит до установки (`install_video_import`).
    pub pending: PendingVideoImport,
}

impl fmt::Debug for PreparedVideoImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedVideoImport")
            .field("backend_id", &self.backend.backend_id())
            .field("pending", &self.pending)
            .finish()
    }
}

/// Непрозрачная половина импорта до установки.
///
/// `Send`: слот кандидата приложения может жить в другом потоке до коммита.
/// Drop без установки — отмена: GPU-ресурсы подготовки освобождаются, активный импорт не тронут.
pub struct PendingVideoImport {
    /// Рендер, на чьих ресурсах подготовлен импорт.
    owner: RendererInstanceId,

    /// Путь передачи (для решений приложения без открытия коробки).
    path: VideoImportPath,

    /// Настоящее состояние импорта рендера.
    state: Box<dyn Any + Send>,
}

impl PendingVideoImport {
    /// Кладёт состояние импорта рендера в коробку. Вызывает только конкретный рендер.
    pub fn new<State: Any + Send>(
        owner: RendererInstanceId,
        path: VideoImportPath,
        state: State,
    ) -> Self {
        Self {
            owner,
            path,
            state: Box::new(state),
        }
    }

    /// На каком рендере подготовлен импорт.
    #[must_use]
    pub const fn owner(&self) -> RendererInstanceId {
        self.owner
    }

    /// Путь передачи кадров.
    #[must_use]
    pub const fn path(&self) -> VideoImportPath {
        self.path
    }

    /// Рендер открывает свою коробку при установке.
    pub fn into_state<State: Any>(
        self,
        expected_owner: RendererInstanceId,
    ) -> Result<State, VideoImportInstallError> {
        if self.owner != expected_owner {
            return Err(VideoImportInstallError::PreparedByAnotherRenderer {
                prepared_by: self.owner,
            });
        }
        self.state
            .downcast::<State>()
            .map(|state| *state)
            .map_err(
                |_foreign_state| VideoImportInstallError::PreparedByAnotherRenderer {
                    prepared_by: self.owner,
                },
            )
    }
}

impl fmt::Debug for PendingVideoImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingVideoImport")
            .field("owner", &self.owner)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Состояние активного импорта, переносимое на новый рендер того же семейства.
pub struct VideoImportHandoff {
    /// Семейство рендера, которое умеет открыть содержимое.
    family: RendererFamily,

    /// Путь передачи кадров.
    path: VideoImportPath,

    /// Настоящее состояние (provider, план, привязка отложенных освобождений).
    state: Box<dyn Any + Send>,
}

impl VideoImportHandoff {
    /// Кладёт состояние для переноса. Вызывает только конкретный рендер.
    pub fn new<State: Any + Send>(
        family: RendererFamily,
        path: VideoImportPath,
        state: State,
    ) -> Self {
        Self {
            family,
            path,
            state: Box::new(state),
        }
    }

    /// Семейство рендера, отдавшего состояние.
    #[must_use]
    pub const fn family(&self) -> RendererFamily {
        self.family
    }

    /// Путь передачи кадров.
    #[must_use]
    pub const fn path(&self) -> VideoImportPath {
        self.path
    }

    /// Новый рендер открывает состояние; чужое семейство — типизированный отказ.
    pub fn into_state<State: Any>(
        self,
        expected_family: RendererFamily,
    ) -> Result<State, VideoImportPrepareError> {
        if self.family != expected_family {
            return Err(VideoImportPrepareError::HandoffFromAnotherFamily {
                handoff_family: self.family,
            });
        }
        self.state
            .downcast::<State>()
            .map(|state| *state)
            .map_err(
                |_foreign_state| VideoImportPrepareError::HandoffFromAnotherFamily {
                    handoff_family: self.family,
                },
            )
    }
}

impl fmt::Debug for VideoImportHandoff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VideoImportHandoff")
            .field("family", &self.family)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Почему импорт не подготовлен. Активный импорт при любой ошибке не тронут.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoImportPrepareError {
    /// Рендер не умеет этот путь передачи (например, GLES2 без импорта DMA-BUF и zero-copy).
    PathUnsupported {
        /// Запрошенный путь.
        path: VideoImportPath,
    },

    /// Состояние для переноса отдал рендер другого семейства.
    HandoffFromAnotherFamily {
        /// Семейство, отдавшее состояние.
        handoff_family: RendererFamily,
    },

    /// Рендер не смог создать ресурсы импорта.
    ResourceCreationFailed {
        /// Сообщение рендера для логов.
        message: String,
    },
}

/// Почему подготовленный импорт не установлен. Прежний активный импорт сохранён.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoImportInstallError {
    /// Импорт подготовлен другим экземпляром рендера (например, до пересоздания).
    PreparedByAnotherRenderer {
        /// Паспорт рендера, подготовившего импорт.
        prepared_by: RendererInstanceId,
    },

    /// Не удалось перепривязать отложенные освобождения к очереди этого рендера.
    SubmissionBindingFailed {
        /// Сообщение рендера для логов.
        message: String,
    },
}

/// Сколько кадров освобождено при потере устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleasedFrameCount(usize);

impl ReleasedFrameCount {
    /// Число освобождённых кадров.
    #[must_use]
    pub const fn new(count: usize) -> Self {
        Self(count)
    }

    /// Число освобождённых кадров.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Освобождение после потери устройства не выполнено.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoImportReleaseError {
    /// Состояние отложенных освобождений повреждено (отравленный mutex): что освобождено,
    /// неизвестно, поэтому повторять нельзя.
    PendingReleaseStatePoisoned,
}
