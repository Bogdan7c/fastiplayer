//! Настройка режима 2 «автозапуск» в `chrome.storage.local` экстеншена.
//!
//! Ключ и форма записи зеркалят `extensions/browser/src/shared/auto-handoff-settings.ts`
//! (`AUTO_HANDOFF_SETTINGS_KEY`, `AutoHandoffSettings`). Стенд пишет настройку так же, как это сделала
//! бы страница настроек: из служебной страницы экстеншена (`run_in_service_page`), где есть `chrome` /
//! `browser`. Экстеншен подхватывает запись без перезапуска (`storage.onChanged`), поэтому после записи
//! строка даёт ему короткий срок [`super::steps::SETTINGS_SETTLE`] дойти до страниц.
//!
//! Страница сайта до этого хранилища не дотягивается, а значит, включить режим 2 за экстеншен не может.

use serde_json::{Value, json};

use crate::browsers::BrowserUnderTest;

/// Ключ настройки в `storage.local` (зеркало `AUTO_HANDOFF_SETTINGS_KEY`).
pub(super) const SETTINGS_KEY: &str = "autoHandoffSettings";
/// Порог длины ролика в стенде, секунды. Продукт по умолчанию берёт 60 с, но фильм стенда ровно
/// 60,0 с: на самой границе «≥ порога» строка зависела бы от округления контейнера. Порог 45 с
/// оставляет запас по обе стороны: фильм (60 с) длиннее, короткий ролик (30 с) короче.
pub(super) const BENCH_MIN_DURATION_S: u32 = 45;

/// Правило сайта (зеркало `SiteRule`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SiteRule {
    /// На этом сайте видео сразу уходит в плеер (его же пишет «Always for this site» предложения).
    Always,
    /// На этом сайте режим 2 не срабатывает никогда.
    Never,
    /// На этом сайте человека спрашивают (предложение «Ask», 12b).
    Ask,
}

impl SiteRule {
    fn as_stored(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::Ask => "ask",
        }
    }
}

/// Фильтры видео в настройке (решения 260–261): звук, длина, цикл.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BenchVideoFilters {
    /// Все три фильтра включены на всех сайтах — поведение 12a, на нём построены строки «осталось у браузера».
    AllOnEverySite,
    /// Все выключены, списков нет — значение по умолчанию экстеншена (решение 261).
    AllOff,
}

impl BenchVideoFilters {
    fn to_stored_value(self) -> Value {
        let enabled = matches!(self, Self::AllOnEverySite);
        let filter = json!({ "enabled": enabled, "sites": [] });
        json!({ "sound": filter, "duration": filter, "loop": filter })
    }
}

/// Что стенд записывает в настройку режима 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AutoHandoffSetup {
    enabled: bool,
    min_duration_s: u32,
    video_filters: BenchVideoFilters,
    /// Правило для сайта без своей записи.
    unknown_site_rule: SiteRule,
    /// Правила сайтов: ключ — хост без `www.`.
    site_rules: Vec<(String, SiteRule)>,
    /// Свой стоп-лист: записи вида `host` или `host/путь`.
    own_stop_list: Vec<String>,
}

impl AutoHandoffSetup {
    /// Режим включён для всех сайтов, у которых нет своего правила; порог длины стенда.
    pub(super) fn enabled_for_every_site() -> Self {
        Self {
            enabled: true,
            min_duration_s: BENCH_MIN_DURATION_S,
            video_filters: BenchVideoFilters::AllOnEverySite,
            // Сайт без своего правила: сразу передавать (решение 251: по умолчанию «always»).
            unknown_site_rule: SiteRule::Always,
            site_rules: Vec::new(),
            own_stop_list: Vec::new(),
        }
    }

    /// Режим выключен (как у нового пользователя); правила сайтов при этом живут (правило «ask» работает
    /// и при выключенном режиме).
    pub(super) fn switched_off(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// То же, но фильтры видео как по умолчанию у экстеншена: все выключены (решение 261).
    pub(super) fn with_default_video_filters(mut self) -> Self {
        self.video_filters = BenchVideoFilters::AllOff;
        self
    }

    /// То же плюс правило для сайта без своей записи.
    pub(super) fn with_unknown_site_rule(mut self, rule: SiteRule) -> Self {
        self.unknown_site_rule = rule;
        self
    }

    /// То же плюс правило для сайта `site_key` (хост без `www.`); прежнее правило этого сайта заменяется.
    pub(super) fn with_site_rule(mut self, site_key: &str, rule: SiteRule) -> Self {
        self.site_rules.retain(|(site, _)| site != site_key);
        self.site_rules.push((site_key.to_owned(), rule));
        self
    }

    /// То же плюс запись в своём стоп-листе.
    pub(super) fn with_own_stop_entry(mut self, entry: &str) -> Self {
        self.own_stop_list.push(entry.to_owned());
        self
    }

    /// Значение, которое ляжет под [`SETTINGS_KEY`].
    pub(super) fn to_stored_value(&self) -> Value {
        let site_rules: serde_json::Map<String, Value> = self
            .site_rules
            .iter()
            .map(|(site, rule)| (site.clone(), json!(rule.as_stored())))
            .collect();
        json!({
            "enabled": self.enabled,
            "min_duration_s": self.min_duration_s,
            "video_filters": self.video_filters.to_stored_value(),
            "unknown_site_rule": self.unknown_site_rule.as_stored(),
            "site_rules": site_rules,
            "own_stop_list": self.own_stop_list,
        })
    }

    /// Скрипт служебной страницы: записать настройку и прочитать её обратно (ответ — JSON записи).
    fn write_script(&self) -> String {
        format!(
            "async (api) => {{ await api.storage.local.set({{ {SETTINGS_KEY}: {value} }}); \
             const stored = await api.storage.local.get({key:?}); \
             return JSON.stringify(stored[{key:?}] ?? null); }}",
            value = self.to_stored_value(),
            key = SETTINGS_KEY,
        )
    }
}

/// Какую настройку режима 2 строка кладёт в экстеншен перед страницей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsPreset {
    /// Ничего не писать: режим выключен, как у нового пользователя (решение 9).
    UntouchedDefault,
    /// Режим включён для всех сайтов, фильтры видео включены везде (поведение 12a).
    On,
    /// Режим включён, фильтры видео как по умолчанию — выключены (решение 261).
    OnDefaultFilters,
    /// Режим включён, но у сайта стенда правило «никогда».
    SiteRuleNever,
    /// Режим включён, но сайт стенда в своём стоп-листе.
    OwnStopList,
    /// Режим ВЫКЛЮЧЕН, но у сайта стенда правило «ask»: предложение должно появиться и так (12b).
    SiteRuleAsk,
    /// Режим включён, сайт новый, а правило для новых сайтов «ask» (12b).
    UnknownSiteAsk,
}

impl SettingsPreset {
    /// Что записать для сайта стенда `site_key`; `None` — хранилище не трогать.
    pub(super) fn setup(self, site_key: &str) -> Option<AutoHandoffSetup> {
        let on = AutoHandoffSetup::enabled_for_every_site();
        match self {
            Self::UntouchedDefault => None,
            Self::On => Some(on),
            Self::OnDefaultFilters => Some(on.with_default_video_filters()),
            Self::SiteRuleNever => Some(on.with_site_rule(site_key, SiteRule::Never)),
            Self::OwnStopList => Some(on.with_own_stop_entry(site_key)),
            Self::SiteRuleAsk => Some(on.switched_off().with_site_rule(site_key, SiteRule::Ask)),
            Self::UnknownSiteAsk => Some(on.with_unknown_site_rule(SiteRule::Ask)),
        }
    }
}

/// Ключ сайта для правил из адреса сервера стенда (`http://127.0.0.1:7001` → `127.0.0.1`):
/// хост строчными и без `www.`, как делает `siteKeyOfHost` экстеншена.
pub(super) fn site_key_of_origin(origin: &str) -> Result<String, String> {
    let without_scheme = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let host = without_scheme
        .split(['/', ':'])
        .next()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| format!("no host in the stand origin '{origin}'"))?;
    let host = host.to_ascii_lowercase();
    Ok(host.strip_prefix("www.").unwrap_or(&host).to_owned())
}

/// Разбор ответа [`AutoHandoffSetup::write_script`] и сверка с тем, что записывали.
fn check_stored_reply(setup: &AutoHandoffSetup, reply: &str) -> Result<(), String> {
    let stored: Value = serde_json::from_str(reply)
        .map_err(|error| format!("the settings reply is not JSON ({error}): {reply}"))?;
    let wanted = setup.to_stored_value();
    if stored == wanted {
        Ok(())
    } else {
        Err(format!(
            "the extension storage holds {stored}, expected {wanted}"
        ))
    }
}

/// Пишет настройку режима 2 в хранилище экстеншена и убеждается, что она там лежит.
/// Нужна открытая служебная страница экстеншена (`ReturnHand::watch_page_layer`).
pub(super) fn write_settings(
    browser: &mut dyn BrowserUnderTest,
    setup: &AutoHandoffSetup,
) -> Result<(), String> {
    let reply = browser
        .run_in_service_page(&setup.write_script())
        .map_err(|error| format!("writing the mode 2 settings: {error}"))?;
    check_stored_reply(setup, &reply)
}

/// Скрипт служебной страницы: прочитать настройку режима 2 из хранилища (ответ — JSON записи или `null`).
fn read_script() -> String {
    format!(
        "async (api) => {{ const stored = await api.storage.local.get({key:?}); \
         return JSON.stringify(stored[{key:?}] ?? null); }}",
        key = SETTINGS_KEY,
    )
}

/// Читает настройку режима 2 из хранилища экстеншена так, как она лежит сейчас (ничего не пишет).
/// Нужна открытая служебная страница экстеншена.
pub(super) fn read_stored_settings(browser: &mut dyn BrowserUnderTest) -> Result<Value, String> {
    let reply = browser
        .run_in_service_page(&read_script())
        .map_err(|error| format!("reading the mode 2 settings: {error}"))?;
    serde_json::from_str(&reply)
        .map_err(|error| format!("the settings reply is not JSON ({error}): {reply}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_bench_setup_turns_the_mode_on_with_the_bench_threshold() {
        let value = AutoHandoffSetup::enabled_for_every_site().to_stored_value();
        assert_eq!(value["enabled"], json!(true));
        assert_eq!(value["min_duration_s"], json!(BENCH_MIN_DURATION_S));
        assert_eq!(value["unknown_site_rule"], json!("always"));
        assert_eq!(value["site_rules"], json!({}));
        assert_eq!(value["own_stop_list"], json!([]));
    }

    #[test]
    fn a_never_rule_and_a_stop_entry_land_in_their_own_fields() {
        let value = AutoHandoffSetup::enabled_for_every_site()
            .with_site_rule("127.0.0.1", SiteRule::Never)
            .with_own_stop_entry("127.0.0.1")
            .to_stored_value();
        assert_eq!(value["site_rules"], json!({ "127.0.0.1": "never" }));
        assert_eq!(value["own_stop_list"], json!(["127.0.0.1"]));
        // Правило сайта не выключает режим целиком: он включён, просто не для этого сайта.
        assert_eq!(value["enabled"], json!(true));
    }

    #[test]
    fn the_write_script_stores_under_the_extension_key_and_reads_it_back() {
        let script = AutoHandoffSetup::enabled_for_every_site().write_script();
        assert!(script.contains("storage.local.set({ autoHandoffSettings: {"));
        assert!(script.contains("storage.local.get(\"autoHandoffSettings\")"));
        assert!(script.contains("\"enabled\":true"));
    }

    #[test]
    fn the_bench_filters_are_written_on_everywhere_and_the_default_preset_turns_them_off() {
        let on = AutoHandoffSetup::enabled_for_every_site().to_stored_value();
        for name in ["sound", "duration", "loop"] {
            assert_eq!(
                on["video_filters"][name],
                json!({ "enabled": true, "sites": [] }),
                "{name}"
            );
        }
        let default_filters = SettingsPreset::OnDefaultFilters
            .setup("127.0.0.1")
            .expect("written")
            .to_stored_value();
        assert_eq!(default_filters["enabled"], json!(true));
        for name in ["sound", "duration", "loop"] {
            assert_eq!(
                default_filters["video_filters"][name],
                json!({ "enabled": false, "sites": [] }),
                "{name}"
            );
        }
    }

    #[test]
    fn every_preset_writes_what_its_name_says() {
        assert_eq!(SettingsPreset::UntouchedDefault.setup("127.0.0.1"), None);
        let on = SettingsPreset::On.setup("127.0.0.1").expect("written");
        assert_eq!(on.to_stored_value()["site_rules"], json!({}));
        let never = SettingsPreset::SiteRuleNever
            .setup("127.0.0.1")
            .expect("written");
        assert_eq!(
            never.to_stored_value()["site_rules"],
            json!({ "127.0.0.1": "never" })
        );
        assert_eq!(never.to_stored_value()["own_stop_list"], json!([]));
        let stop = SettingsPreset::OwnStopList
            .setup("127.0.0.1")
            .expect("written");
        assert_eq!(
            stop.to_stored_value()["own_stop_list"],
            json!(["127.0.0.1"])
        );
        assert_eq!(stop.to_stored_value()["site_rules"], json!({}));
    }

    #[test]
    fn the_site_key_is_the_bare_host_of_the_stand_origin() {
        assert_eq!(
            site_key_of_origin("http://127.0.0.1:7001"),
            Ok("127.0.0.1".to_owned())
        );
        assert_eq!(
            site_key_of_origin("http://WWW.Bench.example:8"),
            Ok("bench.example".to_owned())
        );
        assert!(site_key_of_origin("http://:80").is_err());
    }

    #[test]
    fn the_stored_reply_must_match_what_was_written() {
        let setup = AutoHandoffSetup::enabled_for_every_site();
        let same = setup.to_stored_value().to_string();
        assert_eq!(check_stored_reply(&setup, &same), Ok(()));
        let disabled = json!({"enabled": false}).to_string();
        let problem = check_stored_reply(&setup, &disabled).expect_err("different value");
        assert!(problem.contains("expected"), "{problem}");
        assert!(check_stored_reply(&setup, "null").is_err());
        assert!(check_stored_reply(&setup, "not json").is_err());
    }

    #[test]
    fn an_ask_rule_and_an_ask_default_land_in_their_own_fields() {
        let rule = SettingsPreset::SiteRuleAsk
            .setup("127.0.0.1")
            .expect("written")
            .to_stored_value();
        // Правило «ask» работает и при выключенном режиме: его и проверяет строка.
        assert_eq!(rule["enabled"], json!(false));
        assert_eq!(rule["site_rules"], json!({ "127.0.0.1": "ask" }));
        assert_eq!(rule["unknown_site_rule"], json!("always"));
        let unknown = SettingsPreset::UnknownSiteAsk
            .setup("127.0.0.1")
            .expect("written")
            .to_stored_value();
        assert_eq!(unknown["enabled"], json!(true));
        assert_eq!(unknown["unknown_site_rule"], json!("ask"));
        assert_eq!(unknown["site_rules"], json!({}));
    }

    #[test]
    fn a_new_rule_for_a_site_replaces_the_old_one() {
        let value = AutoHandoffSetup::enabled_for_every_site()
            .with_site_rule("127.0.0.1", SiteRule::Ask)
            .with_site_rule("127.0.0.1", SiteRule::Always)
            .to_stored_value();
        assert_eq!(value["site_rules"], json!({ "127.0.0.1": "always" }));
    }

    #[test]
    fn the_read_script_only_reads_the_extension_key() {
        let script = read_script();
        assert!(script.contains("storage.local.get(\"autoHandoffSettings\")"));
        assert!(!script.contains(".set("));
    }
}
