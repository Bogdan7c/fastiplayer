//! Фасад установщика экстеншена: единственная точка входа приложения.
//!
//! Все методы блокирующие (файлы, процесс `flatpak`, запуск браузера): приложение
//! вызывает их из фонового потока. Швы (окружение, страницы магазинов, `flatpak`,
//! запуск браузера) передаются в конструктор, поэтому тесты подставляют фейки.
//!
//! # Где здесь шов под Windows
//!
//! Обзор и установка пока отвечают [`ExtensionInstallError::UnsupportedPlatform`]
//! вне Linux. На Windows будет другой «тихий» путь: ключ реестра
//! `HKCU\\Software\\Google\\Chrome\\Extensions\\<ID>` со значением `update_url` на
//! сервер обновлений Chrome Web Store, плюс та же страница магазина для остальных
//! браузеров. Менять придётся `assess` и ветки `install`/`cancel_pending_install`, каталог
//! и тип отчёта останутся прежними.

use crate::catalog::{BrowserTarget, BrowserTargetId, ExtensionSupport, TARGETS, find_target};
use crate::detection::{Installation, detect_installation};
use crate::environment::InstallEnvironment;
use crate::flatpak::FlatpakTool;
use crate::status::SurveyError;

use super::error::ExtensionInstallError;
use super::hint::{RemoveHintOutcome, remove_hint, write_hint};
use super::launch::{BrowserLaunchCommand, BrowserLauncher};
use super::listing::ExtensionListings;
use super::state::{
    Assessment, AssessmentInputs, ExtensionReport, ExtensionState, ExtensionSurvey, InstallAction,
    assess,
};

/// Итог установки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionInstallOutcome {
    /// Экстеншен уже стоит и включён, ничего не сделано.
    AlreadyInstalled,
    /// Стоит, но отключён: включить может только человек в браузере. Ничего не сделано.
    AlreadyInstalledButDisabled,
    /// Подсказка записана: браузер поставит экстеншен при СЛЕДУЮЩЕМ запуске.
    ///
    /// Записывается только на свободное место: чужой файл под этим именем мы не
    /// перезаписываем (в этом случае открывается страница магазина).
    HintWritten {
        /// Файл подсказки.
        hint_path: std::path::PathBuf,
    },
    /// Страница магазина открыта в этом браузере; «Add» жмёт человек.
    StorePageOpened {
        /// Открытый адрес.
        url: String,
    },
}

/// Установщик экстеншена Fastiplayer Bridge в браузеры пользователя.
pub struct ExtensionInstaller {
    environment: InstallEnvironment,
    listings: ExtensionListings,
    flatpak: Box<dyn FlatpakTool>,
    launcher: Box<dyn BrowserLauncher>,
}

impl ExtensionInstaller {
    /// Собирает установщик из швов.
    pub fn new(
        environment: InstallEnvironment,
        listings: ExtensionListings,
        flatpak: Box<dyn FlatpakTool>,
        launcher: Box<dyn BrowserLauncher>,
    ) -> Self {
        Self {
            environment,
            listings,
            flatpak,
            launcher,
        }
    }

    /// Обзор: все найденные браузеры каталога и состояние экстеншена в каждом.
    /// Ничего не пишет.
    pub fn survey(&self) -> Result<ExtensionSurvey, SurveyError> {
        if !cfg!(target_os = "linux") {
            return Err(SurveyError::UnsupportedPlatform);
        }
        let (flatpak_apps, flatpak_problem) = match self.flatpak.installed_apps() {
            Ok(apps) => (apps, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let inputs = self.inputs();
        let browsers = TARGETS
            .iter()
            .filter_map(|target| {
                let installation = detect_installation(target, &self.environment, &flatpak_apps);
                if installation == Installation::Absent {
                    return None;
                }
                let Assessment {
                    state,
                    install_action,
                    hint,
                } = assess(&inputs, target, &installation);
                Some(ExtensionReport {
                    target: target.target_id(),
                    display_name: target.display_name,
                    packaging: crate::status::packaging_kind(target),
                    state,
                    install_action,
                    hint,
                })
            })
            .collect();
        Ok(ExtensionSurvey {
            browsers,
            flatpak_problem,
        })
    }

    /// Кнопка Install: кладёт подсказку или открывает страницу магазина в этом браузере.
    pub fn install(
        &self,
        target_id: &BrowserTargetId,
    ) -> Result<ExtensionInstallOutcome, ExtensionInstallError> {
        let target = self.resolve_target(target_id)?;
        let installation = self.detect(target)?;
        if installation == Installation::Absent {
            return Err(ExtensionInstallError::BrowserNotInstalled);
        }
        let assessment = assess(&self.inputs(), target, &installation);
        match assessment.state {
            ExtensionState::Unsupported(reason) => Err(ExtensionInstallError::Unsupported(reason)),
            ExtensionState::AwaitingPublication => Err(ExtensionInstallError::NotPublished),
            ExtensionState::Unreadable { path, kind } => {
                Err(ExtensionInstallError::ProfileUnreadable { path, kind })
            }
            ExtensionState::Installed => Ok(ExtensionInstallOutcome::AlreadyInstalled),
            ExtensionState::InstalledButDisabled => {
                Ok(ExtensionInstallOutcome::AlreadyInstalledButDisabled)
            }
            ExtensionState::WaitingForBrowserRestart
            | ExtensionState::RemovedInBrowser
            | ExtensionState::NotInstalled => match assessment.install_action {
                Some(InstallAction::InstallSilently) => self.write_hint_for(target),
                Some(InstallAction::OpenStorePage) => self.open_store_page(target, &installation),
                // Состояние требует действия, а оценка его не назвала: баг каталога.
                None => Err(ExtensionInstallError::Unsupported(
                    super::state::ExtensionUnsupported::CatalogMisconfigured,
                )),
            },
        }
    }

    /// Кнопка Cancel: отменяет ещё не случившуюся установку — удаляет ТОЛЬКО нашу
    /// подсказку, пока браузер её не обработал.
    ///
    /// Уже установленный экстеншен это НЕ убирает (проверено вживую: Chromium не
    /// удаляет экстеншен при исчезновении подсказки): убрать его может только человек
    /// на странице расширений браузера, Fastiplayer этого не умеет. Браузер для
    /// отмены не обязан быть установленным: уборка после его удаления тоже работает.
    pub fn cancel_pending_install(
        &self,
        target_id: &BrowserTargetId,
    ) -> Result<RemoveHintOutcome, ExtensionInstallError> {
        let target = self.resolve_target(target_id)?;
        let (user_data, store_id) = self.hint_location(target)?;
        remove_hint(&user_data, store_id)
    }

    fn resolve_target(
        &self,
        target_id: &BrowserTargetId,
    ) -> Result<&'static BrowserTarget, ExtensionInstallError> {
        if !cfg!(target_os = "linux") {
            return Err(ExtensionInstallError::UnsupportedPlatform);
        }
        find_target(target_id).ok_or(ExtensionInstallError::UnknownTarget)
    }

    fn write_hint_for(
        &self,
        target: &BrowserTarget,
    ) -> Result<ExtensionInstallOutcome, ExtensionInstallError> {
        let (user_data, store_id) = self.hint_location(target)?;
        let hint_path = write_hint(&user_data, store_id)?;
        Ok(ExtensionInstallOutcome::HintWritten { hint_path })
    }

    fn open_store_page(
        &self,
        target: &BrowserTarget,
        installation: &Installation,
    ) -> Result<ExtensionInstallOutcome, ExtensionInstallError> {
        let url = self
            .listings
            .store_page_url(target.family)
            .ok_or(ExtensionInstallError::NotPublished)?;
        let command = match (installation, target.flatpak_app_id()) {
            (Installation::System { executable }, _) => {
                BrowserLaunchCommand::Executable(executable.clone())
            }
            (Installation::Flatpak, Some(app_id)) => BrowserLaunchCommand::Flatpak {
                app_id: app_id.to_owned(),
            },
            // Snap и «нет браузера» отсеяны оценкой раньше.
            _ => return Err(ExtensionInstallError::BrowserNotInstalled),
        };
        self.launcher
            .open_url(&command, &url)
            .map_err(ExtensionInstallError::Launch)?;
        Ok(ExtensionInstallOutcome::StorePageOpened { url })
    }

    /// Папка профилей и ID магазина, по которым строится подсказка.
    fn hint_location(
        &self,
        target: &BrowserTarget,
    ) -> Result<(std::path::PathBuf, &str), ExtensionInstallError> {
        let ExtensionSupport::Chromium {
            user_data,
            delivery: crate::catalog::ChromiumDelivery::ExternalHint,
        } = target.extension
        else {
            return match target.extension {
                ExtensionSupport::NotSupported(reason) => {
                    Err(ExtensionInstallError::Unsupported(reason.into()))
                }
                _ => Err(ExtensionInstallError::NoHintForThisBrowser),
            };
        };
        let store_id = self
            .listings
            .chrome_web_store_id()
            .ok_or(ExtensionInstallError::NotPublished)?;
        let user_data = user_data
            .resolve(&self.environment, target.flatpak_app_id())
            .ok_or(ExtensionInstallError::Unsupported(
                super::state::ExtensionUnsupported::CatalogMisconfigured,
            ))?;
        Ok((user_data, store_id))
    }

    fn inputs(&self) -> AssessmentInputs<'_> {
        AssessmentInputs {
            environment: &self.environment,
            listings: &self.listings,
        }
    }

    /// Как установлена одна цель. Сбой `flatpak` у Flatpak-цели — отдельная ошибка
    /// [`ExtensionInstallError::FlatpakUnavailable`], а не «браузера нет»; пакетной
    /// цели утилита не нужна.
    fn detect(&self, target: &BrowserTarget) -> Result<Installation, ExtensionInstallError> {
        let flatpak_apps = if target.flatpak_app_id().is_some() {
            self.flatpak
                .installed_apps()
                .map_err(ExtensionInstallError::FlatpakUnavailable)?
        } else {
            Vec::new()
        };
        Ok(detect_installation(
            target,
            &self.environment,
            &flatpak_apps,
        ))
    }
}
