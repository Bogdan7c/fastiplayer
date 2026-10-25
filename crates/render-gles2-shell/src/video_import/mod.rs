//! Склад кадров GLES2-рендера: то, что `RenderShell::video_frames` и подготовщик импорта делают
//! с кадрами декодера (сессия 09 плана `user/gles2-renderer/`).
//!
//! # Как устроено (и чем отличается от wgpu)
//!
//! У wgpu каждый кадр материализуется в свой набор текстур, а ключ кадра держит их. У GLES2 на
//! старом железе (T410) видеослой держит **один текущий кадр**: загруженный в кольцо слотов
//! (программный путь) или импортированный DMA-BUF (аппаратный). Поэтому ключ кадра
//! ([`RenderableFrameKey`](render_shell_api::RenderableFrameKey)) несёт не текстуры, а **рецепт**
//! ([`Gles2PreparedFrame`]): эпоху подключения декодера, ключ кадра видеослоя и источник
//! ресурсов кадра. Если приложение рисует ключ, который уже не «текущий» (замороженный кадр,
//! повтор прошлого кадра при `Busy`, scrub), рендер по рецепту возвращает его картинку на
//! видеослой — ключ держит аренду, поэтому ресурсы кадра у провайдера ещё живы. Чужую или
//! устаревшую картинку молча не рисуем никогда.
//!
//! # Модули
//!
//! - [`environment`] — подготовка импорта (обёртка декодера по fence) — общая для склада рендера
//!   и переносимого подготовщика потока плеера;
//! - [`prepared`] — рецепт кадра внутри ключа и источник ресурсов;
//! - [`layer_feed`] — «сделать кадр текущим на видеослое» (загрузка или DMA-BUF) с исходами;
//! - [`dma_buf_outcome`] — перевод отказов аппаратного пути в словарь контракта;
//! - [`state`] — активный импорт, эпоха и учёт того, какой кадр сейчас на видеослое;
//! - [`handoff`] — перенос импорта при пересоздании рендера и владение привязкой освобождения.

mod dma_buf_outcome;
mod environment;
mod handoff;
mod layer_feed;
mod prepared;
mod state;

pub use environment::Gles2VideoImportPreparer;
pub(crate) use environment::{ImportEnvironment, PendingGles2Import, PendingOrigin};
pub(crate) use handoff::{Gles2ImportHandoff, OwnedReleaseBinding};
pub(crate) use layer_feed::{LayerShow, LayerShowOutcome, show_frame_on_layer};
pub(crate) use prepared::Gles2PreparedFrame;
pub(crate) use state::{ActiveImport, ImportEpoch, ShownFrameTracker};

/// Семейство рендера, которому принадлежат коробки склада (импорт, перенос при пересоздании).
pub(crate) const GLES2_FAMILY: render_shell_api::RendererFamily =
    render_shell_api::RendererFamily::Gles2;
