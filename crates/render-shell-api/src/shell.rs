//! Главный контракт «приложение ↔ рендер».
//!
//! Приложение держит `Box<dyn RenderShell>` и не видит ни GPU-API, ни способ импорта кадров.
//! Реализуют: `render-wgpu-shell` (сессия 02), `render-gles2-shell` (сессия 09), в будущем —
//! рендеры Windows. Поведение каждого метода закрепляет conformance-набор
//! (`conformance` feature), одинаковый для всех рендеров.

use std::fmt;
use std::time::Duration;

use render_core::{
    RenderCapabilities, RenderDiagnostics, RenderLiveSettings, RenderLiveSettingsAdapter,
};

use crate::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor, RendererFamily, RendererInstanceId, SurfaceFrameAcquire,
    SurfacePresentSettings, VideoFrameImport, VideoImportPreparer,
};

/// Рендер окна приложения. Object-safe: приложение держит `Box<dyn RenderShell>`.
///
/// Порядок одного кадра (инвариант сессии 06a cpu-parity):
/// 1. [`Self::acquire_surface_frame`] — буфер swapchain (может ждать FIFO);
/// 2. приложение выбирает видеокадр и субтитры и при необходимости готовит ключ через
///    [`Self::video_frames`] → `materialize`;
/// 3. [`Self::render_frame`] с полученным билетом.
pub trait RenderShell {
    /// Семейство рендера (для каталога, логов, метки «запуск не завершён»).
    fn family(&self) -> RendererFamily;

    /// Паспорт этого экземпляра: им помечены его билеты и ключи кадров.
    fn instance_id(&self) -> RendererInstanceId;

    /// Что умеет рендер. Источник истины поддержки кадров — `supported_frame_contracts`,
    /// выведенный из реально найденных возможностей драйвера, а не из имени GPU.
    fn capabilities(&self) -> &RenderCapabilities;

    /// Нейтральная диагностика для панели телеметрии.
    fn diagnostics(&self) -> RenderDiagnostics;

    /// Новый размер окна в физических пикселях; применяется до следующего acquire.
    ///
    /// Пока окно отпущено ([`Self::detach_presentation`]), размер только запоминается и
    /// применяется при возврате окна.
    fn resize(&mut self, size: RenderSurfaceSize);

    /// Применяет новые настройки показа (present mode, задержка) к ТЕКУЩЕЙ surface окна,
    /// без нового device и без пересоздания импорта кадров (сессия 02a, «одно окно — одна
    /// surface»).
    ///
    /// Режим выбирается по тем же правилам, что при создании рендера. Отказ surface
    /// откатывается к прежней конфигурации (`RejectedPreviousRestored`); если не принята и она —
    /// `PresentationLost`. Без окна — `PresentationDetached`; при этом ничего не меняется.
    ///
    /// Вызывать только между кадрами: без живого билета этого рендера.
    fn reconfigure_presentation(
        &mut self,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError>;

    /// Отпускает окно: уничтожает surface (и swapchain), запоминая конфигурацию и окно.
    ///
    /// Нужен полному пересозданию рендера: новый рендер создаёт свою surface на том же окне
    /// только после того, как старый отпустил свою. Пока окно отпущено,
    /// [`Self::acquire_surface_frame`] даёт `Dropped(PresentationDetached)`. Повтор —
    /// `AlreadyDetached`. Вызывать только между кадрами.
    fn detach_presentation(&mut self) -> PresentationDetachOutcome;

    /// Возвращает окно: новая surface на прежнем окне с прежней конфигурацией и текущим
    /// размером окна. Повтор при уже держащем окно рендере — `AlreadyAttached`. При ошибке
    /// рендер остаётся без окна.
    fn reattach_presentation(
        &mut self,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError>;

    /// Получает буфер swapchain под кадр.
    fn acquire_surface_frame(&mut self, screen: RenderScreenDescriptor) -> SurfaceFrameAcquire;

    /// Можно ли в этом кадре повторить прежний интерфейс (`EguiFrameContent::ReusePrevious`).
    ///
    /// `true` только если рендер удерживает примитивы прошлого свежего кадра того же размера и
    /// масштаба. Приложение спрашивает до решения о плане кадра.
    fn can_reuse_retained_ui(&self, screen: RenderScreenDescriptor) -> bool;

    /// Записывает и показывает кадр.
    ///
    /// Чужой билет или чужой ключ кадра — `Failed` с типом `ForeignSurfaceFrame`/
    /// `ForeignVideoFrame`, без записи. `ReusePrevious` без удержанного интерфейса —
    /// `Dropped(RetainedUiUnavailable)`.
    ///
    /// Изменения текстур `EguiFrameContent::Fresh` — состояние рендера (сессия 02a): они
    /// применяются ПЕРВЫМ шагом, до любых отказов, и `free` тоже (раз рисования не будет,
    /// освобождать можно сразу). Частичная правка текстуры, которой у рендера нет, — не паника,
    /// а `Dropped(UiTexturesOutOfSync)`: остальные изменения применены, кадр не рисуется.
    fn render_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome;

    /// Снимок живых настроек цвета (переносится на новый рендер при пересоздании).
    fn live_settings(&self) -> RenderLiveSettings;

    /// Просмотр/применение/откат живых настроек без пересборки пайплайнов и декодера.
    fn live_settings_adapter(&mut self) -> &mut dyn RenderLiveSettingsAdapter;

    /// Ждёт завершения уже отправленной работы GPU (перед заменой рендера), не дольше `timeout`.
    fn wait_for_gpu_idle(&mut self, timeout: Duration) -> Result<(), RendererGpuDrainError>;

    /// Счётчики горячего пути видео (пересоздание привязок ресурсов, запись uniform-ов).
    fn hot_path_stats(&self) -> RenderHotPathStats;

    /// Нужен ли ещё один кадр, хотя приложению показать больше нечего.
    ///
    /// Спрашивать после [`Self::render_frame`]. [`FollowUpFrame::Soon`] — картинка на экране
    /// ещё изменится сама по себе (например, GLES2 досчитывает таблицу HDR → SDR в фоне, и
    /// готовность замечает только следующий `render_frame`); приложение, которое спит в
    /// паузе, обязано перерисоваться. Пока рендер отвечает `NotNeeded`, будить цикл
    /// событий незачем. Метод ничего не меняет в рендере.
    fn needs_follow_up_frame(&self) -> FollowUpFrame;

    /// Склад кадров: импорт кадров декодера и освобождение после GPU.
    fn video_frames(&mut self) -> &mut dyn VideoFrameImport;

    /// Переносимый подготовщик импорта для этого рендера (первый шаг подключения декодера в
    /// другом потоке — у кандидата нового файла это поток плеера).
    ///
    /// Подготовщик держит только то, что нужно для обёртки декодера и подготовки импорта
    /// (у wgpu — клоны device/queue, у GLES2 — очередь освобождений по fence, без GL-объектов),
    /// и помечает импорты паспортом этого рендера: установить их можно только на нём через
    /// `video_frames().install_video_import`. Склад рендера при этом не меняется.
    fn video_import_preparer(&self) -> Box<dyn VideoImportPreparer>;

    /// Сигнал «проход показа отправлен на GPU», если рендер его даёт.
    ///
    /// Рендер звонит в сигнал после каждого показанного кадра (`Presented`) и молчит, когда
    /// на GPU ничего не ушло. Приложение передаёт сигнал фабрике аппаратного декодера: на
    /// GPU, где декод и показ стоят в одной очереди (Intel Ironlake), декодер подаёт
    /// следующую картинку после прохода показа, и кадр не опаздывает на vsync
    /// (gles2-renderer 12a.2). `None` — рендер сигнала не даёт (wgpu): декодер работает
    /// без такта. Клон указывает на тот же сигнал; метод ничего не меняет в рендере.
    fn present_pass_signal(&self) -> Option<video_core::PresentPassSignal>;
}

/// Просит ли рендер ещё один кадр без внешних изменений (ответ [`RenderShell::needs_follow_up_frame`]).
///
/// Отдельный тип, а не `bool`: на месте вызова видно смысл, а будущие варианты
/// («через N мс») добавятся без смены сигнатуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUpFrame {
    /// Картинка устоялась: пробуждать приложение не нужно.
    NotNeeded,

    /// Картинка вот-вот изменится сама: нужен следующий кадр.
    Soon,
}

/// Размер surface окна в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderSurfaceSize {
    /// Ширина в пикселях.
    pub width: u32,

    /// Высота в пикселях.
    pub height: u32,
}

/// Счётчики горячего пути видео-прохода в нейтральной форме.
///
/// «Привязка ресурсов» — wgpu bind group или набор текстур/uniform-ов GLES2 для кадра.
/// Трасса `render_frame_timing` пишет их под прежними именами (`bind_groups_*`, `uniform_*`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderHotPathStats {
    /// Сколько привязок ресурсов кадра создано заново.
    pub resource_bindings_created: u64,

    /// Сколько привязок взято из кэша.
    pub resource_bindings_reused: u64,

    /// Сколько раз записан uniform кадра.
    pub uniform_writes: u64,

    /// Сколько записей uniform пропущено (байты не изменились).
    pub uniform_writes_skipped: u64,
}

/// Почему не удалось дождаться завершения работы GPU.
///
/// Приложение различает `DeviceLost` (освободить кадры принудительно и откатиться) и
/// остальные причины (повторить ожидание при откате).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererGpuDrainError {
    /// Рендер доказал потерю устройства/контекста.
    DeviceLost(RendererDeviceLost),

    /// Работа GPU не завершилась за отведённое время.
    Timeout,

    /// API отклонил само ожидание (у wgpu — неверный submission index).
    WaitRejected,

    /// Состояние «устройство потеряно» повреждено (отравленный mutex), ему нельзя верить.
    DeviceLostStatePoisoned,
}

impl fmt::Display for RendererGpuDrainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceLost(device_lost) => {
                write!(formatter, "GPU device lost: {}", device_lost.message)
            }
            Self::Timeout => formatter.write_str("GPU work did not finish before the timeout"),
            Self::WaitRejected => formatter.write_str("GPU API rejected the idle wait"),
            Self::DeviceLostStatePoisoned => {
                formatter.write_str("GPU device-lost state is poisoned")
            }
        }
    }
}

impl std::error::Error for RendererGpuDrainError {}

/// Подтверждённая потеря устройства/контекста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererDeviceLost {
    /// Причина в нейтральной форме.
    pub reason: RendererDeviceLossReason,

    /// Сообщение драйвера для логов и отчёта об ошибке.
    pub message: String,
}

/// Нейтральная причина потери устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererDeviceLossReason {
    /// Драйвер/система сбросили устройство (GPU hang, reset, `GL_CONTEXT_LOST`).
    Unknown,

    /// Устройство уничтожено самим приложением.
    Destroyed,
}
