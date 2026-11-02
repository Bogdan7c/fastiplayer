//! Тестовый стенд моста в браузер (сессия 08 плана `user/browser-bridge/`).
//!
//! Модули и их владельцы:
//! - [`media`] — генерация тестовых роликов ffmpeg-ом в кэш и каталог `catalog.json`;
//! - [`site`] — HTTP-сервер стенда на двух origin, отчёты страниц;
//! - [`environment`] — временный HOME, частная шина D-Bus, невидимый экран, установка
//!   посредника настоящим установщиком;
//! - [`browsers`] — запуск Chromium и Firefox с экстеншенами и навигация;
//! - [`observe`] — чтение журнала плеера и посредника;
//! - [`scenarios`] — сценарии прогона (проверка страниц, дымок, статус экстеншена).

pub mod browsers;
pub mod environment;
pub mod media;
pub mod observe;
pub mod scenarios;
pub mod site;
