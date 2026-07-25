//! Целостность каталога S42 evidence и всех ссылок на него.

// Ordered set делает сравнение полного reference inventory детерминированным.
use std::collections::BTreeSet;

// Путь к scoped manifest остаётся владельцем root integration target.
use super::S42_ACCEPTANCE_PATH;
// Модуль использует только явный facade общих JSON/evidence helpers.
use super::support::{
    assert_typed_evidence, collect_referenced_evidence_ids, load_json_document, required_object,
};

/// Все catalog entries существуют, содержат symbol и реально используются.
#[test]
fn evidence_catalog_has_no_stale_path_symbol_or_dead_reference() {
    // Загружаем единственный S42 evidence owner.
    let s42_acceptance = load_json_document(S42_ACCEPTANCE_PATH);
    // Catalog является named object, а не positional array.
    let evidence_catalog = required_object(&s42_acceptance, "evidence_catalog");
    // Пустой catalog не может закрыть тринадцать rows.
    assert!(!evidence_catalog.is_empty());

    // Каждый catalog entry проверяется по kind/role/filesystem semantics.
    for (evidence_id, evidence) in evidence_catalog {
        // Object type обязателен даже если helper читает fields через Value.
        assert!(
            evidence.is_object(),
            "evidence `{evidence_id}` обязан быть object"
        );
        // Production boundary, executable test и owner decision валидируются раздельно.
        assert_typed_evidence(evidence_id, evidence);
    }

    // Собираем все references из rows и scoped decisions.
    let mut referenced_ids = BTreeSet::new();
    // Traversal intentionally пропускает catalog values без `evidence_ids`.
    collect_referenced_evidence_ids(&s42_acceptance, &mut referenced_ids);
    // Все references обязаны разрешаться в catalog.
    for evidence_id in &referenced_ids {
        // Missing catalog entry является stale manifest.
        assert!(
            evidence_catalog.contains_key(evidence_id),
            "неизвестный evidence reference `{evidence_id}`"
        );
    }
    // Dead catalog item создавал бы ложное ощущение дополнительного coverage.
    assert_eq!(
        referenced_ids,
        evidence_catalog.keys().cloned().collect::<BTreeSet<_>>()
    );
}
