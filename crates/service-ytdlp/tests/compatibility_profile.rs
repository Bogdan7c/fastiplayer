//! Focused checks checked-in compatibility profile `yt-dlp 2026.07.04`.
//!
//! Тест намеренно работает только с data artifacts и не вызывает production
//! process, transport, demux, decoder либо player code.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Точная upstream release identity, которую обязан закреплять весь corpus.
const EXPECTED_RELEASE: &str = "2026.07.04";
/// Точный commit официального release tag.
const EXPECTED_COMMIT: &str = "fdec00e0bf530dc6c3cc7b1dd780e95d9ae460e9";
/// Точное source tree официального release tag.
const EXPECTED_TREE: &str = "b14ea6bf92e81a98bdcf652f5e46977c1ee593cc";
/// SHA-256 конкретного official GitHub source archive, наблюдённого при S00.
const EXPECTED_SOURCE_ARCHIVE_SHA256: &str =
    "7fb7ca0509dd8f21263246d3d749a346e049fa9d3cfdef072e05c7bbd88d6fc0";
/// Относительный путь owner-а compatibility artifacts.
const PROFILE_DIRECTORY: &str = "compatibility/2026.07.04";

/// Возвращает абсолютный путь compatibility owner-а внутри текущего crate.
fn profile_directory() -> PathBuf {
    // Cargo предоставляет стабильный абсолютный путь crate без зависимости от cwd.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PROFILE_DIRECTORY)
}

/// Читает JSON fixture, не включая его потенциально secret-bearing содержимое в ошибку.
fn read_json(path: &Path) -> Value {
    // Ошибка чтения сообщает только checked-in путь, а не содержимое fixture.
    let json_bytes = fs::read(path).unwrap_or_else(|error| {
        panic!("не удалось прочитать compatibility artifact {path:?}: {error}")
    });
    // Ошибка parse сообщает только путь: raw JSON не дублируется в test output.
    serde_json::from_slice(&json_bytes).unwrap_or_else(|error| {
        panic!("не удалось разобрать compatibility artifact {path:?}: {error}")
    })
}

/// Возвращает обязательное строковое поле manifest object.
fn required_string<'value>(object: &'value Value, field: &str) -> &'value str {
    // Называем отсутствующее поле, но никогда не печатаем соседние значения.
    object
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("обязательное строковое поле `{field}` отсутствует"))
}

/// Возвращает обязательный массив manifest object.
fn required_array<'value>(object: &'value Value, field: &str) -> &'value [Value] {
    // Тип поля является частью machine-readable schema.
    object
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("обязательный массив `{field}` отсутствует"))
}

/// Проверяет уникальность строкового identity field во всём массиве объектов.
fn assert_unique_string_field(objects: &[Value], field: &str) {
    // Set хранит только уже встреченные non-secret schema identities.
    let mut seen_identities = HashSet::new();
    // Каждый объект обязан иметь проверяемое identity field.
    for object in objects {
        // Identity читается через общий strict helper.
        let identity = required_string(object, field);
        // Повтор означает alias/schema conflict.
        assert!(
            seen_identities.insert(identity),
            "duplicate compatibility identity в поле `{field}`"
        );
    }
}

/// Выделяет имя official `_format_fields` из более точного variant path.
fn upstream_format_field_name(classified_path: &str) -> &str {
    // Variant suffix начинается с array, condition либо nested-field separator.
    classified_path
        .split(['[', '(', '.'])
        .next()
        .unwrap_or(classified_path)
}

/// Рекурсивно собирает все checked-in `fixture_id`.
fn collect_fixture_ids(value: &Value, fixture_ids: &mut HashSet<String>) {
    // Обрабатываем JSON object и его дочерние значения.
    if let Some(object) = value.as_object() {
        // Fixture identity может находиться у корня либо у отдельной target row.
        if let Some(fixture_id) = object.get("fixture_id").and_then(Value::as_str) {
            // Duplicate fixture ID сделал бы provenance неоднозначным.
            assert!(
                fixture_ids.insert(fixture_id.to_string()),
                "duplicate fixture_id в checked-in corpus"
            );
        }
        // Рекурсивно посещаем каждое поле object.
        for child_value in object.values() {
            // Дочерние fixtures могут быть вложены в `formats` или `fixtures`.
            collect_fixture_ids(child_value, fixture_ids);
        }
    // Обрабатываем JSON array и каждый его элемент.
    } else if let Some(array) = value.as_array() {
        // Порядок corpus не влияет на проверку identity.
        for child_value in array {
            // Рекурсивно посещаем вложенный object/array.
            collect_fixture_ids(child_value, fixture_ids);
        }
    }
}

/// Рекурсивно проверяет, что fixture URLs синтетические, а secret material redacted.
fn assert_fixture_is_secret_safe(value: &Value) {
    // Object требует проверки field-aware secret markers.
    if let Some(object) = value.as_object() {
        // Проверяем каждую пару key/value без вывода raw value.
        for (field, child_value) in object {
            // Сравнение secret fields выполняется без учёта регистра.
            let lowercase_field = field.to_ascii_lowercase();
            // Cookie payload в corpus обязан быть только marker-ом.
            if lowercase_field == "cookies" {
                // Реальное cookie value не должно попасть даже в synthetic corpus.
                assert!(
                    child_value
                        .as_str()
                        .is_some_and(|text| text.starts_with("<redacted-")),
                    "fixture содержит неотредактированное cookie field"
                );
            }
            // Authorization header обязан быть только marker-ом.
            if lowercase_field == "authorization" {
                // Проверяем значение, не печатая его при failure.
                assert!(
                    child_value
                        .as_str()
                        .is_some_and(|text| text.starts_with("<redacted-")),
                    "fixture содержит неотредактированный Authorization header"
                );
            }
            // Cryptographic secret/key/IV fields обязаны быть только markers.
            if matches!(lowercase_field.as_str(), "secret" | "key" | "iv") {
                // Эти поля никогда не нужны compatibility test в usable виде.
                assert!(
                    child_value
                        .as_str()
                        .is_some_and(|text| text.starts_with("<redacted-")),
                    "fixture содержит неотредактированный cryptographic secret"
                );
            }
            // Рекурсивно проверяем дочернее значение с именем его поля.
            assert_fixture_is_secret_safe(child_value);
        }
    // Array не меняет secret semantics родительского поля.
    } else if let Some(array) = value.as_array() {
        // Проверяем каждый элемент массива.
        for child_value in array {
            // Secret semantics определяются самим вложенным object field.
            assert_fixture_is_secret_safe(child_value);
        }
    // String может оказаться locator-ом.
    } else if let Some(text) = value.as_str() {
        // Ищем только scheme separator, чтобы не принять source path за URL.
        if let Some((_, locator_tail)) = text.split_once("://") {
            // Authority заканчивается перед первым path separator.
            let authority = locator_tail.split('/').next().unwrap_or_default();
            // Userinfo в synthetic fixture не нужен и может скрыть leak.
            assert!(
                !authority.contains('@'),
                "fixture locator содержит userinfo"
            );
            // Port не влияет на проверку reserved synthetic host.
            let host = authority.split(':').next().unwrap_or_default();
            // Каждый checked-in fixture locator обязан использовать reserved namespace.
            assert!(
                host == "invalid" || host.ends_with(".invalid"),
                "fixture содержит locator вне reserved .invalid namespace"
            );
            // Query/fragment могли бы случайно сохранить usable token.
            assert!(
                !text.contains('?') && !text.contains('#'),
                "fixture locator содержит query или fragment"
            );
        }
    }
}

/// Рекурсивно проверяет bounds только у raw identity fields.
fn assert_raw_identity_bounds(value: &Value, raw_fields: &HashSet<&str>, max_bytes: usize) {
    // Object связывает значение с именем identity field.
    if let Some(object) = value.as_object() {
        // Проверяем каждое поле object.
        for (field, child_value) in object {
            // Raw identity должен сохраниться целиком, но оставаться bounded.
            if raw_fields.contains(field.as_str()) {
                // В profile fixtures identity всегда строковый.
                let identity = child_value
                    .as_str()
                    .unwrap_or_else(|| panic!("raw identity `{field}` обязан быть строкой"));
                // len() для UTF-8 строки возвращает точное количество bytes.
                assert!(
                    identity.len() <= max_bytes,
                    "raw identity `{field}` превышает profile bound"
                );
            }
            // Вложенные format/request objects проверяются тем же правилом.
            assert_raw_identity_bounds(child_value, raw_fields, max_bytes);
        }
    // Array содержит произвольное количество format/result objects.
    } else if let Some(array) = value.as_array() {
        // Проверяем каждый array element.
        for child_value in array {
            // Bounds не зависят от позиции элемента.
            assert_raw_identity_bounds(child_value, raw_fields, max_bytes);
        }
    }
}

/// Загружает manifest и весь перечисленный им official synthetic corpus.
fn load_profile_and_corpus() -> (Value, Vec<Value>) {
    // Owner path вычисляется один раз.
    let owner_directory = profile_directory();
    // Manifest является machine-readable source of truth.
    let profile = read_json(&owner_directory.join("profile.json"));
    // Corpus paths берутся из manifest, а не дублируются в тесте.
    let corpus_paths = required_array(
        profile
            .get("corpus")
            .unwrap_or_else(|| panic!("manifest corpus section отсутствует")),
        "official_synthetic_fixtures",
    );
    // Выделяем ровно необходимую ёмкость для parsed fixtures.
    let mut corpus = Vec::with_capacity(corpus_paths.len());
    // Каждый declared corpus artifact обязан существовать и быть JSON.
    for corpus_path in corpus_paths {
        // Путь обязан быть относительной строкой.
        let relative_path = corpus_path
            .as_str()
            .unwrap_or_else(|| panic!("corpus path обязан быть строкой"));
        // Absolute path или parent traversal нарушили бы ownership boundary.
        assert!(
            !Path::new(relative_path).is_absolute() && !relative_path.contains(".."),
            "corpus path покидает compatibility owner"
        );
        // Загружаем checked-in artifact.
        corpus.push(read_json(&owner_directory.join(relative_path)));
    }
    // Возвращаем manifest и fixtures одним coherent snapshot.
    (profile, corpus)
}

/// Проверяет exact source fingerprint, schema uniqueness и target traceability.
#[test]
fn manifest_has_exact_source_and_no_schema_conflicts() {
    // Загружаем единый checked-in snapshot.
    let (profile, corpus) = load_profile_and_corpus();
    // Source section обязательна.
    let source = profile
        .get("source")
        .unwrap_or_else(|| panic!("manifest source section отсутствует"));
    // Release обязан совпасть точно, без semver range.
    assert_eq!(required_string(source, "release"), EXPECTED_RELEASE);
    // Tag обязан совпасть с release.
    assert_eq!(required_string(source, "tag"), EXPECTED_RELEASE);
    // Commit исключает неоднозначность mutable branch.
    assert_eq!(required_string(source, "commit"), EXPECTED_COMMIT);
    // Tree дополнительно fingerprint-ит содержимое source.
    assert_eq!(required_string(source, "tree"), EXPECTED_TREE);
    // Archive fingerprint обнаруживает подмену конкретного downloaded source snapshot.
    assert_eq!(
        required_string(source, "source_archive_sha256_observed_2026_07_20"),
        EXPECTED_SOURCE_ARCHIVE_SHA256
    );

    // Result identities не могут конфликтовать.
    assert_unique_string_field(required_array(&profile, "result_types"), "identity");
    // Top-level result/DTO paths не могут иметь две противоречивые rows.
    assert_unique_string_field(required_array(&profile, "result_fields"), "path");
    // Canonical upstream field list обязан быть exact set без duplicates.
    let upstream_format_fields = required_array(&profile, "upstream_format_fields")
        .iter()
        .map(|field| {
            field
                .as_str()
                .unwrap_or_else(|| panic!("upstream format field обязан быть строкой"))
        })
        .collect::<HashSet<_>>();
    // Set size обязан совпасть с array size, иначе official field повторён.
    assert_eq!(
        upstream_format_fields.len(),
        required_array(&profile, "upstream_format_fields").len(),
        "duplicate official `_format_fields` identity"
    );
    // Format field paths не могут иметь две противоречивые classification rows.
    let classified_format_fields = required_array(&profile, "format_fields");
    // Exact variant paths не могут конфликтовать.
    assert_unique_string_field(classified_format_fields, "path");
    // Собираем canonical owner field для каждой variant classification.
    let classified_upstream_fields = classified_format_fields
        .iter()
        .filter_map(|field| {
            // Private provider fields с `_` не входят в public `_format_fields`.
            let classified_path = required_string(field, "path");
            // Остальные variant paths обязаны покрывать official public field.
            (!classified_path.starts_with('_')).then(|| upstream_format_field_name(classified_path))
        })
        .collect::<HashSet<_>>();
    // Ни одно official field нельзя пропустить из classification inventory.
    assert_eq!(
        classified_upstream_fields, upstream_format_fields,
        "classification inventory расходится с pinned official `_format_fields`"
    );
    // Request-material decisions не могут дублировать один path.
    assert_unique_string_field(required_array(&profile, "request_material_fields"), "path");
    // Target IDs обязаны быть уникальны.
    let target_rows = required_array(&profile, "target_rows");
    assert_unique_string_field(target_rows, "id");
    // Codec profile IDs обязаны быть уникальны.
    let codec_profiles = required_array(&profile, "codec_profiles");
    assert_unique_string_field(codec_profiles, "id");
    // Собираем exact codec profile references.
    let codec_profile_ids = codec_profiles
        .iter()
        .map(|profile| required_string(profile, "id"))
        .collect::<HashSet<_>>();
    // S00 не может молча расширить decoder scope.
    for codec_profile in codec_profiles {
        // Каждый profile обязан явно отрицать expansion.
        assert_eq!(
            codec_profile.get("expands_decoder_scope"),
            Some(&Value::Bool(false))
        );
        // Пустой codec family set не описывает target row.
        assert!(
            !required_array(codec_profile, "families").is_empty(),
            "codec profile не содержит families"
        );
    }
    // Excluded IDs обязаны быть уникальны.
    let excluded_rows = required_array(&profile, "excluded_rows");
    assert_unique_string_field(excluded_rows, "id");
    // Target и excluded namespace не должны пересекаться.
    let target_ids = target_rows
        .iter()
        .map(|row| required_string(row, "id"))
        .collect::<HashSet<_>>();
    // Проверяем каждый explicit exclusion.
    for excluded_row in excluded_rows {
        // Один ID не может одновременно обещаться и исключаться.
        assert!(
            !target_ids.contains(required_string(excluded_row, "id")),
            "row одновременно Target и ProfileExcluded"
        );
    }

    // Protocol alias не может принадлежать двум families.
    let mut protocol_aliases = HashSet::new();
    // Обходим каждую alias family.
    for alias_family in required_array(&profile, "protocol_aliases") {
        // Family identity обязана быть non-empty.
        assert!(!required_string(alias_family, "family").is_empty());
        // Обходим exact aliases.
        for alias in required_array(alias_family, "aliases") {
            // Alias обязан быть строкой.
            let alias = alias
                .as_str()
                .unwrap_or_else(|| panic!("protocol alias обязан быть строкой"));
            // Duplicate alias сделал бы normalization неоднозначным.
            assert!(
                protocol_aliases.insert(alias),
                "protocol alias принадлежит нескольким families"
            );
        }
    }

    // Собираем fixture identities только из corpus, не из manifest references.
    let mut fixture_ids = HashSet::new();
    // Каждый corpus artifact может содержать несколько fixtures.
    for fixture_document in &corpus {
        // Exact release provenance обязателен у каждого official fixture file.
        assert_eq!(
            required_string(fixture_document, "source_release"),
            EXPECTED_RELEASE
        );
        // Exact commit provenance обязателен у каждого official fixture file.
        assert_eq!(
            required_string(fixture_document, "source_commit"),
            EXPECTED_COMMIT
        );
        // Source contract anchor не может быть пустым.
        assert!(!required_string(fixture_document, "source_contract").is_empty());
        // Рекурсивно собираем все fixture IDs.
        collect_fixture_ids(fixture_document, &mut fixture_ids);
    }
    // Corpus обязан содержать evidence, а не только report.
    assert!(!fixture_ids.is_empty(), "official synthetic corpus пуст");

    // Каждая Target row обязана ссылаться на future implementation и fixture.
    for target_row in target_rows {
        // S00 не выдаёт Target row за уже работающий runtime.
        assert_eq!(required_string(target_row, "status"), "Target");
        // Future sessions закрепляют ownership дальнейшей реализации.
        assert!(
            !required_array(target_row, "future_sessions").is_empty(),
            "Target row не связана с future session"
        );
        // Каждая codec profile reference обязана разрешаться.
        for codec_profile_ref in required_array(target_row, "codec_profile_refs") {
            // Reference обязана быть строкой.
            let codec_profile_ref = codec_profile_ref
                .as_str()
                .unwrap_or_else(|| panic!("codec profile reference обязана быть строкой"));
            // Unknown profile сделал бы target semantics неявной.
            assert!(
                codec_profile_ids.contains(codec_profile_ref),
                "Target row ссылается на отсутствующий codec profile"
            );
        }
        // Fixture reference обязана разрешаться в corpus.
        assert!(
            fixture_ids.contains(required_string(target_row, "fixture_id")),
            "Target row ссылается на отсутствующий fixture"
        );
    }
}

/// Проверяет safety-critical argv обоих режимов.
#[test]
fn invocation_profiles_keep_hermetic_and_manual_guarantees_separate() {
    // Загружаем manifest; corpus для argv-проверки не нужен.
    let (profile, _) = load_profile_and_corpus();
    // Invocation section обязательна.
    let invocations = profile
        .get("invocations")
        .unwrap_or_else(|| panic!("manifest invocations section отсутствует"));
    // Inventory hermetic profile проверяется exact equality.
    let hermetic_inventory = invocations
        .get("hermetic_inventory")
        .unwrap_or_else(|| panic!("hermetic inventory invocation отсутствует"));
    // Exact ordered args являются safety contract.
    assert_eq!(
        required_array(hermetic_inventory, "argv_before_url"),
        [
            "--ignore-config",
            "--no-plugin-dirs",
            "--quiet",
            "--no-warnings",
            "--simulate",
            "--dump-single-json",
            "--no-playlist"
        ]
    );
    // Hermetic mode не читает system config.
    assert_eq!(
        hermetic_inventory.get("loads_system_config"),
        Some(&Value::Bool(false))
    );
    // Hermetic mode не импортирует plugins.
    assert_eq!(
        hermetic_inventory.get("loads_plugins"),
        Some(&Value::Bool(false))
    );
    // Hermetic mode не открывает user cookie file.
    assert_eq!(
        hermetic_inventory.get("loads_cookie_file"),
        Some(&Value::Bool(false))
    );

    // Selected profile обязан сохранять те же safety args.
    let hermetic_selected = invocations
        .get("hermetic_selected")
        .unwrap_or_else(|| panic!("hermetic selected invocation отсутствует"));
    // Selector добавляется только после hermetic prefix.
    assert_eq!(
        required_array(hermetic_selected, "argv_before_selector"),
        [
            "--ignore-config",
            "--no-plugin-dirs",
            "--quiet",
            "--no-warnings",
            "--simulate",
            "--dump-single-json",
            "--no-playlist",
            "--format"
        ]
    );

    // Проверяем отсутствие app-owned side-effect flags в обоих hermetic argv.
    let forbidden_arguments = [
        "--no-simulate",
        "--skip-download",
        "--no-download",
        "--print-to-file",
        "--exec",
        "--exec-before-download",
        "--use-postprocessor",
        "--mark-watched",
        "--cookies",
        "--cookies-from-browser",
    ];
    // Обходим оба safety-critical argv.
    for arguments in [
        required_array(hermetic_inventory, "argv_before_url"),
        required_array(hermetic_selected, "argv_before_selector"),
    ] {
        // Проверяем каждый app-owned argument.
        for argument in arguments {
            // Argument обязан быть строкой.
            let argument = argument
                .as_str()
                .unwrap_or_else(|| panic!("CLI argument обязан быть строкой"));
            // Write family проверяется prefix-ом.
            assert!(
                !argument.starts_with("--write-"),
                "hermetic argv содержит write behavior"
            );
            // Остальные опасные flags проверяются exact.
            assert!(
                !forbidden_arguments.contains(&argument),
                "hermetic argv содержит side-effect argument"
            );
        }
    }

    // Manual mode обязан быть отдельной explicit trust boundary.
    let manual_opt_in = invocations
        .get("manual_opt_in_inventory")
        .unwrap_or_else(|| panic!("manual opt-in invocation отсутствует"));
    // Manual mode честно сообщает чтение system config.
    assert_eq!(
        manual_opt_in.get("loads_system_config"),
        Some(&Value::Bool(true))
    );
    // Manual mode честно сообщает загрузку plugins.
    assert_eq!(manual_opt_in.get("loads_plugins"), Some(&Value::Bool(true)));
    // Все три documented side-effect caveats обязательны.
    let caveats = required_array(manual_opt_in, "outside_app_guarantee")
        .iter()
        .filter_map(Value::as_str)
        .collect::<HashSet<_>>();
    // Trusted config может добавить side effects.
    assert!(caveats.contains("trusted_user_config_may_add_side_effects"));
    // Trusted plugin code находится вне app guarantee.
    assert!(caveats.contains("trusted_plugin_import_or_execution_may_add_side_effects"));
    // User-owned cookie jar может быть обновлён system yt-dlp.
    assert!(caveats.contains("user_owned_cookie_jar_may_be_updated_by_system_yt_dlp"));

    // Current selected production suffix также фиксируется как manual opt-in.
    let manual_selected = invocations
        .get("manual_opt_in_selected")
        .unwrap_or_else(|| panic!("manual selected invocation отсутствует"));
    // Exact current selected argv не должен скрыто расходиться с process owner.
    assert_eq!(
        required_array(manual_selected, "argv_before_selector"),
        [
            "--quiet",
            "--no-warnings",
            "--simulate",
            "--dump-single-json",
            "--no-playlist",
            "--format"
        ]
    );
    // Selected manual mode имеет ту же explicit trust boundary.
    assert_eq!(
        manual_selected.get("loads_system_config"),
        Some(&Value::Bool(true))
    );
    // Selected manual mode также допускает trusted plugins.
    assert_eq!(
        manual_selected.get("loads_plugins"),
        Some(&Value::Bool(true))
    );
}

/// Проверяет separation formats/requested_formats, raw bounds и secret-safe fixtures.
#[test]
fn corpus_preserves_unknown_identity_without_leaking_request_secrets() {
    // Загружаем coherent manifest/corpus snapshot.
    let (profile, corpus) = load_profile_and_corpus();
    // Bounds section обязательна.
    let bounds = profile
        .get("bounds")
        .unwrap_or_else(|| panic!("manifest bounds section отсутствует"));
    // Bound читается как беззнаковое число bytes.
    let max_bytes = bounds
        .get("raw_identity_max_utf8_bytes")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("raw identity bound обязан помещаться в usize"));
    // Нулевой bound не сохранял бы future identity.
    assert!(
        max_bytes > 0,
        "raw identity bound обязан быть положительным"
    );
    // Собираем exact bounded field names.
    let raw_fields = required_array(bounds, "raw_identity_fields")
        .iter()
        .map(|field| {
            field
                .as_str()
                .unwrap_or_else(|| panic!("raw identity field обязан быть строкой"))
        })
        .collect::<HashSet<_>>();

    // Проверяем каждый corpus artifact.
    for fixture_document in &corpus {
        // Corpus не должен содержать usable locator/header/cookie/key.
        assert_fixture_is_secret_safe(fixture_document);
        // Все raw identities обязаны укладываться в versioned bound.
        assert_raw_identity_bounds(fixture_document, &raw_fields, max_bytes);
    }

    // Находим format inventory fixture по его уникальному root fixture ID.
    let format_inventory = corpus
        .iter()
        .find(|document| {
            document
                .get("fixture_id")
                .and_then(Value::as_str)
                .is_some_and(|fixture_id| fixture_id == "format-inventory-separation")
        })
        .unwrap_or_else(|| panic!("format inventory fixture отсутствует"));
    // Payload содержит extractor inventory и selection result рядом для проверки.
    let inventory_payload = format_inventory
        .get("payload")
        .unwrap_or_else(|| panic!("format inventory payload отсутствует"));
    // `formats` является полным inventory.
    let formats = required_array(inventory_payload, "formats");
    // `requested_formats` является только выбранными merge components.
    let requested_formats = required_array(inventory_payload, "requested_formats");
    // Selection result не может быть тем же списком, что inventory.
    assert!(
        requested_formats.len() < formats.len(),
        "requested_formats ошибочно подменяет formats inventory"
    );
    // Собираем snapshot-local format IDs inventory.
    let inventory_ids = formats
        .iter()
        .map(|format| required_string(format, "format_id"))
        .collect::<HashSet<_>>();
    // Каждый requested component обязан происходить из того же extraction snapshot.
    for requested_format in requested_formats {
        // Unknown requested ID без semantic inventory match является stale/invalid.
        assert!(
            inventory_ids.contains(required_string(requested_format, "format_id")),
            "requested format отсутствует в formats inventory"
        );
    }

    // Находим future unknown identity fixture.
    let unknown_fixture = corpus
        .iter()
        .flat_map(|document| {
            document
                .get("fixtures")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find(|fixture| {
            fixture
                .get("fixture_id")
                .and_then(Value::as_str)
                .is_some_and(|fixture_id| fixture_id == "future-unknown-identity")
        })
        .unwrap_or_else(|| panic!("future unknown identity fixture отсутствует"));
    // Unknown path обязан сохранять typed incompatibility classification.
    assert_eq!(
        required_string(unknown_fixture, "expected_classification"),
        "IncompatibleYtDlpContract"
    );
    // Raw protocol identity сохраняется verbatim, а не схлопывается в generic unknown.
    assert_eq!(
        required_string(
            unknown_fixture
                .get("payload")
                .unwrap_or_else(|| panic!("unknown identity payload отсутствует")),
            "protocol"
        ),
        "future_serializable_transport_v2"
    );
}
