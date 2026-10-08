//! Внешний запрос открытия: файлы и ссылки, принесённые в окно снаружи.
//!
//! Модуль переиспользуется источниками «принесли в окно» (drag & drop, сессия 12) и
//! «передали из второго экземпляра» (сессия 13): оба сводятся к [`ExternalOpenRequest`](request::ExternalOpenRequest).
//!
//! Конвейер (каждый этап — отдельный файл и отдельный владелец инварианта):
//!
//! ```text
//! winit_source ──DropGestureEvent──▶ gesture ──ExternalOpenRequest──▶ classify
//!   (единственный winit-aware файл)   (один бросок = один запрос)        │ ExternalOpenPlan
//!                                                                         ▼
//!                       dispatch ◀──OpenStep── route (чистая таблица решений)
//!                          │
//!                          ▼ ExternalOpenHost (реализует AppState: существующие пути)
//! ```
//!
//! Куда попал бросок, решает [`target`] по прямоугольнику панели плейлиста, который
//! публикует UI каждый кадр. Подсветку рисует `ui::external_drop_overlay`.
//!
//! Что НЕ делает модуль: не открывает media сам (только через [`ExternalOpenHost`]),
//! не читает содержимое файлов и не хранит пути в логах.

pub(crate) mod classify;
pub(crate) mod dispatch;
pub(crate) mod event;
pub(crate) mod gesture;
pub(crate) mod notice;
pub(crate) mod owner;
pub(crate) mod playlist_intent;
pub(crate) mod request;
pub(crate) mod route;
pub(crate) mod target;
pub(crate) mod uri;
pub(crate) mod winit_source;

pub(crate) use dispatch::{ExternalOpenHost, dispatch_external_open_request};
pub(crate) use event::DropGestureEvent;
pub(crate) use notice::DropNotice;
pub(crate) use owner::{DropOverlay, ExternalOpenOwner};
pub(crate) use request::DropTarget;

#[cfg(test)]
pub(crate) mod pipeline_tests;
