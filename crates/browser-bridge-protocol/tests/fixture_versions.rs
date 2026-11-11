//! Версия каждой валидной фикстуры сеанса по общим таблицам (с протокола 1.10, сессия 16a2).
//!
//! Независимая оценка — только JSON фикстуры и три таблицы: самая новая из версии типа
//! (`message-versions.json`), версий присутствующих необязательных полей
//! (`field-versions.json`, с 1.10) и версий значений (`value-versions.json`, с 1.3). Она обязана
//! совпасть с `required_version` разобранного сообщения. Так фикстура с полем
//! `clock.generation` считается сообщением 1.10, а тик без поля — 1.0.
//!
//! Версии в таблицах — текст `major.minor`; сравниваются ЧИСЛАМИ: `"1.10"` как строка меньше
//! `"1.9"`. То же проверяет экстеншен (`extensions/browser/tests/protocol/fixtures.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use browser_bridge_protocol::{
    PageSessionMessage, PlayerSessionMessage, ProtocolVersion, decode_message,
};
use serde::Deserialize;
use serde_json::Value;

/// Таблица по сторонам: сторона → имя (тип или «сообщение.поле») → версия текстом.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SideVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

/// Таблица значений: «сообщение.поле» → значение → версия текстом.
type ValueVersions = BTreeMap<String, BTreeMap<String, String>>;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(&std::fs::read_to_string(fixtures_root().join(name)).unwrap()).unwrap()
}

/// Версия из текста таблицы — числами.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

/// Версия, которой по таблицам требует сообщение `message` одной стороны.
fn table_required_version(
    message: &Value,
    type_versions: &BTreeMap<String, String>,
    field_versions: &BTreeMap<String, String>,
    value_versions: &ValueVersions,
) -> ProtocolVersion {
    let message_type = message["type"].as_str().unwrap();
    let mut required = version_of(&type_versions[message_type]);
    for (field, field_value) in message.as_object().unwrap() {
        let key = format!("{message_type}.{field}");
        if let Some(version) = field_versions.get(&key) {
            required = required.max(version_of(version));
        }
        if let (Some(by_value), Some(text)) = (value_versions.get(&key), field_value.as_str()) {
            required = required.max(version_of(&by_value[text]));
        }
    }
    required
}

#[test]
fn every_valid_session_fixture_requires_exactly_the_version_the_shared_tables_give() {
    let manifest: Value = read_json("manifest.json");
    let types: SideVersions = read_json("message-versions.json");
    let fields: SideVersions = read_json("field-versions.json");
    let values: ValueVersions = read_json("value-versions.json");
    let mut checked_per_side: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ticks_with_generation = 0;
    for case in manifest["cases"].as_array().unwrap() {
        // Только валидные текстовые сообщения сеанса: у рамок и сообщений входа версий типов нет.
        let schema = case["schema"].as_str().unwrap();
        if case["expect"]["result"] != "ok" || !schema.ends_with("_session") {
            continue;
        }
        let file = case["file"].as_str().unwrap();
        let bytes = std::fs::read(fixtures_root().join(file)).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        let (expected, actual) = match schema {
            "page_session" => (
                table_required_version(&json, &types.page_session, &fields.page_session, &values),
                decode_message::<PageSessionMessage>(&bytes)
                    .unwrap()
                    .required_version(),
            ),
            "player_session" => (
                table_required_version(
                    &json,
                    &types.player_session,
                    &fields.player_session,
                    &values,
                ),
                decode_message::<PlayerSessionMessage>(&bytes)
                    .unwrap()
                    .required_version(),
            ),
            _ => continue,
        };
        assert_eq!(actual, expected, "{file}");
        *checked_per_side.entry(schema).or_default() += 1;
        if json["type"] == "clock" && json.get("generation").is_some() {
            assert_eq!(actual, ProtocolVersion::V1_10, "{file}");
            ticks_with_generation += 1;
        }
    }
    // Проверка не пустая: сверены обе стороны, и тики с номером поколения среди фикстур есть.
    assert_eq!(
        checked_per_side.keys().copied().collect::<Vec<_>>(),
        ["page_session", "player_session"],
        "{checked_per_side:?}"
    );
    assert!(ticks_with_generation >= 3, "{ticks_with_generation}");
}
