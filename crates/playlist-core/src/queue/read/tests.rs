use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    CachedPlaylistMetadata, ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath,
    LocalLocator, PlaylistCompoundGroupDraft, PlaylistEntryDraft, PlaylistItemDraft,
    PlaylistLocator, PlaylistMediaKind, SecretUrlLocator,
};

use super::super::{
    AddItemsOutcome, MoveItemIntent, MoveItemOutcome, PlaylistMetadataPatch, PlaylistQueue,
};

/// Общий предел production queue-модуля до обязательной декомпозиции.
const DEFAULT_QUEUE_MODULE_MAXIMUM_LINES: usize = 800;

/// Центральный storage owner получает более строгий предел.
const QUEUE_OWNER_MAXIMUM_LINES: usize = 700;

/// Entry allocation owner должен оставаться компактнее центрального owner-а.
const QUEUE_ENTRIES_MAXIMUM_LINES: usize = 600;

/// Shuffle runtime уже близок к порогу и не должен поглощать новую policy logic.
const QUEUE_SHUFFLE_RUNTIME_MAXIMUM_LINES: usize = 750;

/// Существующий typed outcome vocabulary временно имеет отдельный явный предел.
const QUEUE_OUTCOMES_MAXIMUM_LINES: usize = 900;

/// First-class entry/payload owners остаются ниже общей границы queue-модулей.
const PLAYLIST_DOMAIN_OWNER_MAXIMUM_LINES: usize = 700;

/// Создаёт deterministic URL draft без network I/O.
fn url_draft(raw_url: &str, label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::url(
        SecretUrlLocator::from_reopenable_url(raw_url).expect("test URL must be valid"),
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Unknown),
    )
}

/// Создаёт compound draft для проверки derived traversal без flat queue cache.
fn compound_draft(label: &str, part_count: usize) -> PlaylistEntryDraft {
    // Каждая part получает различимый URL, чтобы exact order был наблюдаемым.
    let parts = (0..part_count)
        .map(|part_index| {
            url_draft(
                &format!("https://example.invalid/{label}/part-{part_index}"),
                &format!("{label} part {part_index}"),
            )
        })
        .collect();
    // Root provenance не является playable part и остаётся на group boundary.
    let group = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(
            SecretUrlLocator::from_reopenable_url(format!("https://example.invalid/{label}/root"))
                .expect("compound root URL must be valid"),
        ),
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
        parts,
    )
    .expect("compound fixture must contain at least one part");
    // Явный variant сохраняет one-part group как compound.
    PlaylistEntryDraft::Compound(group)
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

/// Отличает production module от colocated unit-test module по имени файла.
fn is_playlist_queue_test_source(source_path: &Path) -> bool {
    source_path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name == "tests.rs" || file_name.ends_with("_tests.rs"))
}

/// Возвращает именованный line budget конкретного production queue owner-а.
fn playlist_queue_module_maximum_lines(relative_path: &Path) -> usize {
    match relative_path.to_str() {
        Some("mod.rs") => QUEUE_OWNER_MAXIMUM_LINES,
        Some("entries.rs") => QUEUE_ENTRIES_MAXIMUM_LINES,
        Some("shuffle/runtime.rs") => QUEUE_SHUFFLE_RUNTIME_MAXIMUM_LINES,
        Some("outcomes.rs") => QUEUE_OUTCOMES_MAXIMUM_LINES,
        _ => DEFAULT_QUEUE_MODULE_MAXIMUM_LINES,
    }
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
        queue.move_item(
            crate::PlaylistEntryId::Single(committed_ids[0]),
            MoveItemIntent::ToBack,
        ),
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
fn derived_compound_iterator_keeps_exact_len_when_both_ends_are_consumed() {
    // Fixture смешивает Single, one-part и many-part entries.
    let mut queue = PlaylistQueue::new();
    let allocation = queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(url_draft("https://example.invalid/first", "first")),
            compound_draft("one-part", 1),
            compound_draft("many-parts", 3),
            PlaylistEntryDraft::Single(url_draft("https://example.invalid/last", "last")),
        ])
        .expect("mixed compound fixture must commit");
    let crate::AddPlaylistEntriesOutcome::Added(allocated) = allocation else {
        panic!("non-empty fixture must allocate identities");
    };
    let expected_ids = allocated.iter_playable_item_ids().collect::<Vec<_>>();

    // ExactSizeIterator обязан сообщать точный остаток после каждого направления.
    let mut playable_ids = queue.iter_playable_ids();
    assert_eq!(playable_ids.len(), 6);
    assert_eq!(playable_ids.next(), Some(expected_ids[0]));
    assert_eq!(playable_ids.len(), 5);
    assert_eq!(playable_ids.next_back(), Some(expected_ids[5]));
    assert_eq!(playable_ids.len(), 4);
    assert_eq!(playable_ids.next(), Some(expected_ids[1]));
    assert_eq!(playable_ids.len(), 3);
    assert_eq!(playable_ids.next_back(), Some(expected_ids[4]));
    assert_eq!(playable_ids.len(), 2);
    assert_eq!(playable_ids.next(), Some(expected_ids[2]));
    assert_eq!(playable_ids.len(), 1);
    assert_eq!(playable_ids.next_back(), Some(expected_ids[3]));
    assert_eq!(playable_ids.len(), 0);
    assert_eq!(playable_ids.next(), None);
    assert_eq!(playable_ids.next_back(), None);
}

#[test]
fn playlist_queue_storage_and_owner_modules_stay_hardened() {
    // Gate читает только checked-in production sources и не зависит от shell tools.
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_directory = manifest_directory.join("src");
    let queue_owner_path = source_directory.join("queue/mod.rs");
    let queue_owner_source =
        fs::read_to_string(&queue_owner_path).expect("queue owner source must be readable");

    // PlaylistQueue хранит только nested top-level entries без parallel playable Vec/snapshot.
    let queue_fields = queue_owner_source
        .split_once("pub struct PlaylistQueue {")
        .and_then(|(_, suffix)| suffix.split_once("}\n"))
        .map(|(fields, _)| fields)
        .expect("PlaylistQueue fields must remain discoverable");
    assert_eq!(queue_fields.matches("Vec<PlaylistEntry>").count(), 1);
    assert_eq!(queue_fields.matches("Vec<").count(), 1);
    assert!(!queue_fields.contains("Arc<"));
    assert!(!queue_fields.contains("OwnedPlayableItemsSnapshot"));

    // Любой новый production queue-модуль автоматически попадает под default budget.
    let queue_source_directory = source_directory.join("queue");
    for source_path in rust_source_paths(&queue_source_directory)
        .into_iter()
        .filter(|source_path| !is_playlist_queue_test_source(source_path))
    {
        let relative_path = source_path
            .strip_prefix(&queue_source_directory)
            .expect("queue source must stay below queue source directory");
        let maximum_lines = playlist_queue_module_maximum_lines(relative_path);
        let source = fs::read_to_string(&source_path).expect("owner source must be readable");
        let actual_lines = source.lines().count();
        assert!(
            actual_lines <= maximum_lines,
            "{} содержит {actual_lines} строк при gate-лимите {maximum_lines}; \
             новую логику нужно вынести к отдельному owner-у",
            relative_path.display()
        );
    }

    // First-class entry/payload owners также не должны разрастаться вокруг queue boundary.
    for relative_path in ["entry.rs", "payload.rs"] {
        let source_path = source_directory.join(relative_path);
        let source =
            fs::read_to_string(&source_path).expect("domain owner source must be readable");
        let actual_lines = source.lines().count();
        assert!(
            actual_lines <= PLAYLIST_DOMAIN_OWNER_MAXIMUM_LINES,
            "{relative_path} содержит {actual_lines} строк при gate-лимите \
             {PLAYLIST_DOMAIN_OWNER_MAXIMUM_LINES}; новую логику нужно вынести в отдельный модуль"
        );
    }
}

#[test]
fn workspace_has_no_legacy_playlist_queue_read_surface_or_callers() {
    // S01Q закрывает временный bridge и запрещает его возврат во всех dependent crates.
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository_root = manifest_directory
        .parent()
        .and_then(Path::parent)
        .expect("playlist-core must live under repository crates directory");

    // Компиляция доказывает отсутствие typed callers, а source audit ловит возврат bridge заранее.
    for migrated_source_root in [
        manifest_directory.join("src"),
        repository_root.join("crates/playlist-state/src"),
        repository_root.join("crates/app-egui/src"),
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
                "workspace source returned to legacy queue read API: {}",
                source_path.display()
            );
        }
    }
}
