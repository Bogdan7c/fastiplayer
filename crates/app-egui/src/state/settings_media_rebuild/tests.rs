//! Охранные тесты неблокирующего пути пересборки.
//!
//! `AppState` нельзя собрать без настоящего renderer-а, поэтому сквозная цепочка
//! (Preparing -> Installing -> RestoringState) проверяется ручной приёмкой, а здесь
//! закреплено то, что регрессирует незаметно: UI-путь не возвращается к блокирующим вызовам.

/// Блокирующие вызовы, которым нет места в неблокирующем пути.
const FORBIDDEN_BLOCKING_CALLS: [&str; 5] = [
    "install_prepared_media_strong(",
    "prepare_rebuild_source(",
    ".wait_for_outcome(",
    ".recv()",
    "restore_playback_after_media_reconfigure(",
];

#[test]
fn owner_module_never_calls_blocking_install_prepare_or_waits() {
    let owner_source = include_str!("../settings_media_rebuild.rs");
    // Тестовый модуль в конце файла не считается: его guardrail-текст содержит эти имена.
    let production_part = owner_source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or(owner_source);

    for call in FORBIDDEN_BLOCKING_CALLS {
        assert!(
            !production_part.contains(call),
            "the owner of the reassembly must not cause a blocking \"{call}\""
        );
    }
}

#[test]
fn stepwise_branch_of_the_adapter_does_not_prepare_or_install_on_the_ui_thread() {
    let adapter_source = include_str!("../../frame_prepare/settings_runtime_adapter.rs");
    let reconfigure_section = source_between(
        adapter_source,
        "fn reconfigure_active_media(",
        "fn rebuild_active_media_blocking(",
    );

    for call in FORBIDDEN_BLOCKING_CALLS {
        assert!(
            !reconfigure_section.contains(call),
            "'reconfigure_active_media' should not directly cause '{call}': blocking work lives only in 'rebuild_active_media_blocking' (rollback)"
        );
    }
    assert!(
        reconfigure_section.contains("begin_settings_media_rebuild("),
        "stepwise branch must run reassembly through the owner in AppState"
    );
}

/// Возвращает кусок исходника между двумя маркерами (оба обязаны существовать).
fn source_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_index = source
        .find(start)
        .unwrap_or_else(|| panic!("{start} marker not found"));
    let tail = &source[start_index..];
    let end_index = tail
        .find(end)
        .unwrap_or_else(|| panic!("{end} marker not found"));
    &tail[..end_index]
}
