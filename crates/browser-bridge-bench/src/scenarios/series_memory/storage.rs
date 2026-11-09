//! Память выбора экстеншена (14b2) глазами стенда: чем и как её читать, подсаживать и стирать.
//!
//! Экстеншен хранит выбор зрителя (озвучка, качество, субтитры) по сайту и сериалу в
//! `chrome.storage.local`, ключ [`STORAGE_KEY`]. Договор формата (его пишет экстеншен, стенд только
//! читает и, для проверки «читает в приватном окне», подсаживает одну запись сам):
//!
//! ```text
//! { "version": 1, "series": { "<64 hex>": { "audio"?: string, "quality"?: string,
//!                                           "subtitles"?: …, "used_at": <мс эпохи> } } }
//! ```
//!
//! Ключ записи — отпечаток [`choice_fingerprint`]: SHA-256 от `fastiplayer-choice-memory:v1`, имени
//! сайта и ключа сериала. Стенд считает его САМ и независимо от экстеншена: совпадение отпечатка —
//! доказательство, что экстеншен запомнил выбор именно для этой пары «сайт + сериал».
//!
//! Модуль не ходит в браузер: он даёт тексты скриптов для `run_in_extension_page` и разбирает их
//! ответы. Так формат проверяется тестами без браузера.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Ключ `storage.local`, под которым экстеншен хранит память выбора.
pub(super) const STORAGE_KEY: &str = "rememberedChoices";
/// Версия формата, которую понимает стенд.
const SUPPORTED_VERSION: u32 = 1;
/// Приставка отпечатка: меняется вместе с версией формата.
const FINGERPRINT_PREFIX: &str = "fastiplayer-choice-memory:v1";
/// Ключ сериала, который сообщает адаптер `bench-series` (атрибут `data-series-key` страницы).
pub(super) const BENCH_SERIES_KEY: &str = "bench-series";

/// Имя сайта для отпечатка из адреса сервера стенда (`http://127.0.0.1:4321` → `127.0.0.1`):
/// имя хоста маленькими буквами, без `www.` и без порта — так считает экстеншен.
pub(super) fn site_key_of_origin(origin: &str) -> String {
    let without_scheme = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    let host = authority.split(':').next().unwrap_or(authority);
    let lowered = host.to_ascii_lowercase();
    lowered
        .strip_prefix("www.")
        .map_or(lowered.clone(), str::to_owned)
}

/// Отпечаток пары «сайт + сериал»: 64 строчные шестнадцатеричные цифры.
pub(super) fn choice_fingerprint(site_key: &str, series_key: &str) -> String {
    let material = format!("{FINGERPRINT_PREFIX}\n{site_key}\n{series_key}");
    let digest = Sha256::digest(material.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Запись одного сериала в памяти выбора.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct StoredSeriesChoice {
    /// Подпись озвучки так, как её показывает страница (`Dub B`).
    pub audio: Option<String>,
    /// Подпись качества.
    pub quality: Option<String>,
    /// Выбор субтитров как есть (стенд его не разбирает: сценарии 14b2 субтитры не трогают).
    pub subtitles: Option<Value>,
    /// Когда запись обновлялась, мс эпохи.
    pub used_at: u64,
}

/// Всё содержимое памяти выбора.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct StoredChoices {
    version: u32,
    /// Записи по отпечаткам.
    #[serde(default)]
    pub series: BTreeMap<String, StoredSeriesChoice>,
}

/// Что лежит в `storage.local` под [`STORAGE_KEY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StoredMemory {
    /// Ключа нет совсем (экстеншен ещё ничего не писал или его стёрли).
    Absent,
    /// Ключ есть и разобран.
    Present(StoredChoices),
}

impl StoredMemory {
    /// Запись сериала по отпечатку.
    pub(super) fn entry(&self, fingerprint: &str) -> Option<&StoredSeriesChoice> {
        match self {
            Self::Absent => None,
            Self::Present(choices) => choices.series.get(fingerprint),
        }
    }

    /// Сколько сериалов помнит память: «забыть» обязано дать ноль.
    pub(super) fn series_count(&self) -> usize {
        match self {
            Self::Absent => 0,
            Self::Present(choices) => choices.series.len(),
        }
    }
}

/// Скрипт `async (api) => string`: отдаёт память выбора как JSON (`null`, если ключа нет).
pub(super) fn read_script() -> String {
    format!(
        "async (api) => {{ const stored = await api.storage.local.get('{STORAGE_KEY}'); \
         return JSON.stringify(stored['{STORAGE_KEY}'] ?? null); }}"
    )
}

/// Скрипт «забыть»: то же сообщение, что шлёт кнопка настроек экстеншена. Отдаёт ответ фона как JSON.
pub(super) fn forget_script() -> String {
    "async (api) => JSON.stringify(await api.runtime.sendMessage({ kind: \"forget_remembered_choices\" }))"
        .to_owned()
}

/// Скрипт «подсадить запись»: кладёт в память ровно одну запись сериала (как будто зритель выбирал
/// раньше). Версия и поля — по договору формата.
pub(super) fn seed_script(fingerprint: &str, audio_label: &str, used_at: u64) -> String {
    let memory = json!({
        "version": SUPPORTED_VERSION,
        "series": { fingerprint: { "audio": audio_label, "used_at": used_at } },
    });
    format!(
        "async (api) => {{ await api.storage.local.set({{ {STORAGE_KEY}: {memory} }}); return 'seeded'; }}"
    )
}

/// Скрипт «показать вкладки»: каждый адрес и признак приватного окна, как их видит экстеншен.
/// Нужен, чтобы приватный прогон не оказался обычным: без `incognito: true` проверка «в приватном
/// окне не пишем» была бы пустой.
pub(super) fn tabs_script() -> String {
    "async (api) => JSON.stringify((await api.tabs.query({})).map((tab) => ({ url: String(tab.url), incognito: tab.incognito })))"
        .to_owned()
}

/// Вкладка глазами экстеншена.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct SeenTab {
    pub url: String,
    pub incognito: bool,
}

/// Разбирает ответ [`tabs_script`].
pub(super) fn parse_tabs(raw: &str) -> Result<Vec<SeenTab>, String> {
    serde_json::from_str(raw).map_err(|error| format!("tabs answer {raw:?}: {error}"))
}

/// Разбирает ответ [`read_script`]. Версия и форма проверяются строго: незнакомое — ошибка, а не
/// «пусто» (иначе смена формата превратила бы проверки в вакуум).
pub(super) fn parse_memory(raw: &str) -> Result<StoredMemory, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("storage answer {raw:?}: {error}"))?;
    if value.is_null() {
        return Ok(StoredMemory::Absent);
    }
    let choices: StoredChoices = serde_json::from_value(value)
        .map_err(|error| format!("storage value {raw:?} breaks the format: {error}"))?;
    if choices.version != SUPPORTED_VERSION {
        return Err(format!(
            "storage version {} is not {SUPPORTED_VERSION}: {raw}",
            choices.version
        ));
    }
    Ok(StoredMemory::Present(choices))
}

/// Ответ фона на «забыть».
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ForgetReply {
    /// Память стёрта.
    Forgotten,
    /// Фон не смог.
    Failed { reason: String },
}

/// Разбирает ответ [`forget_script`].
pub(super) fn parse_forget_reply(raw: &str) -> Result<ForgetReply, String> {
    serde_json::from_str(raw).map_err(|error| format!("forget answer {raw:?}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_follows_the_documented_recipe() {
        // Значение посчитано независимо: `printf 'fastiplayer-choice-memory:v1\n127.0.0.1\nbench-series' | sha256sum`.
        assert_eq!(
            choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY),
            "b014190269f1029e5ffaf89acab16aaf15e4c077d9ab1c674226f6b7f17a42c6"
        );
        // Другой сайт или другой сериал — другая запись.
        assert_ne!(
            choice_fingerprint("localhost", BENCH_SERIES_KEY),
            choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY)
        );
        assert_ne!(
            choice_fingerprint("127.0.0.1", "other"),
            choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY)
        );
    }

    #[test]
    fn the_site_key_is_a_bare_lowercase_host() {
        assert_eq!(site_key_of_origin("http://127.0.0.1:4321"), "127.0.0.1");
        assert_eq!(
            site_key_of_origin("http://WWW.Example.COM:80/x"),
            "example.com"
        );
        assert_eq!(site_key_of_origin("http://localhost:9"), "localhost");
    }

    #[test]
    fn a_stored_memory_is_read_with_its_entry_and_counted() {
        let fingerprint = choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY);
        let raw = format!(
            r#"{{"version":1,"series":{{"{fingerprint}":{{"audio":"Dub B","used_at":1234}}}}}}"#
        );
        let memory = parse_memory(&raw).expect("valid memory");
        assert_eq!(memory.series_count(), 1);
        let entry = memory.entry(&fingerprint).expect("entry by fingerprint");
        assert_eq!(entry.audio.as_deref(), Some("Dub B"));
        assert_eq!(entry.quality, None);
        assert_eq!(entry.used_at, 1234);
        assert!(memory.entry("0".repeat(64).as_str()).is_none());
    }

    #[test]
    fn a_missing_key_and_an_empty_series_both_mean_nothing_is_remembered() {
        assert_eq!(parse_memory("null").expect("absent"), StoredMemory::Absent);
        assert_eq!(StoredMemory::Absent.series_count(), 0);
        let empty = parse_memory(r#"{"version":1,"series":{}}"#).expect("empty");
        assert_eq!(empty.series_count(), 0);
    }

    #[test]
    fn an_unknown_version_or_shape_is_an_error_not_silence() {
        assert!(parse_memory(r#"{"version":2,"series":{}}"#).is_err());
        assert!(parse_memory(r#"{"series":{}}"#).is_err());
        assert!(parse_memory(r#"{"version":1,"series":{"a":{"audio":"x"}}}"#).is_err());
        assert!(parse_memory("not json").is_err());
    }

    #[test]
    fn the_forget_reply_distinguishes_success_from_failure() {
        assert_eq!(
            parse_forget_reply(r#"{"kind":"forgotten"}"#),
            Ok(ForgetReply::Forgotten)
        );
        assert_eq!(
            parse_forget_reply(r#"{"kind":"failed","reason":"disk"}"#),
            Ok(ForgetReply::Failed {
                reason: "disk".to_owned()
            })
        );
        assert!(parse_forget_reply("null").is_err());
    }

    #[test]
    fn the_seed_script_writes_exactly_the_documented_record() {
        let fingerprint = choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY);
        let script = seed_script(&fingerprint, "Dub B", 4242);
        assert!(script.contains(STORAGE_KEY));
        assert!(script.contains(&fingerprint));
        assert!(script.contains(r#""audio":"Dub B""#));
        assert!(script.contains(r#""used_at":4242"#));
        assert!(script.contains(r#""version":1"#));
    }

    #[test]
    fn tabs_are_parsed_with_their_private_flag() {
        let tabs =
            parse_tabs(r#"[{"url":"http://a/","incognito":true},{"url":"b","incognito":false}]"#)
                .expect("tabs");
        assert!(tabs[0].incognito && !tabs[1].incognito);
        assert!(parse_tabs("{}").is_err());
    }
}
