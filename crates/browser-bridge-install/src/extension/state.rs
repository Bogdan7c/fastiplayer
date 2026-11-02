//! Состояние экстеншена в каждом найденном браузере и то, что сделает кнопка Install.
//!
//! Состояние считается заново при каждом обзоре из файлов профилей (только чтение) и
//! из нашей подсказки: ничего не запоминается, удалили руками — статус честно
//! изменился.

use std::path::PathBuf;

use browser_bridge_protocol::extension_identity::{
    BrowserFamily, CHROMIUM_DEVELOPMENT_EXTENSION_ID,
};

use crate::catalog::{
    BrowserTarget, BrowserTargetId, ChromiumDelivery, ExtensionSupport, ExtensionUnsupportedReason,
};
use crate::detection::Installation;
use crate::environment::InstallEnvironment;
use crate::flatpak::FlatpakError;
use crate::status::PackagingKind;

use super::hint::{HintState, hint_path, inspect_hint};
use super::listing::ExtensionListings;
use super::presence::{
    ChromiumExtensionIds, Presence, Unreadable, UnreadableKind, UserUninstall,
    read_chromium_presence, read_firefox_presence,
};

/// Почему в этом браузере экстеншен не ставится.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionUnsupported {
    /// Snap-браузер: песочница не даёт ни моста, ни нашей подсказки.
    Snap,
    /// Мост для Flatpak-Firefox не поддерживается.
    FlatpakFirefoxFamily,
    /// В браузере нет веб-магазина.
    NoWebStore,
    /// Ошибка каталога: корень профилей Flatpak у пакетной цели. Тест каталога
    /// следит, чтобы этого не было; вариант нужен, чтобы не паниковать.
    CatalogMisconfigured,
}

impl From<ExtensionUnsupportedReason> for ExtensionUnsupported {
    fn from(reason: ExtensionUnsupportedReason) -> Self {
        match reason {
            ExtensionUnsupportedReason::FlatpakFirefoxFamily => Self::FlatpakFirefoxFamily,
            ExtensionUnsupportedReason::NoWebStore => Self::NoWebStore,
        }
    }
}

/// Где сейчас экстеншен в этом браузере.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionState {
    /// Стоит и включён.
    Installed,
    /// Стоит, но отключён: включить может только человек в браузере.
    InstalledButDisabled,
    /// Наша подсказка на месте, браузер поставит экстеншен при следующем запуске.
    WaitingForBrowserRestart,
    /// Человек удалил экстеншен в браузере: тихий путь для него закрыт навсегда,
    /// остаётся страница магазина.
    RemovedInBrowser,
    /// Не стоит.
    NotInstalled,
    /// Для этого семейства браузеров экстеншен ещё не опубликован в магазине.
    AwaitingPublication,
    /// Ставить нельзя.
    Unsupported(ExtensionUnsupported),
    /// Файл профиля не прочитался, состояние неизвестно.
    Unreadable {
        /// Файл или каталог.
        path: PathBuf,
        /// Причина.
        kind: UnreadableKind,
    },
}

/// Что сделает установка для этого браузера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    /// Плеер кладёт подсказку; браузер сам поставит экстеншен при следующем запуске.
    InstallSilently,
    /// Плеер открывает страницу магазина в этом браузере; «Add» жмёт человек. Это же
    /// запасной путь, если браузер подсказку проигнорировал или лежит чужой файл.
    OpenStorePage,
}

/// Строка обзора: один найденный браузер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionReport {
    /// ID цели для действий.
    pub target: BrowserTargetId,
    /// Имя для интерфейса.
    pub display_name: &'static str,
    /// Пакет или Flatpak.
    pub packaging: PackagingKind,
    /// Где экстеншен.
    pub state: ExtensionState,
    /// Что сделает кнопка Install (`None` — кнопка не нужна).
    pub install_action: Option<InstallAction>,
    /// Что лежит под именем нашей подсказки. Отмена установки предлагается, только
    /// пока экстеншен ещё не стоит и подсказка [`HintState::Ours`]. Если подсказку прочитать не удалось, здесь
    /// `Absent`, а причина — в [`ExtensionState::Unreadable`].
    pub hint: HintState,
}

/// Результат обзора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSurvey {
    /// Найденные браузеры (не установленные в обзор не попадают).
    pub browsers: Vec<ExtensionReport>,
    /// Сбой утилиты `flatpak` (Flatpak-цели при этом не показываются).
    pub flatpak_problem: Option<FlatpakError>,
}

/// Результат оценки одной цели.
pub(crate) struct Assessment {
    pub state: ExtensionState,
    pub install_action: Option<InstallAction>,
    pub hint: HintState,
}

/// Всё, что нужно оценке, кроме самой цели.
pub(crate) struct AssessmentInputs<'a> {
    pub environment: &'a InstallEnvironment,
    pub listings: &'a ExtensionListings,
}

/// Оценивает цель. Только чтение файлов профилей и нашей подсказки.
pub(crate) fn assess(
    inputs: &AssessmentInputs<'_>,
    target: &BrowserTarget,
    installation: &Installation,
) -> Assessment {
    if matches!(installation, Installation::Snap { .. }) {
        return unsupported(ExtensionUnsupported::Snap);
    }
    match target.extension {
        ExtensionSupport::NotSupported(reason) => unsupported(reason.into()),
        ExtensionSupport::Chromium {
            user_data,
            delivery,
        } => {
            let Some(user_data) = user_data.resolve(inputs.environment, target.flatpak_app_id())
            else {
                return unsupported(ExtensionUnsupported::CatalogMisconfigured);
            };
            assess_chromium(inputs.listings, &user_data, delivery)
        }
        ExtensionSupport::Firefox { profile_roots } => {
            let roots: Vec<PathBuf> = profile_roots
                .iter()
                .filter_map(|root| root.resolve(inputs.environment, target.flatpak_app_id()))
                .collect();
            assess_firefox(inputs.listings, &roots)
        }
    }
}

fn unsupported(reason: ExtensionUnsupported) -> Assessment {
    Assessment {
        state: ExtensionState::Unsupported(reason),
        install_action: None,
        hint: HintState::Absent,
    }
}

fn unreadable(failure: Unreadable) -> Assessment {
    Assessment {
        state: ExtensionState::Unreadable {
            path: failure.path,
            kind: failure.kind,
        },
        install_action: None,
        hint: HintState::Absent,
    }
}

fn assess_chromium(
    listings: &ExtensionListings,
    user_data: &std::path::Path,
    delivery: ChromiumDelivery,
) -> Assessment {
    let store_id = listings.chrome_web_store_id();
    let ids = ChromiumExtensionIds {
        store: store_id,
        development: CHROMIUM_DEVELOPMENT_EXTENSION_ID,
    };
    let presence = match read_chromium_presence(user_data, ids) {
        Ok(presence) => presence,
        Err(failure) => return unreadable(failure),
    };
    // Подсказку читаем, только если знаем её имя и этот браузер её понимает.
    let hint = match (store_id, delivery) {
        (Some(store_id), ChromiumDelivery::ExternalHint) => {
            match inspect_hint(&hint_path(user_data, store_id)) {
                Ok(hint) => hint,
                Err(failure) => {
                    return unreadable(Unreadable {
                        path: failure.path,
                        kind: UnreadableKind::Io(failure.kind),
                    });
                }
            }
        }
        _ => HintState::Absent,
    };
    let state = match presence.presence {
        Presence::Enabled => ExtensionState::Installed,
        Presence::Disabled => ExtensionState::InstalledButDisabled,
        Presence::Absent if store_id.is_none() => ExtensionState::AwaitingPublication,
        Presence::Absent if presence.user_uninstall == UserUninstall::Recorded => {
            ExtensionState::RemovedInBrowser
        }
        Presence::Absent if hint == HintState::Ours => ExtensionState::WaitingForBrowserRestart,
        Presence::Absent => ExtensionState::NotInstalled,
    };
    let install_action = chromium_install_action(&state, delivery, hint);
    Assessment {
        state,
        install_action,
        hint,
    }
}

/// Что делает кнопка Install в Chromium-браузере.
///
/// Тихая подсказка предлагается, только когда место под неё свободно: чужой файл мы
/// не перезаписываем, а уже лежащая своя подсказка повторной записью ничего не
/// изменит (браузер её проигнорировал или ещё не перезапускался) — в обоих случаях
/// остаётся страница магазина.
fn chromium_install_action(
    state: &ExtensionState,
    delivery: ChromiumDelivery,
    hint: HintState,
) -> Option<InstallAction> {
    match (state, delivery, hint) {
        (ExtensionState::NotInstalled, ChromiumDelivery::ExternalHint, HintState::Absent) => {
            Some(InstallAction::InstallSilently)
        }
        (ExtensionState::NotInstalled, _, _)
        | (ExtensionState::WaitingForBrowserRestart, _, _)
        | (ExtensionState::RemovedInBrowser, _, _) => Some(InstallAction::OpenStorePage),
        _ => None,
    }
}

fn assess_firefox(listings: &ExtensionListings, roots: &[PathBuf]) -> Assessment {
    let presence = match read_firefox_presence(roots) {
        Ok(presence) => presence,
        Err(failure) => return unreadable(failure),
    };
    let published = listings.is_published_for(BrowserFamily::Firefox);
    let (state, install_action) = match presence {
        Presence::Enabled => (ExtensionState::Installed, None),
        Presence::Disabled => (ExtensionState::InstalledButDisabled, None),
        Presence::Absent if !published => (ExtensionState::AwaitingPublication, None),
        Presence::Absent => (
            ExtensionState::NotInstalled,
            Some(InstallAction::OpenStorePage),
        ),
    };
    Assessment {
        state,
        install_action,
        hint: HintState::Absent,
    }
}
