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
