//! Что робот читает у экстеншена через его служебную страницу (вкладки, слой на странице,
//! настройка входов). Функции JS выполняются в странице экстеншена `run_in_service_page`: она
//! висит фоновой вкладкой, поэтому активная вкладка при чтении не меняется.

use serde_json::Value;

use crate::browsers::{BrowserError, BrowserUnderTest};

/// Имя глобала изолированного мира с зондом слоя страницы (`src/shared/entry-contract.ts`).
const PAGE_UI_PROBE_FUNCTION_NAME: &str = "fastiplayerBridgePageUiProbe";
/// Ключ настройки входов в `storage.local` (`src/shared/entry-settings.ts`).
const ENTRY_SETTINGS_KEY: &str = "entrySettings";
/// Часть адреса самой служебной страницы: её из списка вкладок убираем.
const SERVICE_PAGE_MARK: &str = "options.html";

/// Вкладка браузера, как её видит экстеншен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TabInfo {
    pub(super) url: String,
    pub(super) active: bool,
}

/// Прямоугольник кнопки поверх видео в координатах окна страницы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ButtonRect {
    pub(super) left: f64,
    pub(super) top: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

/// Что слой экстеншена показывает на странице сейчас (суммарно по фреймам).
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct PageUiState {
    /// Имя случая показанной плашки.
    pub(super) banner: Option<String>,
    /// Кнопка поверх видео.
    pub(super) overlay_button: Option<ButtonRect>,
}

/// Настройки входов, которые робот ставит экстеншену.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EntrySettingsSetup {
    pub(super) video_menu: bool,
    pub(super) link_menu: bool,
    pub(super) overlay_button: bool,
}

impl EntrySettingsSetup {
    /// Значения по умолчанию продукта (решение 53).
    pub(super) const DEFAULT: Self = Self {
        video_menu: true,
        link_menu: true,
        overlay_button: false,
    };
}

/// JS: список вкладок всех окон без служебной страницы.
fn tabs_function() -> String {
    format!(
        "async (api) => {{ const tabs = await api.tabs.query({{}}); \
         return JSON.stringify(tabs.filter((tab) => !(tab.url || '').includes('{SERVICE_PAGE_MARK}')) \
         .map((tab) => ({{ url: tab.url || '', active: Boolean(tab.active) }}))); }}"
    )
}

/// JS: опрос зонда слоя страницы во всех фреймах вкладки с адресом, начинающимся с `url_prefix`.
fn page_ui_function(url_prefix: &str) -> String {
    let prefix = Value::String(url_prefix.to_owned());
    format!(
        "async (api) => {{ const tabs = await api.tabs.query({{}}); \
         const tab = tabs.find((candidate) => (candidate.url || '').startsWith({prefix})); \
         if (!tab) return JSON.stringify({{ error: 'no tab for the prefix' }}); \
         const results = await api.scripting.executeScript({{ target: {{ tabId: tab.id, allFrames: true }}, \
         func: () => (typeof globalThis.{PAGE_UI_PROBE_FUNCTION_NAME} === 'function' ? globalThis.{PAGE_UI_PROBE_FUNCTION_NAME}() : null) }}); \
         return JSON.stringify({{ frames: results.map((entry) => entry.result) }}); }}"
    )
}

/// JS: текст значка расширения на вкладке с адресом `url_prefix` (`!` у отказа без попапа).
fn badge_function(url_prefix: &str) -> String {
    let prefix = Value::String(url_prefix.to_owned());
    format!(
        "async (api) => {{ const tabs = await api.tabs.query({{}}); \
         const tab = tabs.find((candidate) => (candidate.url || '').startsWith({prefix})); \
         if (!tab) return JSON.stringify({{ error: 'no tab for the prefix' }}); \
         return JSON.stringify({{ text: await api.action.getBadgeText({{ tabId: tab.id }}) }}); }}"
    )
}

/// JS: записать настройку входов.
fn write_settings_function(settings: EntrySettingsSetup) -> String {
    format!(
        "async (api) => {{ await api.storage.local.set({{ {ENTRY_SETTINGS_KEY}: {{ video_menu: {}, link_menu: {}, overlay_button: {} }} }}); return 'saved'; }}",
        settings.video_menu, settings.link_menu, settings.overlay_button
    )
}

fn browser_problem(what: &str, error: BrowserError) -> String {
    format!("{what}: {error}")
}

/// Вкладки браузера (порядок экстеншена).
pub(super) fn read_tabs(browser: &mut dyn BrowserUnderTest) -> Result<Vec<TabInfo>, String> {
    let text = browser
        .run_in_service_page(&tabs_function())
        .map_err(|error| browser_problem("reading the tabs", error))?;
    parse_tabs(&text)
}

/// Разбор ответа [`tabs_function`].
pub(super) fn parse_tabs(text: &str) -> Result<Vec<TabInfo>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("tabs reply is not JSON: {error}: {text}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| format!("tabs reply is not a list: {text}"))?;
    items
        .iter()
        .map(|item| {
            Ok(TabInfo {
                url: item
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("tab without url: {item}"))?
                    .to_owned(),
                active: item
                    .get("active")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| format!("tab without active: {item}"))?,
            })
        })
        .collect()
}

/// Слой экстеншена на вкладке с адресом `url_prefix`.
pub(super) fn read_page_ui(
    browser: &mut dyn BrowserUnderTest,
    url_prefix: &str,
) -> Result<PageUiState, String> {
    let text = browser
        .run_in_service_page(&page_ui_function(url_prefix))
        .map_err(|error| browser_problem("reading the page layer", error))?;
    parse_page_ui(&text)
}

/// Разбор ответа [`page_ui_function`]: кадры без зонда (`null`) пропускаются.
pub(super) fn parse_page_ui(text: &str) -> Result<PageUiState, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("probe reply is not JSON: {error}: {text}"))?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(format!("page layer probe: {error}"));
    }
    let frames = value
        .get("frames")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("probe reply has no frames: {text}"))?;
    let mut state = PageUiState::default();
    for probe in frames.iter().filter(|probe| !probe.is_null()) {
        if let Some(banner) = probe.get("banner").and_then(Value::as_str) {
            state.banner = Some(banner.to_owned());
        }
        if let Some(rect) = probe.get("overlay_button").filter(|rect| !rect.is_null()) {
            let number = |name: &str| {
                rect.get(name)
                    .and_then(Value::as_f64)
                    .ok_or_else(|| format!("button rect without {name}: {rect}"))
            };
            state.overlay_button = Some(ButtonRect {
                left: number("left")?,
                top: number("top")?,
                width: number("width")?,
                height: number("height")?,
            });
        }
    }
    Ok(state)
}

/// Текст значка расширения на вкладке с адресом `url_prefix`.
pub(super) fn read_badge_text(
    browser: &mut dyn BrowserUnderTest,
    url_prefix: &str,
) -> Result<String, String> {
    let text = browser
        .run_in_service_page(&badge_function(url_prefix))
        .map_err(|error| browser_problem("reading the badge", error))?;
    parse_badge(&text)
}

/// Разбор ответа [`badge_function`].
pub(super) fn parse_badge(text: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("badge reply is not JSON: {error}: {text}"))?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(format!("badge probe: {error}"));
    }
    value
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("badge reply has no text: {text}"))
}

/// Ставит настройку входов; экстеншен подхватывает её без перезапуска браузера (`storage.onChanged`).
pub(super) fn write_entry_settings(
    browser: &mut dyn BrowserUnderTest,
    settings: EntrySettingsSetup,
) -> Result<(), String> {
    let reply = browser
        .run_in_service_page(&write_settings_function(settings))
        .map_err(|error| browser_problem("writing the entry settings", error))?;
    if reply == "saved" {
        Ok(())
    } else {
        Err(format!("the settings were not saved: {reply}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_reply_gives_urls_and_which_one_is_active() {
        let tabs =
            parse_tabs(r#"[{"url":"http://a/","active":false},{"url":"http://b/","active":true}]"#)
                .expect("parses");
        assert_eq!(tabs.len(), 2);
        assert!(!tabs[0].active);
        assert!(tabs[1].active);
        assert_eq!(tabs[1].url, "http://b/");
        assert!(parse_tabs(r#"[{"url":"x"}]"#).is_err());
        assert!(parse_tabs("nope").is_err());
    }

    #[test]
    fn page_ui_reply_merges_frames_and_skips_frames_without_the_probe() {
        let state = parse_page_ui(
            r#"{"frames":[null,{"banner":"opened","overlay_button":null},{"banner":null,"overlay_button":{"left":1,"top":2,"width":30,"height":20}}]}"#,
        )
        .expect("parses");
        assert_eq!(state.banner.as_deref(), Some("opened"));
        assert_eq!(
            state.overlay_button,
            Some(ButtonRect {
                left: 1.0,
                top: 2.0,
                width: 30.0,
                height: 20.0
            })
        );
        let empty = parse_page_ui(r#"{"frames":[null]}"#).expect("parses");
        assert_eq!(empty, PageUiState::default());
    }

    #[test]
    fn a_probe_error_and_a_broken_reply_are_errors_not_empty_state() {
        assert!(parse_page_ui(r#"{"error":"no tab for the prefix"}"#).is_err());
        assert!(parse_page_ui("{}").is_err());
        assert!(parse_page_ui(r#"{"frames":[{"overlay_button":{"left":1}}]}"#).is_err());
    }

    #[test]
    fn a_badge_reply_gives_the_text_and_an_error_reply_is_an_error() {
        assert_eq!(parse_badge(r#"{"text":"!"}"#), Ok("!".to_owned()));
        assert_eq!(parse_badge(r#"{"text":""}"#), Ok(String::new()));
        assert!(parse_badge(r#"{"error":"no tab for the prefix"}"#).is_err());
        assert!(parse_badge("{}").is_err());
    }

    #[test]
    fn the_settings_script_carries_every_flag_and_the_storage_key() {
        let script = write_settings_function(EntrySettingsSetup {
            video_menu: false,
            link_menu: true,
            overlay_button: true,
        });
        assert!(script.contains("entrySettings"));
        assert!(script.contains("video_menu: false, link_menu: true, overlay_button: true"));
    }

    #[test]
    fn the_page_ui_script_quotes_the_prefix_as_a_json_string() {
        let script = page_ui_function("http://127.0.0.1:1/p.html?a=\"b\"");
        assert!(script.contains("\\\"b\\\""), "{script}");
        assert!(script.contains("fastiplayerBridgePageUiProbe"));
    }
}
