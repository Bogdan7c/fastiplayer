//! Каталог целей: одна строка = браузер × упаковка (пакет или Flatpak).
//!
//! Таблица статическая и ничего не знает о диске: где искать браузер, куда он читает
//! файл подключения и какого он семейства. Семейство берётся из
//! `browser_bridge_protocol::extension_identity` — от него зависит формат файла
//! (`allowed_origins` у Chromium, `allowed_extensions` у Firefox).
//!
//! Установщик экстеншена (сессия 07a, модуль `extension`) использует эту же таблицу:
//! семейство, упаковка и app ID те же, а поле [`BrowserTarget::extension`] говорит,
//! где лежат профили браузера и каким способом экстеншен доставляется.

use std::path::PathBuf;

use browser_bridge_protocol::extension_identity::BrowserFamily;

use crate::environment::InstallEnvironment;

// Короткие имена вариантов нужны только строкам таблицы ниже.
use BrowserFamily::{Chromium, Firefox};
use ChromiumDelivery::{ExternalHint, StorePage};
use ExtensionSupport::{Chromium as ChromiumExtension, Firefox as FirefoxExtension, NotSupported};
use ExtensionUnsupportedReason::{FlatpakFirefoxFamily, NoWebStore};
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

impl ManifestRoot {
    /// Абсолютный путь корня или `None`, если корень Flatpak, а цель — пакет
    /// (`flatpak_app_id` пуст).
    ///
    /// Тот же каталог служит и корнем профилей браузера: у Chromium это
    /// «user data dir», внутри которого лежит `NativeMessagingHosts`, у Firefox —
    /// каталог с `profiles.ini`.
    pub fn resolve(
        &self,
        environment: &InstallEnvironment,
        flatpak_app_id: Option<&str>,
    ) -> Option<PathBuf> {
        match self {
            Self::XdgConfig(name) => Some(environment.xdg_config_home.join(name)),
            Self::HomeDot(name) => Some(environment.home.join(name)),
            Self::FlatpakConfig(name) => flatpak_app_id.map(|app_id| {
                environment
                    .flatpak_app_dir(app_id)
                    .join("config")
                    .join(name)
            }),
        }
    }
}

/// Как плеер доставляет экстеншен в Chromium-браузер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromiumDelivery {
    /// Браузер читает пользовательскую папку `External Extensions/` (Chromium,
    /// Brave, Vivaldi, Edge): плеер кладёт туда файл со ссылкой на магазин, и
    /// браузер сам ставит экстеншен при следующем запуске, без вопросов.
    ExternalHint,
    /// Фирменный Google Chrome эту папку игнорирует (проверено вживую):
    /// остаётся открыть страницу магазина в этом браузере, «Add» жмёт человек.
    StorePage,
}

/// Почему экстеншен для этой цели ставить нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionUnsupportedReason {
    /// Мост для Flatpak-Firefox не поддерживается (bug 2078349), экстеншен без
    /// моста бесполезен.
    FlatpakFirefoxFamily,
    /// В браузере нет веб-магазина (ungoogled-chromium), ставить не откуда.
    NoWebStore,
}

/// Что установщик экстеншена умеет делать с целью.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionSupport {
    /// Chromium-семейство: профили лежат в `user_data`.
    Chromium {
        /// Корень профилей («user data dir»); тот же, где лежит `NativeMessagingHosts`.
        user_data: ManifestRoot,
        /// Как доставлять экстеншен.
        delivery: ChromiumDelivery,
    },
    /// Firefox-семейство: профили перечислены в `profiles.ini` этих корней.
    Firefox {
        /// Каталоги, где может лежать `profiles.ini` (у Firefox их два).
        profile_roots: &'static [ManifestRoot],
    },
    /// Ставить нельзя.
    NotSupported(ExtensionUnsupportedReason),
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
    /// Что умеет установщик экстеншена (сессия 07a).
    pub extension: ExtensionSupport,
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
        self.manifest_roots
            .iter()
            .filter_map(|root| root.resolve(environment, self.flatpak_app_id()))
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
    extension: ExtensionSupport,
) -> BrowserTarget {
    BrowserTarget {
        id,
        display_name,
        family,
        packaging: Packaging::System { executables },
        manifest_roots,
        verification,
        extension,
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
    extension: ExtensionSupport,
) -> BrowserTarget {
    BrowserTarget {
        id,
        display_name,
        family,
        packaging: Packaging::Flatpak { app_id },
        manifest_roots,
        verification,
        extension,
    }
}

/// Корни профилей Firefox: главный `~/.mozilla/firefox` и XDG-путь новых версий.
const FIREFOX_PROFILE_ROOTS: &[ManifestRoot] =
    &[HomeDot(".mozilla/firefox"), XdgConfig("mozilla/firefox")];
/// Корень профилей LibreWolf.
const LIBREWOLF_PROFILE_ROOTS: &[ManifestRoot] = &[HomeDot(".librewolf")];
/// Корень профилей Waterfox.
const WATERFOX_PROFILE_ROOTS: &[ManifestRoot] = &[HomeDot(".waterfox")];

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
        FirefoxExtension {
            profile_roots: FIREFOX_PROFILE_ROOTS,
        },
    ),
    system(
        "chromium",
        "Chromium",
        Chromium,
        &["chromium"],
        &[XdgConfig("chromium")],
        Verified,
        ChromiumExtension {
            user_data: XdgConfig("chromium"),
            delivery: ExternalHint,
        },
    ),
    // Фирменный Chrome папку `External Extensions` игнорирует (проверено вживую).
    system(
        "google-chrome",
        "Google Chrome",
        Chromium,
        &["google-chrome-stable", "google-chrome"],
        &[XdgConfig("google-chrome")],
        NotVerified,
        ChromiumExtension {
            user_data: XdgConfig("google-chrome"),
            delivery: StorePage,
        },
    ),
    system(
        "brave",
        "Brave",
        Chromium,
        &["brave", "brave-browser"],
        &[XdgConfig("BraveSoftware/Brave-Browser")],
        NotVerified,
        ChromiumExtension {
            user_data: XdgConfig("BraveSoftware/Brave-Browser"),
            delivery: ExternalHint,
        },
    ),
    system(
        "vivaldi",
        "Vivaldi",
        Chromium,
        &["vivaldi", "vivaldi-stable"],
        &[XdgConfig("vivaldi")],
        NotVerified,
        ChromiumExtension {
            user_data: XdgConfig("vivaldi"),
            delivery: ExternalHint,
        },
    ),
    system(
        "microsoft-edge",
        "Microsoft Edge",
        Chromium,
        &["microsoft-edge-stable", "microsoft-edge"],
        &[XdgConfig("microsoft-edge")],
        NotVerified,
        ChromiumExtension {
            user_data: XdgConfig("microsoft-edge"),
            delivery: ExternalHint,
        },
    ),
    system(
        "librewolf",
        "LibreWolf",
        Firefox,
        &["librewolf"],
        &[HomeDot(".librewolf")],
        NotVerified,
        FirefoxExtension {
            profile_roots: LIBREWOLF_PROFILE_ROOTS,
        },
    ),
    system(
        "waterfox",
        "Waterfox",
        Firefox,
        &["waterfox"],
        &[HomeDot(".waterfox")],
        NotVerified,
        FirefoxExtension {
            profile_roots: WATERFOX_PROFILE_ROOTS,
        },
    ),
    // Flatpak, Chromium-семейство: файл лежит в `~/.var/app/<id>/config/<имя>/`.
    flatpak(
        "google-chrome-flatpak",
        "Google Chrome (Flatpak)",
        Chromium,
        "com.google.Chrome",
        &[FlatpakConfig("google-chrome")],
        Verified,
        ChromiumExtension {
            user_data: FlatpakConfig("google-chrome"),
            delivery: StorePage,
        },
    ),
    flatpak(
        "chromium-flatpak",
        "Chromium (Flatpak)",
        Chromium,
        "org.chromium.Chromium",
        &[FlatpakConfig("chromium")],
        NotVerified,
        ChromiumExtension {
            user_data: FlatpakConfig("chromium"),
            delivery: ExternalHint,
        },
    ),
    flatpak(
        "ungoogled-chromium-flatpak",
        "Ungoogled Chromium (Flatpak)",
        Chromium,
        "io.github.ungoogled_software.ungoogled_chromium",
        &[FlatpakConfig("chromium")],
        NotVerified,
        NotSupported(NoWebStore),
    ),
    flatpak(
        "brave-flatpak",
        "Brave (Flatpak)",
        Chromium,
        "com.brave.Browser",
        &[FlatpakConfig("BraveSoftware/Brave-Browser")],
        NotVerified,
        ChromiumExtension {
            user_data: FlatpakConfig("BraveSoftware/Brave-Browser"),
            delivery: ExternalHint,
        },
    ),
    flatpak(
        "vivaldi-flatpak",
        "Vivaldi (Flatpak)",
        Chromium,
        "com.vivaldi.Vivaldi",
        &[FlatpakConfig("vivaldi")],
        NotVerified,
        ChromiumExtension {
            user_data: FlatpakConfig("vivaldi"),
            delivery: ExternalHint,
        },
    ),
    flatpak(
        "microsoft-edge-flatpak",
        "Microsoft Edge (Flatpak)",
        Chromium,
        "com.microsoft.Edge",
        &[FlatpakConfig("microsoft-edge")],
        NotVerified,
        ChromiumExtension {
            user_data: FlatpakConfig("microsoft-edge"),
            delivery: ExternalHint,
        },
    ),
    // Flatpak, Firefox-семейство: пока не поддерживается, файлов нет (manifest_roots пуст).
    flatpak(
        "firefox-flatpak",
        "Firefox (Flatpak)",
        Firefox,
        "org.mozilla.firefox",
        &[],
        NotVerified,
        NotSupported(FlatpakFirefoxFamily),
    ),
    flatpak(
        "librewolf-flatpak",
        "LibreWolf (Flatpak)",
        Firefox,
        "io.gitlab.librewolf-community",
        &[],
        NotVerified,
        NotSupported(FlatpakFirefoxFamily),
    ),
    flatpak(
        "waterfox-flatpak",
        "Waterfox (Flatpak)",
        Firefox,
        "net.waterfox.waterfox",
        &[],
        NotVerified,
        NotSupported(FlatpakFirefoxFamily),
    ),
];

/// Цель по ID.
pub fn find_target(id: &BrowserTargetId) -> Option<&'static BrowserTarget> {
    TARGETS.iter().find(|target| target.id == id.as_str())
}
