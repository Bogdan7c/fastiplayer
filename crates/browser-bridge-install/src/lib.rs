//! Установщик моста «браузер качает — мы играем» (issue #2, сессия 06).
//!
//! Что делает: находит браузеры пользователя (пакеты и Flatpak, Snap — только
//! распознаёт и честно говорит «не поддерживается»), по команде «Connect» кладёт
//! посредника `fastiplayer-browser-host`, файл подключения браузера (native messaging
//! manifest) и `.service` D-Bus activation, а по «Disconnect» убирает только своё.
//! UI здесь нет: приложение вызывает [`BrowserBridgeInstaller`] из фонового потока
//! (все методы блокирующие).
//!
//! # Главная идея статуса
//!
//! Статус браузера — это сравнение «что записал бы ЭТОТ плеер» с тем, что лежит на
//! диске ([`BrowserBridgeStatus`]). Ничего не запоминается в базе: удалили файл
//! руками — статус честно изменился. Файлами владеет копия плеера, последней
//! нажавшая «Connect» (dev-сборка и установленная делят одни пути).
//!
//! # Карта модулей (каждый владеет одним)
//!
//! - [`environment`] — HOME, XDG, PATH и snap-каталоги (тесты строят фейковые);
//! - [`source`] — путь плеера и посредника РЯДОМ с ним, проблемы источника;
//! - [`catalog`] — статическая таблица «браузер × упаковка»;
//! - [`detection`] — установлен ли браузер (исполняемый файл / Flatpak-приложение);
//! - [`flatpak`] — шов к утилите `flatpak` и разбор прав песочницы;
//! - [`layout`] — где что лежит на диске;
//! - [`manifest`] — сборка и разбор файла подключения (только из `extension_identity`);
//! - [`expected`] — «что записал бы этот плеер» целиком;
//! - [`inspection`] — чтение и классификация того, что лежит на диске;
//! - [`status`] — обзор [`BrowserSurvey`] и [`BrowserBridgeStatus`];
//! - [`install`], [`disconnect`], [`allow`] — действия; [`error`] — их ошибки и
//!   предупреждения; [`reloader`] — шов к шине;
//! - [`facade`] — [`BrowserBridgeInstaller`], единственная точка входа приложения.
//!
//! # Что здесь НЕ делается (решение 161)
//!
//! Тихая установка самого экстеншена — сессия 07a. Она добавит установщик экстеншена
//! рядом, на том же каталоге: семейство, упаковка и app ID уже есть в [`catalog`].
//! Windows: вместо файлов в `~/.config` там реестр; шов — [`facade`] и [`catalog`],
//! а [`BrowserBridgeInstaller::survey`] пока отвечает «платформа не поддерживается».

#![forbid(unsafe_code)]

pub mod allow;
pub mod catalog;
pub mod detection;
pub mod disconnect;
pub mod environment;
pub mod error;
pub mod expected;
pub mod facade;
pub mod flatpak;
mod fs_util;
mod host_copy;
pub mod inspection;
pub mod install;
pub mod layout;
pub mod manifest;
pub mod reloader;
pub mod source;
pub mod status;

pub use allow::AllowError;
pub use catalog::{BrowserTarget, BrowserTargetId, Packaging, TARGETS, Verification, find_target};
pub use desktop_integration::SessionBusReloadError;
pub use disconnect::DisconnectWarning;
pub use environment::{EnvironmentError, InstallEnvironment};
pub use error::{InstallError, InstallErrorKind, InstallStep, InstallWarning, UnsupportedReason};
pub use facade::{AllowOutcome, BrowserBridgeInstaller, ConnectOutcome, DisconnectOutcome};
pub use flatpak::{
    BRIDGE_TALK_NAME_MASK, FlatpakBusAccess, FlatpakError, FlatpakTool, RealFlatpakTool,
    parse_bus_access,
};
pub use reloader::{DesktopSessionBusReloader, SessionBusReloader};
pub use source::{InstallSource, SourceProblem};
pub use status::{
    BrowserBridgeStatus, BrowserReport, BrowserSurvey, PackagingKind, SurveyError,
    UnavailableReason,
};

#[cfg(test)]
mod tests;
