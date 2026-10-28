//! Превращение аренды кадра декодера в непрозрачный ключ рендера.
//!
//! # Ключ как номерок гардероба
//!
//! [`RenderableFrameKey`] — то, что приложение держит вместо GPU-текстур. Внутри:
//! - паспорт рендера, который подготовил кадр (чужой ключ рендер не рисует);
//! - клон аренды кадра ([`VideoFrameLease`]): пока жив ключ, жива аренда, и декодер не может
//!   переиспользовать поверхность, которую рендер ещё читает;
//! - подготовленные рендером ресурсы (текстуры/EGLImage/views) в том же `Arc`: открыть их
//!   может только сам рендер (`prepared_for`).
//!
//! Инвариант для реализаций: рендер не хранит ключи у себя после возврата из `render_frame`.
//! Иначе аренда кадра не закончится, и кадр не вернётся декодеру (это ловит conformance-проверка
//! «освобождение ровно один раз»). Свои кэши импорта рендер держит по идентичности буфера,
//! а не по ключам.

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use video_core::{DecodedFrame, DmaBufDescriptorRejection};
use video_frame_contract::VideoFrameContract;
use video_present_core::VideoFrameLease;

use crate::RendererInstanceId;

/// Непрозрачный ключ готового к показу видеокадра.
///
/// `Clone` дешёвый (один `Arc`): приложение кладёт клоны в кэш «показать прошлый кадр»,
/// в замороженный кадр смены декодера и т.д., не трогая GPU-ресурсы. Аренда и ресурсы рендера
/// лежат в одном `Arc`: ключ маленький (исход `Ready` горячего пути не раздувается копией
/// аренды) и стоит одну аллокацию на подготовленный кадр.
#[derive(Clone)]
pub struct RenderableFrameKey {
    /// Паспорт рендера, который подготовил кадр.
    owner: RendererInstanceId,

    /// Аренда кадра и подготовленные рендером ресурсы; тип ресурсов знает только рендер.
    contents: Arc<dyn KeyContents>,
}

/// Содержимое ключа без знания типа ресурсов рендера.
trait KeyContents: Send + Sync {
    /// Аренда кадра: удерживает кадр декодера, пока жив хоть один клон ключа.
    fn lease(&self) -> &VideoFrameLease;

    /// Подготовленные ресурсы рендера для `downcast_ref`.
    fn prepared(&self) -> &dyn Any;
}

/// Конкретное содержимое ключа для ресурсов типа `Prepared`.
struct TypedKeyContents<Prepared> {
    lease: VideoFrameLease,
    prepared: Prepared,
}

impl<Prepared: Any + Send + Sync> KeyContents for TypedKeyContents<Prepared> {
    fn lease(&self) -> &VideoFrameLease {
        &self.lease
    }

    fn prepared(&self) -> &dyn Any {
        &self.prepared
    }
}

impl RenderableFrameKey {
    /// Собирает ключ из аренды и подготовленных ресурсов. Вызывает только конкретный рендер.
    pub fn new<Prepared: Any + Send + Sync>(
        owner: RendererInstanceId,
        lease: VideoFrameLease,
        prepared: Prepared,
    ) -> Self {
        Self {
            owner,
            contents: Arc::new(TypedKeyContents { lease, prepared }),
        }
    }

    /// Каким рендером подготовлен кадр.
    #[must_use]
    pub const fn owner(&self) -> RendererInstanceId {
        self.owner
    }

    /// Аренда кадра, которую удерживает ключ (pts, generation, отметка «отправлен в рендер»).
    #[must_use]
    pub fn lease(&self) -> &VideoFrameLease {
        self.contents.lease()
    }

    /// Метаданные кадра декодера (размер, ориентация, цвет, контракт).
    #[must_use]
    pub fn decoded_frame(&self) -> &DecodedFrame {
        self.lease().decoded_frame()
    }

    /// Рендер открывает свои подготовленные ресурсы.
    ///
    /// `None`, если ключ выдан другим экземпляром рендера или внутри ресурсы другого типа.
    #[must_use]
    pub fn prepared_for<Prepared: Any>(
        &self,
        expected_owner: RendererInstanceId,
    ) -> Option<&Prepared> {
        if self.owner != expected_owner {
            return None;
        }
        self.contents.prepared().downcast_ref::<Prepared>()
    }

    /// Указывают ли два ключа на одни и те же подготовленные ресурсы (повтор кадра без импорта).
    #[must_use]
    pub fn shares_prepared_frame_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.contents, &other.contents)
    }
}

impl fmt::Debug for RenderableFrameKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderableFrameKey")
            .field("owner", &self.owner)
            .field("resource_handle", &self.lease().resource_handle())
            .finish_non_exhaustive()
    }
}

/// Итог одной попытки подготовить кадр к показу.
#[derive(Debug)]
#[must_use = "исход подготовки кадра определяет показ и учёт ошибок"]
pub struct FrameMaterialization {
    /// Что получилось.
    pub outcome: FrameMaterializationOutcome,

    /// Сколько рендер ждал lock пула ресурсов декодера/своего пула текстур
    /// (диагностика: приложение отправляет её в аренду ровно один раз).
    pub resource_pool_lock_wait: Duration,
}

impl FrameMaterialization {
    /// Исход без ожидания пула (например, отказ до обращения к provider-у).
    pub const fn without_pool_wait(outcome: FrameMaterializationOutcome) -> Self {
        Self {
            outcome,
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    /// Была ли попытка отклонена из-за занятого ресурса (для диагностики аренды).
    #[must_use]
    pub const fn lookup_was_busy(&self) -> bool {
        matches!(self.outcome, FrameMaterializationOutcome::Busy)
    }
}

/// Исход подготовки кадра. Варианты нельзя схлопывать: вызывающий по-разному решает
/// о показе (правило 6 `AGENTS.md`).
#[derive(Debug)]
pub enum FrameMaterializationOutcome {
    /// Кадр готов: ключ можно передать в `render_frame`.
    Ready(RenderableFrameKey),

    /// Ресурс временно занят (пул текстур или lock пула декодера): приложение может показать
    /// прошлый кадр. Кадр декодера не тронут.
    Busy,

    /// Provider не знает ресурса этого кадра (кадр уже освобождён/устарел).
    Missing,

    /// Рендер не умеет этот кадр в принципе или подключённый импорт не для этого пути.
    /// Это не поломка: приложение может перейти на другой декодер.
    Unsupported(FrameImportUnsupportedReason),

    /// К рендеру не подключён ни один декодер (нет активного импорта).
    NoVideoBackendBound,

    /// Поломка: provider в фатальном состоянии или импорт в GPU не удался.
    Error(FrameImportError),
}

/// Почему рендер не может подготовить этот кадр.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameImportUnsupportedReason {
    /// Контракт кадра не объявлен в `RenderCapabilities` этого рендера.
    ContractNotDeclared {
        /// Контракт кадра, пришедший от декодера.
        frame_contract: VideoFrameContract,
    },

    /// Подключённый импорт обслуживает другой путь передачи (например, импорт DMA-BUF
    /// получил кадр программного декодера в памяти процесса).
    TransferPathNotHandledByActiveImport,

    /// Путь передачи тот же, но раскладка кадра не входит в матрицу подключённого импорта.
    LayoutNotHandledByActiveImport,

    /// Дескриптор DMA-BUF отклонён до импорта (раскладка/модификатор/плоскости).
    /// Приложение по этой причине переходит на программный декод для текущего файла.
    DmaBufDescriptorRejected(DmaBufDescriptorRejection),

    /// Дескриптор кадра исправен, но САМ РЕНДЕР не может показать этот аппаратный кадр
    /// (драйвер не импортирует формат, формат вне объявленного, провайдер отдал память
    /// процесса вместо DMA-BUF). Это ограничение рендера, а не ошибка декодера: приложение
    /// по этой причине тоже переходит на программный декод для текущего файла (решение
    /// владельца 41; раньше GLES2 выдавал такой отказ за `InvalidFrameContract` дескриптора).
    RendererCannotShowHardwareFrame(HardwareFrameRefusal),
}

/// Почему рендер не может показать аппаратный кадр с исправным дескриптором.
///
/// Тип нейтральный: подробности драйвера/формата рендер передаёт текстом для логов и
/// диагностики, а различие классов остаётся типизированным.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareFrameRefusal {
    /// Провайдер отдал дескриптор памяти процесса там, где обещан аппаратный буфер.
    HostMemoryDescriptor,

    /// Формат/раскладка кадра вне того, что рендер объявляет (например, 10 бит на GLES2).
    FrameFormatNotShown {
        /// Что именно не подошло (для логов).
        details: String,
    },

    /// Драйвер не объявил, что умеет импортировать этот буфер (нет расширений, нет
    /// `sampler2D` для формата и модификатора).
    DriverCannotImport {
        /// Ответ драйвера/рендера (для логов).
        details: String,
    },
}

impl fmt::Display for HardwareFrameRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostMemoryDescriptor => formatter
                .write_str("the provider returned a host-memory descriptor for a hardware frame"),
            Self::FrameFormatNotShown { details } => {
                write!(
                    formatter,
                    "the renderer does not show this frame format: {details}"
                )
            }
            Self::DriverCannotImport { details } => {
                write!(
                    formatter,
                    "the graphics driver cannot import this hardware frame: {details}"
                )
            }
        }
    }
}

/// Поломка при подготовке кадра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameImportError {
    /// Provider декодера сообщил фатальное состояние (например, отравленный mutex пула).
    ProviderFatal,

    /// Драйвер отказал в импорте/загрузке кадра.
    ImportFailed {
        /// Сообщение рендера для логов.
        message: String,
    },
}

/// Сколько программных кадров рендер довёл до GPU каждым путём (сессия 13a gles2-renderer).
///
/// Это доказательство пути «без CPU-копии» в живом прогоне: трасса кадра пишет оба числа,
/// а замер сравнивает их разность за окно. Счётчики только растут; начинаются с нуля вместе
/// с импортом или рендером (у разных рендеров по-разному), поэтому смысл имеет только
/// разность внутри одного прогона, а не абсолютное значение.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SoftwareFrameUploadStats {
    /// Кадров, которые GPU взял прямо из памяти декодера: CPU их не копировал.
    pub frames_without_cpu_copy: u64,

    /// Кадров, которые CPU скопировал (staging у wgpu, `glTexSubImage2D` у GLES2).
    pub frames_with_cpu_copy: u64,
}

impl SoftwareFrameUploadStats {
    /// Сколько программных кадров учтено всего (обоими путями).
    #[must_use]
    pub const fn frames_total(self) -> u64 {
        self.frames_without_cpu_copy + self.frames_with_cpu_copy
    }
}

/// Нейтральные счётчики импорта кадров для трассы `render_frame_timing`.
///
/// Поля повторяют прежний wgpu-снимок `DmaBufImportStats`, чтобы трасса не меняла имена.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameImportStats {
    /// Сколько импортов в GPU реально выполнено (промахи кэша).
    pub imports_performed: u64,

    /// Сколько импортов отклонил драйвер.
    pub import_failures: u64,

    /// Переиспользует ли рендер импорт одного буфера между кадрами.
    pub image_reuse_enabled: bool,

    /// Состояние кэша импорта.
    pub cache: FrameImportCacheSnapshot,
}

/// Снимок ограниченного кэша импорта по идентичности буфера декодера.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameImportCacheSnapshot {
    /// Сколько записей сейчас в кэше.
    pub entries: usize,

    /// Предел кэша (согласован с пулом поверхностей декодера).
    pub capacity: usize,

    /// Попадания в кэш.
    pub hits: u64,

    /// Замены записи того же ресурса с другой идентичностью.
    pub replaced_same_resource: u64,

    /// Сбросы из-за смены формата.
    pub evicted_format_change: u64,

    /// Вытеснения по пределу ёмкости.
    pub evicted_capacity: u64,
}
