//! Отклонения одного прогона от обычного запуска браузера: порядок установки экстеншенов,
//! свои дополнительные экстеншены (блокировщик матрицы) вместо `--extra-extension`, имена
//! «публичных» хостов, живущих на сервере стенда (`public.bench.example`, рекламный домен).
//!
//! Раньше эти решения были зашиты в `launch_browser` и `HandoffRun::open`. Чистые данные
//! здесь отделены от запуска, поэтому порядок и состав экстеншенов проверяются тестами без
//! браузера.

use std::path::PathBuf;

use crate::browsers::{ExtensionSource, WindowPrivacy};

/// Кто из экстеншенов ставится в браузер первым (порядок влияет на то, чья обёртка
/// `Worker`/`MediaSource` окажется внешней).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum InstallOrder {
    /// Настоящий экстеншен сценария первым, дополнительные после (поведение до 09c).
    #[default]
    ScenarioFirst,
    /// Дополнительные (блокировщик) первыми, настоящий экстеншен после.
    ExtrasFirst,
}

/// Откуда брать дополнительные экстеншены.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum ExtraExtensions {
    /// Из командной строки (`--extra-extension`).
    #[default]
    FromCommandLine,
    /// Ровно эти (пустой список — «без блокировщика», даже если флаг задан).
    Explicit(Vec<PathBuf>),
}

/// Как экстеншен попадает в браузер уже после его запуска (10d, поздний вход).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum RuntimeInstall {
    /// Обычный прогон: экстеншен ставится при запуске, поздней установки нет.
    #[default]
    NotUsed,
    /// Браузер принимает установку после запуска; её делает сама строка, когда ей нужно.
    ByTheRow,
    /// Экстеншен из этого каталога ставится сразу после запуска, ДО открытия страницы (строка
    /// «обновление»: установка уже стоит, потом её обновляют).
    BeforeThePage(PathBuf),
}

impl RuntimeInstall {
    /// Нужен ли браузеру режим, в котором принимается установка после запуска.
    pub(super) fn browser_must_allow_it(&self) -> bool {
        !matches!(self, Self::NotUsed)
    }
}

/// Отклонения прогона от обычного.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct RunTweaks {
    /// Установка экстеншена после запуска браузера.
    pub runtime_install: RuntimeInstall,
    /// Порядок установки.
    pub order: InstallOrder,
    /// Откуда дополнительные экстеншены.
    pub extras: ExtraExtensions,
    /// Имена, которые браузер разрешает в `127.0.0.1` (публичный сайт и рекламный домен).
    pub loopback_host_aliases: Vec<String>,
    /// Страницу стенда открыть под этим именем вместо `127.0.0.1` (порт тот же).
    pub page_host: Option<String>,
    /// Окно браузера прогона: обычное или приватное (память выбора, 14b2).
    pub window_privacy: WindowPrivacy,
}

/// Дополнительный экстеншен: распакованный каталог или пакет.
fn source_of(path: &std::path::Path) -> ExtensionSource {
    if path.is_dir() {
        ExtensionSource::UnpackedDir(path.to_path_buf())
    } else {
        ExtensionSource::PackedFile(path.to_path_buf())
    }
}

impl RunTweaks {
    /// Итоговый список экстеншенов в порядке установки.
    pub(super) fn arrange_extensions(
        &self,
        scenario_extensions: Vec<ExtensionSource>,
        command_line_extras: &[PathBuf],
    ) -> Vec<ExtensionSource> {
        let extras: Vec<ExtensionSource> = match &self.extras {
            ExtraExtensions::FromCommandLine => command_line_extras
                .iter()
                .map(|path| source_of(path))
                .collect(),
            ExtraExtensions::Explicit(paths) => paths.iter().map(|path| source_of(path)).collect(),
        };
        match self.order {
            InstallOrder::ScenarioFirst => scenario_extensions.into_iter().chain(extras).collect(),
            InstallOrder::ExtrasFirst => extras.into_iter().chain(scenario_extensions).collect(),
        }
    }

    /// Адрес сервера стенда под нужным именем: `http://127.0.0.1:1234` → `http://<имя>:1234`.
    pub(super) fn page_origin(&self, main_origin: &str) -> String {
        match (&self.page_host, main_origin.rsplit_once(':')) {
            (Some(host), Some((_, port))) => format!("http://{host}:{port}"),
            _ => main_origin.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real() -> Vec<ExtensionSource> {
        vec![ExtensionSource::UnpackedDir("/real".into())]
    }

    #[test]
    fn scenario_extension_goes_first_by_default_and_extras_follow() {
        let arranged =
            RunTweaks::default().arrange_extensions(real(), &["/missing-extra.xpi".into()]);
        assert_eq!(
            arranged,
            vec![
                ExtensionSource::UnpackedDir("/real".into()),
                ExtensionSource::PackedFile("/missing-extra.xpi".into()),
            ]
        );
    }

    #[test]
    fn extras_first_puts_the_blocker_before_the_real_extension() {
        let tweaks = RunTweaks {
            order: InstallOrder::ExtrasFirst,
            extras: ExtraExtensions::Explicit(vec!["/blocker.xpi".into()]),
            ..RunTweaks::default()
        };
        // Флаг командной строки при явном списке игнорируется.
        let arranged = tweaks.arrange_extensions(real(), &["/from-flag.xpi".into()]);
        assert_eq!(
            arranged,
            vec![
                ExtensionSource::PackedFile("/blocker.xpi".into()),
                ExtensionSource::UnpackedDir("/real".into()),
            ]
        );
    }

    #[test]
    fn explicit_empty_list_means_no_blocker_even_with_the_flag() {
        let tweaks = RunTweaks {
            extras: ExtraExtensions::Explicit(Vec::new()),
            ..RunTweaks::default()
        };
        assert_eq!(
            tweaks.arrange_extensions(real(), &["/from-flag.xpi".into()]),
            real()
        );
    }

    #[test]
    fn only_a_runtime_install_asks_the_browser_for_the_late_install_mode() {
        assert!(!RuntimeInstall::NotUsed.browser_must_allow_it());
        assert!(RuntimeInstall::ByTheRow.browser_must_allow_it());
        assert!(RuntimeInstall::BeforeThePage("/copy".into()).browser_must_allow_it());
        assert!(!RunTweaks::default().runtime_install.browser_must_allow_it());
    }

    #[test]
    fn page_origin_swaps_only_the_host_and_keeps_the_port() {
        let tweaks = RunTweaks {
            page_host: Some("public.bench.example".into()),
            ..RunTweaks::default()
        };
        assert_eq!(
            tweaks.page_origin("http://127.0.0.1:4321"),
            "http://public.bench.example:4321"
        );
        assert_eq!(
            RunTweaks::default().page_origin("http://127.0.0.1:4321"),
            "http://127.0.0.1:4321"
        );
    }
}
