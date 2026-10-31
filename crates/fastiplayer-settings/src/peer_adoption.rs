//! Что копия плеера принимает на лету из config-а, записанного соседней копией
//! («общая доска», решения владельца 135 и 137).
//!
//! Каждая копия (обычное окно и окна браузерного режима) следит за `config.toml`. Когда
//! сосед записал файл, копия сравнивает его со своим committed-документом и применяет
//! разницу тем же конвейером, что Apply. Но не всё:
//!
//! - **запоминаемое состояние** (решение 137) только записывается без потерь и в соседних
//!   окнах на лету НЕ применяется: громкость и mute, субтитры вкл/выкл, ширина сайдбара,
//!   размер и фуллскрин окна браузерного режима. Иначе «убавил звук подкаста — стих фильм
//!   в соседнем окне»;
//! - всё, что меняется в окне настроек (включая стиль субтитров), применяется во всех копиях.
//!
//! Этот модуль — единственный источник истины для такой классификации. Приложение не
//! держит своего списка строк: оно спрашивает [`peer_copy_adoption`] или сразу получает
//! готовый список изменений из [`adoptable_peer_changes`].

use fastiplayer_config::AppConfig;
use settings_core::{
    ExternalSettingChange, SettingAccess, SettingId, SettingsRegistry, SettingsResult,
};

use crate::{SettingApplyMechanism, setting_application_contract};

/// Как копия плеера обращается с настройкой, которую записала соседняя копия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerCopyAdoption {
    /// Принять на лету: значение проходит через runtime-владельцев, как при Apply.
    AdoptLive,
    /// Запоминаемое состояние (решение 137): у соседа значение живёт только в файле,
    /// живой runtime этой копии его не получает.
    RememberedStateOnly,
}

/// Запоминаемое состояние, у которого есть обычный живой маршрут.
///
/// Эти настройки при Apply в своём окне меняют текущее воспроизведение (поэтому их
/// механизм в контракте — не `RememberedStateOnly`), но пишет их сама программа по
/// действиям человека в конкретном окне, и соседнее окно не должно их перенимать:
/// - `audio.volume`, `audio.muted` — громкость и mute (UX-11, слайдер, `M`, MPRIS);
/// - `subtitles.enabled` — кнопка CC и клавиша `C`;
/// - `ui.sidebar.width_points` — перетаскивание края сайдбара.
///
/// Ключи окна браузерного режима (`browser_session.window.*`) сюда не входят: их
/// механизм в контракте уже `RememberedStateOnly`, и классификация берёт его оттуда.
const REMEMBERED_STATE_WITH_LIVE_ROUTE: &[&str] = &[
    "audio.volume",
    "audio.muted",
    "subtitles.enabled",
    "ui.sidebar.width_points",
];

/// Классифицирует настройку для «общей доски».
///
/// Настройка без строки в контракте применения (такого быть не должно — это ловит
/// тест покрытия контракта) принимается на лету: тот же путь, что и Apply.
#[must_use]
pub fn peer_copy_adoption(setting_id: &SettingId) -> PeerCopyAdoption {
    if REMEMBERED_STATE_WITH_LIVE_ROUTE.contains(&setting_id.as_str()) {
        return PeerCopyAdoption::RememberedStateOnly;
    }
    match setting_application_contract(setting_id) {
        Some(contract) if contract.mechanism == SettingApplyMechanism::RememberedStateOnly => {
            PeerCopyAdoption::RememberedStateOnly
        }
        _ => PeerCopyAdoption::AdoptLive,
    }
}

/// Изменения файла соседа, которые эта копия должна принять на лету.
///
/// Сравнивает свой committed-документ с документом на диске по реестру настроек и
/// оставляет только то, что принимается на лету:
/// - запоминаемое состояние ([`PeerCopyAdoption::RememberedStateOnly`]) отбрасывается;
/// - защищённые настройки (`SettingAccess::ReadOnly`, например версия схемы) отбрасываются:
///   их нельзя «применить», а реестр отказал бы в записи всей пачки;
/// - id из `excluded` отбрасываются (несохранённые изменения этой копии: их значение в
///   памяти новее того, что лежит на диске, и принятие откатило бы правку пользователя).
///
/// Пустой результат — своя запись, повтор события или только запоминаемое состояние.
pub fn adoptable_peer_changes(
    registry: &SettingsRegistry<AppConfig>,
    committed: &AppConfig,
    disk: &AppConfig,
    excluded: &[SettingId],
) -> SettingsResult<Vec<ExternalSettingChange>> {
    let diff = registry.diff(committed, disk)?;
    let changes = diff
        .into_changes()
        .into_iter()
        .filter(|change| peer_copy_adoption(&change.id) == PeerCopyAdoption::AdoptLive)
        .filter(|change| {
            registry
                .descriptor(&change.id)
                .is_some_and(|descriptor| descriptor.access == SettingAccess::ReadWrite)
        })
        .filter(|change| !excluded.contains(&change.id))
        .map(|change| ExternalSettingChange::new(change.id, change.after))
        .collect();
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use settings_core::SettingValue;

    use super::*;
    use crate::app_config_registry;

    /// Все пять групп запоминаемого состояния решения 137 классифицированы как
    /// запоминаемые; настройки окна настроек (стиль субтитров) — нет.
    #[test]
    fn remembered_state_groups_are_not_adopted_live_but_settings_window_values_are() {
        for remembered in [
            "audio.volume",
            "audio.muted",
            "subtitles.enabled",
            "ui.sidebar.width_points",
            "browser_session.window.inner_width_points",
            "browser_session.window.inner_height_points",
            "browser_session.window.fullscreen",
        ] {
            assert_eq!(
                peer_copy_adoption(&SettingId::from(remembered)),
                PeerCopyAdoption::RememberedStateOnly,
                "{remembered} is remembered state"
            );
        }
        for live in [
            "subtitles.text_size_percent",
            "subtitles.text_color",
            "network.read_ahead_mb",
            "ui.language",
            "browser_session.buffer_ahead_s",
            "playlist.load_siblings",
        ] {
            assert_eq!(
                peer_copy_adoption(&SettingId::from(live)),
                PeerCopyAdoption::AdoptLive,
                "{live} is adopted live"
            );
        }
    }

    /// Каждый ключ списка существует в реестре: переименование настройки без правки
    /// списка тихо вернуло бы её в «принимать на лету».
    #[test]
    fn every_listed_remembered_key_exists_in_the_registry() {
        let registry = app_config_registry().expect("registry builds");
        for setting_id in REMEMBERED_STATE_WITH_LIVE_ROUTE {
            assert!(
                registry.descriptor(&SettingId::from(*setting_id)).is_some(),
                "{setting_id} must be a registered setting"
            );
        }
    }

    /// Диф отдаёт только принимаемое на лету: громкость соседа и его окно не попадают,
    /// стиль субтитров попадает со значением с диска.
    #[test]
    fn adoptable_changes_drop_remembered_state_and_keep_live_values() {
        let registry = app_config_registry().expect("registry builds");
        let committed = AppConfig::default();
        let mut disk = committed.clone();
        disk.audio.volume = 0.25;
        disk.audio.muted = true;
        disk.subtitles.enabled = !committed.subtitles.enabled;
        disk.ui.sidebar.width_points = committed.ui.sidebar.width_points + 10;
        disk.browser_session.window.fullscreen = !committed.browser_session.window.fullscreen;
        disk.subtitles.text_size_percent = committed.subtitles.text_size_percent + 10;

        let changes =
            adoptable_peer_changes(&registry, &committed, &disk, &[]).expect("diff works");

        assert_eq!(
            changes,
            vec![ExternalSettingChange::new(
                "subtitles.text_size_percent",
                SettingValue::Integer(i64::from(disk.subtitles.text_size_percent)),
            )]
        );
    }

    /// Исключённый id (несохранённая правка этой копии) не принимается, остальное — да.
    #[test]
    fn excluded_unsaved_ids_are_not_adopted() {
        let registry = app_config_registry().expect("registry builds");
        let committed = AppConfig::default();
        let mut disk = committed.clone();
        disk.subtitles.text_size_percent = committed.subtitles.text_size_percent + 10;
        disk.network.read_ahead_mb = committed.network.read_ahead_mb + 1;

        let changes = adoptable_peer_changes(
            &registry,
            &committed,
            &disk,
            &[SettingId::from("subtitles.text_size_percent")],
        )
        .expect("diff works");

        let ids: Vec<&str> = changes
            .iter()
            .map(|change| change.setting_id.as_str())
            .collect();
        assert_eq!(ids, vec!["network.read_ahead_mb"]);
    }

    /// Одинаковые документы (своя запись) — принимать нечего.
    #[test]
    fn identical_documents_produce_nothing_to_adopt() {
        let registry = app_config_registry().expect("registry builds");
        let config = AppConfig::default();

        let changes = adoptable_peer_changes(&registry, &config, &config, &[]).expect("diff");

        assert!(changes.is_empty());
    }
}
