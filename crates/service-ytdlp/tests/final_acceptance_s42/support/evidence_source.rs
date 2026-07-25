//! Строгая проверка checked-in source, Cargo package/target и executable test symbol.

// Filesystem читает только checked-in source и Cargo manifests.
use std::fs;
// Component и Path запрещают unsafe traversal и разбирают Cargo target path.
use std::path::{Component, Path};

// JSON evidence остаётся типизированным владельцем parent module.
use serde_json::Value;

// Parent владеет общими schema/path helpers, этот модуль — source validation.
use super::{required_string, workspace_root};

/// Читает checked-in source через безопасный workspace-relative path.
fn read_evidence_source(evidence_id: &str, evidence_path: &str) -> String {
    // Manifest path не может зависеть от local checkout root.
    let relative_path = Path::new(evidence_path);
    // Absolute path работал бы только на одной машине.
    assert!(
        relative_path.is_relative(),
        "evidence `{evidence_id}` содержит absolute path"
    );
    // Parent/root components запрещают выход из workspace.
    assert!(
        relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "evidence `{evidence_id}` содержит небезопасный path `{evidence_path}`"
    );
    // Exact workspace path строится только из checked-in data.
    let absolute_path = workspace_root().join(relative_path);
    // Missing file означает stale либо выдуманное evidence.
    fs::read_to_string(&absolute_path).unwrap_or_else(|error| {
        panic!(
            "evidence `{evidence_id}` не удалось прочитать {}: {error}",
            absolute_path.display()
        )
    })
}

/// Проверяет common path/symbol invariants и возвращает source.
pub(super) fn source_with_exact_symbol(evidence_id: &str, evidence: &Value) -> String {
    // Путь хранится workspace-relative.
    let evidence_path = required_string(evidence, "path");
    // Symbol является стабильным именем boundary/test/decision anchor.
    let evidence_symbol = required_string(evidence, "symbol");
    // Пустой symbol не может доказывать traceability.
    assert!(
        !evidence_symbol.is_empty(),
        "evidence `{evidence_id}` содержит пустой symbol"
    );
    // Source читается только после path validation.
    let source = read_evidence_source(evidence_id, evidence_path);
    // Exact symbol обязан оставаться в declared file.
    assert!(
        source.contains(evidence_symbol),
        "evidence `{evidence_id}`: symbol `{evidence_symbol}` отсутствует в `{evidence_path}`"
    );
    // Source нужен kind-specific validator-у.
    source
}

/// Проверяет exact production declaration, не принимая comment/string совпадение.
pub(super) fn assert_real_production_symbol(evidence_id: &str, evidence: &Value, source: &str) {
    // Manifest хранит intent-shaped declaration prefix вместе с visibility/kind.
    let expected_symbol = required_string(evidence, "symbol");
    // Exact line-prefix исключает Rust comments, doc comments и quoted literals.
    let matching_declaration_count = source
        .lines()
        .filter(|source_line| {
            // Только Rust tokens в начале trimmed line могут быть declaration anchor.
            let trimmed_line = source_line.trim_start();
            // Manifest может намеренно не закреплять internal visibility owner-а.
            let declaration_line = if expected_symbol.starts_with("pub ") {
                trimmed_line
            } else {
                strip_optional_rust_visibility(trimmed_line)
            };
            // Похожий longer identifier не должен совпасть с manifest symbol.
            let Some(declaration_tail) = declaration_line.strip_prefix(expected_symbol) else {
                return false;
            };
            // Следующий token delimiter допускает signature/body/generic/where continuation.
            declaration_tail.is_empty()
                || declaration_tail.starts_with(char::is_whitespace)
                || declaration_tail.starts_with('(')
                || declaration_tail.starts_with('{')
                || declaration_tail.starts_with('<')
        })
        .count();
    // Missing и duplicate anchors одинаково делают production evidence неоднозначным.
    assert_eq!(
        matching_declaration_count, 1,
        "evidence `{evidence_id}` должен иметь ровно одну production declaration `{expected_symbol}`"
    );
}

/// Удаляет только настоящий Rust visibility token в начале declaration line.
fn strip_optional_rust_visibility(source_line: &str) -> &str {
    // Unrestricted public visibility имеет отдельную lexical форму.
    if let Some(declaration) = source_line.strip_prefix("pub ") {
        return declaration;
    }
    // Restricted `pub(crate|super|self|in path)` заканчивается первой `)`.
    let Some(restricted_visibility) = source_line.strip_prefix("pub(") else {
        return source_line;
    };
    // Missing delimiter означает не declaration, а похожий malformed/source text.
    let Some((_, declaration)) = restricted_visibility.split_once(") ") else {
        return source_line;
    };
    declaration
}

/// Возвращает crate directory из `crates/<dir>/...` evidence path.
fn evidence_crate_directory<'path>(evidence_id: &str, evidence_path: &'path str) -> &'path str {
    // Slash-separated manifest path стабилен на всех supported hosts.
    let mut components = evidence_path.split('/');
    // Executable Rust test обязан принадлежать workspace crate.
    assert_eq!(
        components.next(),
        Some("crates"),
        "test evidence `{evidence_id}` находится вне `crates/`"
    );
    // Второй component является crate directory.
    components
        .next()
        // Missing crate component означает malformed path.
        .unwrap_or_else(|| panic!("test evidence `{evidence_id}` не содержит crate directory"))
}

/// Проверяет top-level либо exact path-registered nested integration test source.
fn assert_integration_target_ownership(
    evidence_id: &str,
    target_relative_path: &str,
    target: &str,
    root_target_source: Option<&str>,
) {
    // Slash является canonical repository separator для checked-in evidence.
    let path_components = target_relative_path.split('/').collect::<Vec<_>>();
    // Version 1 разрешает top-level target либо один directly registered module.
    match path_components.as_slice() {
        // Обычный `tests/<target>.rs` остаётся прежним strict contract.
        [target_file] => {
            // File stem является Cargo integration target name.
            let expected_target = Path::new(target_file)
                .file_stem()
                // `.rs` file обязательно имеет stem.
                .and_then(|stem| stem.to_str())
                // Non-UTF target нельзя сериализовать в manifest.
                .unwrap_or_else(|| {
                    panic!("test evidence `{evidence_id}` имеет invalid target path")
                });
            // Exact target запрещает stale renamed integration test.
            assert_eq!(
                target, expected_target,
                "test evidence `{evidence_id}` содержит неверный integration target"
            );
        }
        // Modularized target допускает только `tests/<target>/<module>.rs`.
        [target_directory, module_file] => {
            // Directory обязан совпасть с Cargo target byte-for-byte.
            assert_eq!(
                target, *target_directory,
                "test evidence `{evidence_id}` nested directory не совпадает с target"
            );
            // Nested evidence остаётся Rust source, а не arbitrary fixture.
            assert_eq!(
                Path::new(module_file)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("rs"),
                "test evidence `{evidence_id}` nested module не является Rust source"
            );
            // Module stem используется в exact root declaration.
            let module_name = Path::new(module_file)
                .file_stem()
                // Extension уже validated, stem обязан существовать.
                .and_then(|stem| stem.to_str())
                // Invalid stem делает registration недоказуемой.
                .unwrap_or_else(|| {
                    panic!("test evidence `{evidence_id}` имеет invalid nested module")
                });
            // Caller обязан прочитать реальный root `tests/<target>.rs`.
            let root_source = root_target_source.unwrap_or_else(|| {
                panic!("test evidence `{evidence_id}` не имеет integration root source")
            });
            // Exact path attribute не позволяет принять convention-only hidden helper.
            let expected_path_attribute = format!("#[path = \"{target_relative_path}\"]");
            // Exact module declaration связывает path с compiled target.
            let expected_module_declaration = format!("mod {module_name};");
            // Непустые строки сохраняют adjacency при harmless blank lines.
            let nonempty_lines = root_source
                .lines()
                // Whitespace не является частью Rust declaration.
                .map(str::trim)
                // Blank line не разрывает attribute/declaration pair.
                .filter(|line| !line.is_empty())
                // Pair-wise scan требует owned collection только для tiny root module.
                .collect::<Vec<_>>();
            // Exact adjacent pair доказывает reachability nested test source.
            assert!(
                nonempty_lines.windows(2).any(|lines| {
                    lines[0] == expected_path_attribute && lines[1] == expected_module_declaration
                }),
                "test evidence `{evidence_id}` nested module не зарегистрирован exact path в root target"
            );
        }
        // Более глубокий helper не является самостоятельным executable evidence.
        _ => {
            panic!("test evidence `{evidence_id}` указывает unsupported nested integration helper")
        }
    }
}

/// Проверяет declared Cargo package и test target.
pub(super) fn assert_test_package_and_target(evidence_id: &str, evidence: &Value) {
    // Exact package name обязателен для executable evidence.
    let package = required_string(evidence, "package");
    // Exact Cargo target отличает lib unit test от integration test.
    let target = required_string(evidence, "target");
    // Evidence path уже validated common helper-ом.
    let evidence_path = required_string(evidence, "path");
    // Directory связывает path с Cargo manifest.
    let crate_directory = evidence_crate_directory(evidence_id, evidence_path);
    // Package manifest является checked-in owner metadata.
    let cargo_manifest_path = workspace_root()
        .join("crates")
        .join(crate_directory)
        .join("Cargo.toml");
    // Missing Cargo manifest запрещает fake package identity.
    let cargo_manifest = fs::read_to_string(&cargo_manifest_path).unwrap_or_else(|error| {
        panic!(
            "test evidence `{evidence_id}` не удалось прочитать {}: {error}",
            cargo_manifest_path.display()
        )
    });
    // Exact package declaration ищется без permissive substring alias.
    let package_declaration = format!("name = \"{package}\"");
    // Declared package обязан совпасть с owner Cargo.toml.
    assert!(
        cargo_manifest
            .lines()
            .any(|line| line.trim() == package_declaration),
        "test evidence `{evidence_id}` package `{package}` не совпадает с Cargo.toml"
    );

    // Integration target определяется top-level `tests/<stem>.rs`.
    let integration_marker = format!("crates/{crate_directory}/tests/");
    // Source under tests directory должен иметь exact filename либо registered module target.
    if let Some(target_relative_path) = evidence_path.strip_prefix(&integration_marker) {
        // Nested source требует реальный root `tests/<target>.rs`.
        let root_target_source = target_relative_path.contains('/').then(|| {
            // Root path определяется declared target, а не nested filename.
            let root_target_path = workspace_root()
                .join("crates")
                .join(crate_directory)
                .join("tests")
                .join(format!("{target}.rs"));
            // Missing root означает, что Cargo никогда не выполнит nested test.
            fs::read_to_string(&root_target_path).unwrap_or_else(|error| {
                panic!(
                    "test evidence `{evidence_id}` не удалось прочитать integration root {}: {error}",
                    root_target_path.display()
                )
            })
        });
        // Pure owner check валидирует target/path и exact root registration.
        assert_integration_target_ownership(
            evidence_id,
            target_relative_path,
            target,
            root_target_source.as_deref(),
        );
    } else {
        // Tests внутри `src` выполняются Cargo lib target-ом.
        assert_eq!(
            target, "lib",
            "test evidence `{evidence_id}` внутри src обязано иметь target `lib`"
        );
    }
}

/// Проверяет, что symbol является реальной `#[test] fn`, а не совпадением строки.
pub(super) fn assert_real_test_function(evidence_id: &str, evidence: &Value, source: &str) {
    // Exact function name берётся из typed evidence.
    let expected_symbol = required_string(evidence, "symbol");
    // Предыдущая непустая строка нужна для exact `#[test]` attribute.
    let mut previous_nonempty_line: Option<&str> = None;
    // Найденная function declaration завершает scan.
    let mut found_test_function = false;
    // Source анализируется line-wise без выполнения Rust parser-а.
    for source_line in source.lines() {
        // Whitespace не является частью declaration identity.
        let trimmed_line = source_line.trim();
        // Пустые строки не разрывают attribute/function adjacency semantics.
        if trimmed_line.is_empty() {
            // Следующая непустая строка всё ещё видит предыдущий attribute.
            continue;
        }
        // Standard test function может быть sync либо async.
        let sync_signature = format!("fn {expected_symbol}(");
        // Async форма остаётся допустимой при стандартном `#[test]`.
        let async_signature = format!("async fn {expected_symbol}(");
        // Exact declaration не принимает похожий longer symbol.
        if trimmed_line.starts_with(&sync_signature) || trimmed_line.starts_with(&async_signature) {
            // Реальная executable test обязана иметь непосредственный standard attribute.
            assert_eq!(
                previous_nonempty_line,
                Some("#[test]"),
                "evidence `{evidence_id}` symbol `{expected_symbol}` не является `#[test] fn`"
            );
            // Успешный exact match фиксируется один раз.
            found_test_function = true;
            // Дальнейший source не нужен.
            break;
        }
        // Текущая непустая строка станет predecessor следующей.
        previous_nonempty_line = Some(trimmed_line);
    }
    // Простое упоминание symbol в строке не проходит этот assertion.
    assert!(
        found_test_function,
        "evidence `{evidence_id}` не содержит real test function `{expected_symbol}`"
    );
}

#[cfg(test)]
mod tests {
    // Focused tests проверяют production declarations и integration reachability.
    use super::{assert_integration_target_ownership, assert_real_production_symbol};
    use serde_json::json;

    /// Реальная declaration принимается ровно один раз.
    #[test]
    fn production_symbol_requires_exact_declaration_prefix() {
        let evidence = json!({"symbol": "pub fn prepare_media"});

        assert_real_production_symbol(
            "production-positive",
            &evidence,
            "pub fn prepare_media() {}\n",
        );
    }

    /// Internal visibility может меняться без подмены intent-shaped symbol-а.
    #[test]
    fn production_symbol_accepts_internal_visibility_prefix() {
        let evidence = json!({"symbol": "struct PreparedMedia"});

        assert_real_production_symbol(
            "production-internal",
            &evidence,
            "pub(super) struct PreparedMedia {\n    generation: u64,\n}\n",
        );
    }

    /// Comment/string упоминания не являются production boundary.
    #[test]
    #[should_panic(expected = "ровно одну production declaration")]
    fn production_symbol_rejects_comment_and_string_mentions() {
        let evidence = json!({"symbol": "pub fn prepare_media"});

        assert_real_production_symbol(
            "production-negative",
            &evidence,
            "// pub fn prepare_media() {}\nconst LABEL: &str = \"pub fn prepare_media\";\n",
        );
    }

    /// Registered nested integration module считается executable evidence.
    #[test]
    fn nested_integration_module_requires_exact_path_registration() {
        // Root source повторяет реальную explicit path form modularized target-а.
        let root_source = r#"
            #[path = "final_acceptance_s42/profile_traceability.rs"]
            mod profile_traceability;
        "#;
        // Exact directory, target, path и module declaration должны пройти.
        assert_integration_target_ownership(
            "nested-positive",
            "final_acceptance_s42/profile_traceability.rs",
            "final_acceptance_s42",
            Some(root_source),
        );
    }

    /// Convention-only nested helper без explicit root registration отвергается.
    #[test]
    fn nested_integration_module_rejects_missing_or_wrong_registration() {
        // Wrong module declaration не связывает declared evidence path с target.
        let wrong_registration = r#"
            #[path = "final_acceptance_s42/profile_traceability.rs"]
            mod another_module;
        "#;
        // Panic является ожидаемым fail-closed outcome pure helper-а.
        let missing_registration = std::panic::catch_unwind(|| {
            assert_integration_target_ownership(
                "nested-negative",
                "final_acceptance_s42/profile_traceability.rs",
                "final_acceptance_s42",
                Some(wrong_registration),
            );
        });
        // Silent acceptance создала бы fake executable trace.
        assert!(missing_registration.is_err());
    }

    /// Nested directory не может маскировать другой Cargo target.
    #[test]
    fn nested_integration_module_rejects_target_mismatch_and_deeper_helpers() {
        // Directory/target mismatch обязан fail closed до source scan.
        let target_mismatch = std::panic::catch_unwind(|| {
            assert_integration_target_ownership(
                "nested-target-mismatch",
                "final_acceptance_s42/profile_traceability.rs",
                "another_target",
                Some(""),
            );
        });
        // Target alias не допускается.
        assert!(target_mismatch.is_err());
        // Более глубокий helper не является directly registered module.
        let deeper_helper = std::panic::catch_unwind(|| {
            assert_integration_target_ownership(
                "nested-too-deep",
                "final_acceptance_s42/support/evidence_source.rs",
                "final_acceptance_s42",
                Some(""),
            );
        });
        // Только exact `tests/<target>/<module>.rs` разрешён.
        assert!(deeper_helper.is_err());
    }
}
