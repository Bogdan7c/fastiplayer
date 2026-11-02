//! «Что записал бы этот плеер»: полный набор ожидаемых файлов для одной цели.
//!
//! Статус и установка считают от одного и того же значения, поэтому «подключено»
//! по определению означает «на диске то же, что записал бы Connect сейчас».

use std::path::PathBuf;

use desktop_integration::{ActivationServiceFileError, browser_session_activation_service_file};
use thiserror::Error;

use crate::catalog::BrowserTarget;
use crate::environment::InstallEnvironment;
use crate::layout;
use crate::manifest::{ManifestError, build_manifest};
use crate::source::InstallSource;

/// Почему ожидаемые файлы построить нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExpectationError {
    /// Файл подключения не собрать.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// `.service` для пути этого плеера не собрать.
    #[error("The player path cannot be used for D-Bus activation: {0}")]
    PlayerPath(ActivationServiceFileError),
}

/// Ожидаемые файлы одной цели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedInstall {
    /// Плеер, которого будит шина.
    pub player: PathBuf,
    /// Откуда копируется посредник.
    pub host_source: PathBuf,
    /// Куда копируется посредник.
    pub host_path: PathBuf,
    /// Пути файлов подключения.
    pub manifest_paths: Vec<PathBuf>,
    /// Содержимое файла подключения.
    pub manifest_text: String,
    /// Путь `.service`.
    pub service_path: PathBuf,
    /// Содержимое `.service`.
    pub service_text: String,
}

impl ExpectedInstall {
    /// Ожидаемое для отключения и обзора БЕЗ годного источника: есть только то, что
    /// не зависит от посредника рядом с плеером (пути и файл подключения).
    /// `host_source`, `player` и `service_text` пусты: сравнивать с ними нельзя.
    pub fn without_source(
        environment: &InstallEnvironment,
        target: &BrowserTarget,
    ) -> Result<Self, ExpectationError> {
        let host_path = layout::host_install_path(environment, target);
        Ok(Self {
            player: PathBuf::new(),
            host_source: PathBuf::new(),
            manifest_text: build_manifest(target.family, &host_path)?,
            manifest_paths: layout::manifest_file_paths(environment, target),
            service_path: layout::service_file_path(environment),
            service_text: String::new(),
            host_path,
        })
    }

    /// Строит ожидаемое для цели.
    pub fn build(
        environment: &InstallEnvironment,
        source: &InstallSource,
        target: &BrowserTarget,
    ) -> Result<Self, ExpectationError> {
        let host_path = layout::host_install_path(environment, target);
        let manifest_text = build_manifest(target.family, &host_path)?;
        let service_text = browser_session_activation_service_file(source.player_executable())
            .map_err(ExpectationError::PlayerPath)?;
        Ok(Self {
            player: source.player_executable().to_path_buf(),
            host_source: source.host_executable().to_path_buf(),
            manifest_paths: layout::manifest_file_paths(environment, target),
            host_path,
            manifest_text,
            service_path: layout::service_file_path(environment),
            service_text,
        })
    }
}
