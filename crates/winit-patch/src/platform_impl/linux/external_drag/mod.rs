//! Общая часть приёма внешнего drag & drop для Linux-бэкендов (X11 и Wayland).
//!
//! Модуль владеет тремя вещами:
//! * [`queue`] — очередь готовых событий жеста между бэкендом и диспетчером приложения;
//! * [`mime`] — выбор лучшего формата данных из предложенных источником;
//! * [`payload`] — разбор байтов (`text/uri-list`, `text/x-moz-url`, простой текст)
//!   в публичное [`ExternalDragPayload`](crate::platform::external_drag::ExternalDragPayload).
//!
//! Бэкенды (`wayland/external_drag.rs`, `x11/dnd.rs`) знают только про эти три
//! границы и ничего не знают друг о друге.

// Часть функций (выбор MIME, `text/x-moz-url`, `file://` -> путь) нужна только Wayland-бэкенду;
// при сборке без него они не используются, это ожидаемо.
#[cfg_attr(not(wayland_platform), allow(dead_code))]
pub(crate) mod mime;
#[cfg_attr(not(wayland_platform), allow(dead_code))]
pub(crate) mod payload;
pub(crate) mod queue;

/// Верхняя граница размера данных одного жеста (1 MiB).
///
/// Реальные списки URI занимают килобайты; лимит защищает от источника, который
/// шлёт бесконечный поток. При превышении жест завершается событием `Left`.
pub(crate) const MAX_DROP_PAYLOAD_BYTES: usize = 1024 * 1024;
