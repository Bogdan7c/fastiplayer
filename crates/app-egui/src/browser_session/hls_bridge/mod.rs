//! HLS-поколение браузерного режима (мост, сессия 10a1, решение 228): плеер ведёт
//! нативный HLS готовым движком, а каждый ресурс просит у страницы по номерку.
//!
//! ```text
//!  hls_source_added{root} ─► переводчик ─► HlsFactory: очередь ресурсов + открыватель
//!                                │                          │ поток bridge-hls-open
//!   рамки вида 2, итоги ────────►│ писатель очереди          ▼
//!   (claim_of: «наш» номер)      │            native HLS (media-source-open)
//!                                ▼                          │ «розетка» транспорта
//!  главный цикл ◄─ take_requests ─ очередь ◄── BridgeResourceTransport ◄┘
//!       │ fetch_resource (только при протоколе ≥ 1.1)
//!       ▼
//!   страница качает по номерку с куками вкладки
//! ```
//!
//! Что здесь живёт (каждое — в своём модуле):
//! - [`transport`] + [`fetch_plan`] — переходник «розетка» → очередь: адрес-заглушка →
//!   номерок, диапазон и потолок тела, перевод отказов в `SourceError` с сохранением
//!   «повторять ли». Всё, что не номерок, отвергается без запроса в протокол;
//! - [`opener`] — открытие native HLS в своём потоке, результат почтовым ящиком;
//! - [`sealed_end`] — обёртка демуксера: обрыв печатью (вкладка ушла) — конец
//!   потока, а не ошибка, поэтому скачанное доигрывается;
//! - [`open_failure`] — классы отказа открытия (`file_unavailable`, `unparsable_media`, …);
//! - [`capabilities`] — что умеет плеер (выбор варианта HLS);
//! - [`factory`] — шов с переводчиком.
//!
//! Чего здесь нет: протокол (`browser-bridge-protocol`), сам HLS (`web-media-hls`,
//! `media-source-open`), очередь (`pushed-media-source::resource_fetch`).

pub(crate) mod capabilities;
pub(crate) mod factory;
mod fetch_plan;
pub(crate) mod open_failure;
pub(crate) mod opener;
mod sealed_end;
pub(crate) mod transport;
