use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    CachedPlaylistMetadata, ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath,
    LocalLocator, PlaylistItemDraft, PlaylistLocator, PlaylistMediaKind, SecretUrlLocator,
};

use super::super::{
    AddItemsOutcome, MoveItemIntent, MoveItemOutcome, PlaylistMetadataPatch, PlaylistQueue,
};

/// Создаёт deterministic URL draft без network I/O.
fn url_draft(raw_url: &str, label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::url(
        SecretUrlLocator::from_reopenable_url(raw_url).expect("test URL must be valid"),
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Unknown),
    )
}

/// Извлекает allocated IDs успешного non-empty append.
fn committed_ids(outcome: AddItemsOutcome) -> Vec<crate::PlaylistItemId> {
    match outcome {
        AddItemsOutcome::Added(item_ids) => item_ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("test batch must not be empty"),
    }
}

/// Собирает Rust sources под root без зависимости от shell/tooling окружения.
fn rust_source_paths(root: &Path) -> Vec<PathBuf> {
    let mut pending_directories = vec![root.to_path_buf()];
    let mut source_paths = Vec::new();
    while let Some(directory) = pending_directories.pop() {
        for entry in fs::read_dir(&directory).expect("test source directory must be readable") {
            let path = entry.expect("test source entry must be readable").path();
            if path.is_dir() {
                pending_directories.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                source_paths.push(path);
            }
        }
    }
    source_paths.sort();
    source_paths
}

/// Удаляет whitespace, чтобы multiline method calls считались одним intent callsite.
fn compact_source(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// Считает non-overlapping occurrences фиксированного source token sequence.
fn occurrence_count(source: &str, needle: &str) -> usize {
    source.match_indices(needle).count()
}

/// Строит exact per-file inventory только для файлов с оставшимися callsites.
fn caller_inventory(
    repository_root: &Path,
    source_root: &Path,
    count_calls: impl Fn(&str) -> usize,
) -> BTreeMap<String, usize> {
    rust_source_paths(source_root)
        .into_iter()
        .filter_map(|source_path| {
            let source = fs::read_to_string(&source_path).expect("Rust source must be UTF-8");
            let call_count = count_calls(&compact_source(&source));
            (call_count > 0).then(|| {
                let relative_path = source_path
                    .strip_prefix(repository_root)
                    .expect("source must remain under repository root")
                    .to_string_lossy()
                    .into_owned();
                (relative_path, call_count)
            })
        })
        .collect()
}

/// Превращает readable allowlist pairs в deterministic inventory map.
fn expected_inventory(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
    entries
        .iter()
        .map(|(path, count)| ((*path).to_owned(), *count))
        .collect()
}

#[test]
fn borrowed_read_intents_preserve_order_duplicates_counts_and_non_utf_identity() {
    // Два одинаковых locator обязаны остаться разными playable Item IDs.
    let duplicate_url = "https://example.invalid/watch?token=secret";
    let non_utf_units = vec![b'/', b'm', 0xFF, b'.', b'm', b'k', b'v'];
    let non_utf_locator = LocalLocator::Foreign(ForeignPlatformPath::new(
        ForeignPathPlatform::Linux,
        ForeignPathEncoding::Bytes(non_utf_units.clone()),
    ));
    let mut queue = PlaylistQueue::new();
    let committed_ids = committed_ids(
        queue
            .append_batch(vec![
                url_draft(duplicate_url, "first duplicate"),
                url_draft(duplicate_url, "second duplicate"),
                PlaylistItemDraft::local(
                    non_utf_locator.clone(),
                    None,
                    CachedPlaylistMetadata::new("non-UTF", PlaylistMediaKind::Video),
                ),
            ])
            .expect("test batch must commit"),
    );

    let playable_items = queue.iter_playable_items();
    assert_eq!(playable_items.len(), 3);
    assert_eq!(queue.top_level_entry_count(), 3);
    assert_eq!(queue.retained_item_count(), 3);
    assert_eq!(queue.iter_playable_ids().collect::<Vec<_>>(), committed_ids);
    assert_eq!(
        queue.item(committed_ids[0]).map(|item| item.locator()),
        queue.item(committed_ids[1]).map(|item| item.locator())
    );
    assert_eq!(
        queue.item(committed_ids[2]).map(|item| item.locator()),
        Some(&PlaylistLocator::Local(non_utf_locator))
    );
    let PlaylistLocator::Local(LocalLocator::Foreign(foreign_path)) = queue
        .item(committed_ids[2])
        .expect("non-UTF item must be committed")
        .locator()
    else {
        panic!("non-UTF locator must remain foreign and reversible");
    };
    assert_eq!(
        foreign_path.encoding_for_persistence(),
        &ForeignPathEncoding::Bytes(non_utf_units)
    );
}

#[test]
fn owned_snapshot_has_read_parity_but_never_becomes_mutation_authority() {
    let mut queue = PlaylistQueue::new();
    let committed_ids = committed_ids(
        queue
            .append_batch(vec![
                url_draft("https://user:password@example.invalid/a?token=secret", "a"),
                url_draft("https://example.invalid/b", "b"),
                url_draft("https://example.invalid/c", "c"),
            ])
            .expect("test batch must commit"),
    );
    let snapshot = queue.owned_playable_items_snapshot();

    assert!(!format!("{snapshot:?}").contains("password"));
    assert!(!format!("{snapshot:?}").contains("token=secret"));
    assert_eq!(
        snapshot.iter_playable_ids().collect::<Vec<_>>(),
        queue.iter_playable_ids().collect::<Vec<_>>()
    );
    assert_eq!(snapshot.retained_item_count(), queue.retained_item_count());
    assert_eq!(
        snapshot.item(committed_ids[0]).map(|item| item.item_id()),
        Some(committed_ids[0])
    );

    // Structural mutation публикуется только через queue owner, snapshot остаётся прежним.
    assert!(matches!(
        queue.move_item(committed_ids[0], MoveItemIntent::ToBack),
        MoveItemOutcome::Moved { .. }
    ));
    let original_locator = snapshot
        .item(committed_ids[0])
        .expect("snapshot retains first item")
        .locator()
        .clone();
    queue
        .apply_metadata_patch_batch(vec![PlaylistMetadataPatch::new(
            committed_ids[0],
            original_locator,
            None,
            CachedPlaylistMetadata::new("changed", PlaylistMediaKind::Audio),
        )])
        .expect("metadata patch must commit");

    assert_eq!(
        snapshot.iter_playable_ids().collect::<Vec<_>>(),
        committed_ids
    );
    assert_eq!(
        snapshot
            .item(committed_ids[0])
            .expect("snapshot item remains readable")
            .cached_metadata()
            .fallback_display_name(),
        "a"
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        vec![committed_ids[1], committed_ids[2], committed_ids[0]]
    );
}

#[test]
fn legacy_slice_and_ambiguous_len_callers_match_exact_s01q_inventory() {
    // S01P мигрирует domain/state целиком, а app-wide callers намеренно оставляет S01Q.
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository_root = manifest_directory
        .parent()
        .and_then(Path::parent)
        .expect("playlist-core must live under repository crates directory");
    let app_source_root = repository_root.join("crates/app-egui/src");

    let items_inventory = caller_inventory(repository_root, &app_source_root, |source| {
        occurrence_count(source, ".items()")
    });
    let expected_items_inventory = expected_inventory(&[
        ("crates/app-egui/src/playlist_runtime/actions.rs", 2),
        (
            "crates/app-egui/src/playlist_runtime/controller/discovery/tests.rs",
            2,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/initial_queue_playback.rs",
            1,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/local_file_selection.rs",
            2,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/removal.rs",
            7,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/removal/clear.rs",
            1,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/removal/tests.rs",
            1,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/reordering.rs",
            4,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/tests.rs",
            2,
        ),
        (
            "crates/app-egui/src/playlist_runtime/discovery/action_jobs/tests.rs",
            1,
        ),
        (
            "crates/app-egui/src/playlist_runtime/discovery/metadata_sort.rs",
            1,
        ),
        (
            "crates/app-egui/src/playlist_runtime/discovery/metadata_sort/tests.rs",
            2,
        ),
        ("crates/app-egui/src/playlist_runtime/persistence.rs", 1),
        (
            "crates/app-egui/src/playlist_runtime/replacement_confirmation/tests.rs",
            4,
        ),
        ("crates/app-egui/src/playlist_runtime/selection.rs", 5),
        ("crates/app-egui/src/playlist_runtime/startup/tests.rs", 1),
        ("crates/app-egui/src/playlist_runtime/view.rs", 1),
        ("crates/app-egui/src/ui/playlist/tests.rs", 7),
        ("crates/app-egui/src/ui/sidebar/header.rs", 2),
    ]);
    assert_eq!(items_inventory, expected_items_inventory);

    let len_inventory = caller_inventory(repository_root, &app_source_root, |source| {
        occurrence_count(source, "queue().len()") + occurrence_count(source, "queue.len()")
    });
    let expected_len_inventory = expected_inventory(&[
        ("crates/app-egui/src/playlist_runtime.rs", 1),
        ("crates/app-egui/src/playlist_runtime/actions.rs", 5),
        ("crates/app-egui/src/playlist_runtime/controller.rs", 1),
        (
            "crates/app-egui/src/playlist_runtime/controller/automatic_lifecycle/tests.rs",
            2,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/removal.rs",
            1,
        ),
        (
            "crates/app-egui/src/playlist_runtime/controller/tests.rs",
            2,
        ),
        (
            "crates/app-egui/src/playlist_runtime/discovery/action_jobs/tests.rs",
            4,
        ),
        ("crates/app-egui/src/playlist_runtime/persistence.rs", 2),
        (
            "crates/app-egui/src/playlist_runtime/removal_undo/tests.rs",
            4,
        ),
        ("crates/app-egui/src/playlist_runtime/selection.rs", 2),
        ("crates/app-egui/src/playlist_runtime/startup/tests.rs", 3),
        ("crates/app-egui/src/playlist_runtime/ui_interaction.rs", 1),
        ("crates/app-egui/src/playlist_runtime/view.rs", 2),
        ("crates/app-egui/src/ui/playlist/tests.rs", 1),
    ]);
    assert_eq!(len_inventory, expected_len_inventory);

    // Новые/мигрированные crates не должны снова начать использовать legacy surface.
    for migrated_source_root in [
        manifest_directory.join("src"),
        repository_root.join("crates/playlist-state/src"),
    ] {
        for source_path in rust_source_paths(&migrated_source_root) {
            if source_path.ends_with("queue/read/tests.rs") {
                continue;
            }
            let source = fs::read_to_string(&source_path).expect("Rust source must be UTF-8");
            let compact_source = compact_source(&source);
            assert!(
                !compact_source.contains(".items()")
                    && !compact_source.contains("queue().len()")
                    && !compact_source.contains("queue.len()")
                    && !compact_source.contains("self.len()"),
                "migrated source returned to legacy queue read API: {}",
                source_path.display()
            );
        }
    }
}
