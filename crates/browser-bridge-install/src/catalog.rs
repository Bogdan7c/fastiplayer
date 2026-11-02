//! Каталог целей: одна строка = браузер × упаковка (пакет или Flatpak).
//!
//! Таблица статическая и ничего не знает о диске: где искать браузер, куда он читает
//! файл подключения и какого он семейства. Семейство берётся из
//! `browser_bridge_protocol::extension_identity` — от него зависит формат файла
//! (`allowed_origins` у Chromium, `allowed_extensions` у Firefox).
//!
//! Сессия 07a (тихая установка экстеншена) использует эту же таблицу: семейство,
//! упаковка и app ID те же, добавится лишь второй установщик рядом.

use std::path::PathBuf;

use browser_bridge_protocol::extension_identity::BrowserFamily;

use crate::environment::InstallEnvironment;

// Короткие имена вариантов нужны только строкам таблицы ниже.
use BrowserFamily::{Chromium, Firefox};
use ManifestRoot::{FlatpakConfig, HomeDot, XdgConfig};
use Verification::{NotVerified, Verified};

/// Подкаталог файлов подключения у Chromium-семейства.
const CHROMIUM_MANIFEST_SUBDIRECTORY: &str = "NativeMessagingHosts";
/// Подкаталог файлов подключения у Firefox-семейства.
const FIREFOX_MANIFEST_SUBDIRECTORY: &str = "native-messaging-hosts";

/// Стабильный идентификатор цели (`firefox`, `google-chrome-flatpak`).
///
/// Приложение хранит и передаёт его как непрозрачную строку.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserTargetId(String);

impl BrowserTargetId {
    /// Идентификатор из строки (значение не проверяется: неизвестный ID даст ошибку при действии).
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Строковое значение.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Как браузер установлен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packaging {
    /// Системный пакет или распаковка: исполняемый файл ищется в `PATH`.
    System {
        /// Возможные имена исполняемого файла.
        executables: &'static [&'static str],
    },
    /// Flatpak-приложение.
    Flatpak {
        /// Идентификатор приложения (`com.google.Chrome`).
        app_id: &'static str,
    },
}

/// Проверена ли цель вживую (решение 160).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// Проверено вживую владельцем.
    Verified,
    /// Путь взят из документации браузера, вживую не проверялся.
    NotVerified,
}

/// Откуда отсчитывается каталог файла подключения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestRoot {
    /// `$XDG_CONFIG_HOME/<имя>` (Chromium-пакеты и дополнительный путь Firefox).
    XdgConfig(&'static str),
    /// `~/<имя>` (Firefox, LibreWolf, Waterfox).
    HomeDot(&'static str),
    /// `~/.var/app/<app id>/config/<имя>` (Flatpak-Chromium).
    FlatpakConfig(&'static str),
}

/// Одна цель каталога.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserTarget {
    /// Стабильный ID.
    pub id: &'static str,
    /// Имя для интерфейса (английское).
    pub display_name: &'static str,
    /// Семейство браузера: формат файла подключения.
    pub family: BrowserFamily,
    /// Пакет или Flatpak.
    pub packaging: Packaging,
    /// Куда браузер читает файлы подключения. Пусто = цель не поддерживается.
    pub manifest_roots: &'static [ManifestRoot],
    /// Проверена ли вживую.
    pub verification: Verification,
}

impl BrowserTarget {
    /// Идентификатор цели как значение для приложения.
    pub fn target_id(&self) -> BrowserTargetId {
        BrowserTargetId::new(self.id)
    }

    /// Flatpak-Firefox пока не умеет `xdg-native-messaging-proxy` (bug 2078349):
    /// файл подключения из песочницы не виден, поэтому цель только помечается.
    pub fn is_flatpak_firefox_family(&self) -> bool {
        matches!(self.packaging, Packaging::Flatpak { .. }) && self.family == BrowserFamily::Firefox
    }

    /// Идентификатор Flatpak-приложения, если цель Flatpak.
    pub fn flatpak_app_id(&self) -> Option<&'static str> {
        match self.packaging {
            Packaging::Flatpak { app_id } => Some(app_id),
            Packaging::System { .. } => None,
        }
    }

    /// Каталоги, в которые браузер ищет файл подключения (в порядке каталога).
    pub fn manifest_directories(&self, environment: &InstallEnvironment) -> Vec<PathBuf> {
        let subdirectory = match self.family {
            BrowserFamily::Chromium => CHROMIUM_MANIFEST_SUBDIRECTORY,
            BrowserFamily::Firefox => FIREFOX_MANIFEST_SUBDIRECTORY,
        };
        let flatpak_config = self
            .flatpak_app_id()
            .map(|app_id| environment.flatpak_app_dir(app_id).join("config"));
        self.manifest_roots
            .iter()
            .filter_map(|root| match root {
                ManifestRoot::XdgConfig(name) => Some(environment.xdg_config_home.join(name)),
                ManifestRoot::HomeDot(name) => Some(environment.home.join(name)),
                ManifestRoot::FlatpakConfig(name) => {
                    flatpak_config.as_ref().map(|config| config.join(name))
                }
            })
            .map(|root| root.join(subdirectory))
            .collect()
    }
}

/// Сокращение для строки таблицы пакетной цели.
const fn system(
    id: &'static str,
    display_name: &'static str,
    family: BrowserFamily,
    executables: &'static [&'static str],
    manifest_roots: &'static [ManifestRoot],
    verification: Verification,
) -> BrowserTarget {
    BrowserTarget {
        id,
        display_name,
        family,
        packaging: Packaging::System { executables },
        manifest_roots,
        verification,
    }
}

/// Сокращение для строки таблицы Flatpak-цели.
const fn flatpak(
    id: &'static str,
    display_name: &'static str,
    family: BrowserFamily,
    app_id: &'static str,
    manifest_roots: &'static [ManifestRoot],
    verification: Verification,
) -> BrowserTarget {
    BrowserTarget {
        id,
        display_name,
        family,
        packaging: Packaging::Flatpak { app_id },
        manifest_roots,
        verification,
    }
}

/// Все цели. Порядок = порядок показа в интерфейсе.
pub static TARGETS: &[BrowserTarget] = &[
    // Пакеты и распаковки: Firefox читает ГЛАВНЫЙ `~/.mozilla/native-messaging-hosts`
    // (Firefox 147+ только его), XDG-путь пишется дополнительно.
    system(
        "firefox",
        "Firefox",
        Firefox,
        &["firefox"],
        &[HomeDot(".mozilla"), XdgConfig("mozilla")],
        Verified,
    ),
    system(
        "chromium",
        "Chromium",
        Chromium,
        &["chromium"],
        &[XdgConfig("chromium")],
        Verified,
    ),
    system(
        "google-chrome",
        "Google Chrome",
        Chromium,
        &["google-chrome-stable", "google-chrome"],
        &[XdgConfig("google-chrome")],
        NotVerified,
    ),
    system(
        "brave",
        "Brave",
        Chromium,
        &["brave", "brave-browser"],
        &[XdgConfig("BraveSoftware/Brave-Browser")],
        NotVerified,
    ),
    system(
        "vivaldi",
        "Vivaldi",
        Chromium,
        &["vivaldi", "vivaldi-stable"],
        &[XdgConfig("vivaldi")],
        NotVerified,
    ),
    system(
        "microsoft-edge",
        "Microsoft Edge",
        Chromium,
        &["microsoft-edge-stable", "microsoft-edge"],
        &[XdgConfig("microsoft-edge")],
        NotVerified,
    ),
    system(
        "librewolf",
        "LibreWolf",
        Firefox,
        &["librewolf"],
        &[HomeDot(".librewolf")],
        NotVerified,
    ),
    system(
        "waterfox",
        "Waterfox",
        Firefox,
        &["waterfox"],
        &[HomeDot(".waterfox")],
        NotVerified,
    ),
    // Flatpak, Chromium-семейство: файл лежит в `~/.var/app/<id>/config/<имя>/`.
    flatpak(
        "google-chrome-flatpak",
        "Google Chrome (Flatpak)",
        Chromium,
        "com.google.Chrome",
        &[FlatpakConfig("google-chrome")],
        Verified,
    ),
    flatpak(
        "chromium-flatpak",
        "Chromium (Flatpak)",
        Chromium,
        "org.chromium.Chromium",
        &[FlatpakConfig("chromium")],
        NotVerified,
    ),
    flatpak(
        "ungoogled-chromium-flatpak",
        "Ungoogled Chromium (Flatpak)",
        Chromium,
        "io.github.ungoogled_software.ungoogled_chromium",
        &[FlatpakConfig("chromium")],
        NotVerified,
    ),
    flatpak(
        "brave-flatpak",
        "Brave (Flatpak)",
        Chromium,
        "com.brave.Browser",
        &[FlatpakConfig("BraveSoftware/Brave-Browser")],
        NotVerified,
    ),
    flatpak(
        "vivaldi-flatpak",
        "Vivaldi (Flatpak)",
        Chromium,
        "com.vivaldi.Vivaldi",
        &[FlatpakConfig("vivaldi")],
        NotVerified,
    ),
    flatpak(
        "microsoft-edge-flatpak",
        "Microsoft Edge (Flatpak)",
        Chromium,
        "com.microsoft.Edge",
        &[FlatpakConfig("microsoft-edge")],
        NotVerified,
    ),
    // Flatpak, Firefox-семейство: пока не поддерживается, файлов нет (manifest_roots пуст).
    flatpak(
        "firefox-flatpak",
        "Firefox (Flatpak)",
        Firefox,
        "org.mozilla.firefox",
        &[],
        NotVerified,
    ),
    flatpak(
        "librewolf-flatpak",
        "LibreWolf (Flatpak)",
        Firefox,
        "io.gitlab.librewolf-community",
        &[],
        NotVerified,
    ),
    flatpak(
        "waterfox-flatpak",
        "Waterfox (Flatpak)",
        Firefox,
        "net.waterfox.waterfox",
        &[],
        NotVerified,
    ),
];

/// Цель по ID.
pub fn find_target(id: &BrowserTargetId) -> Option<&'static BrowserTarget> {
    TARGETS.iter().find(|target| target.id == id.as_str())
}
