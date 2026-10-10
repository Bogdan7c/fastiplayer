//! B4: пропуск неподходящих записей, честный отказ «нечего экспортировать» и
//! человеческие тексты сбоев записи файла.

use super::*;

/// Очередь «ссылка, локальный файл с относительным путём, ссылка».
fn queue_with_middle_entry_unexportable() -> PlaylistQueue {
    let mut queue = PlaylistQueue::new();
    for draft in [
        PlaylistItemDraft::url(
            SecretUrlLocator::from_reopenable_url("https://media.example.test/one.mp4")
                .expect("url one"),
            metadata("one"),
        ),
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("relative/broken.webm")),
            None,
            metadata("broken"),
        ),
        PlaylistItemDraft::url(
            SecretUrlLocator::from_reopenable_url("https://media.example.test/three.mp4")
                .expect("url three"),
            metadata("three"),
        ),
    ] {
        queue.append_one(draft).expect("append fixture");
    }
    queue
}

fn run_export(queue: &PlaylistQueue, target_path: PathBuf) -> PlaylistExportJobCompletion {
    let snapshot =
        PlaylistExportSnapshot::capture(queue, PlaylistExportScope::Full).expect("snapshot");
    prepare_and_write_export(
        1,
        PlaylistExportFormat::M3u8,
        snapshot,
        target_path,
        PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
        &AtomicBool::new(false),
    )
}

#[test]
fn one_unexportable_entry_is_skipped_and_file_has_the_rest() {
    let directory = tempdir().expect("tempdir");
    let target_path = directory.path().join("out.m3u8");

    let completion = run_export(&queue_with_middle_entry_unexportable(), target_path.clone());

    let PlaylistExportJobCompletion::Written {
        report, durability, ..
    } = completion
    else {
        panic!("экспорт обязан завершиться записью, получено {completion:?}");
    };
    assert_eq!(report.exported_tracks, 2);
    assert_eq!(report.skipped.total(), 1);
    assert_eq!(report.skipped.local_path_not_representable, 1);
    let file = fs::read_to_string(&target_path).expect("export file");
    assert!(
        file.contains("one.mp4") && file.contains("three.mp4"),
        "{file}"
    );
    assert!(!file.contains("broken"), "{file}");
    assert_eq!(
        messages::written_feedback(durability, &report).as_deref(),
        Some("Экспортировано 2, пропущено 1: путь к локальному файлу нельзя записать")
    );
}

#[test]
fn all_entries_unexportable_creates_no_file_and_says_nothing_to_export() {
    let directory = tempdir().expect("tempdir");
    let target_path = directory.path().join("empty.m3u8");
    let mut queue = PlaylistQueue::new();
    queue
        .append_one(PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from("relative/a.webm")),
            None,
            metadata("a"),
        ))
        .expect("append");

    let completion = run_export(&queue, target_path.clone());

    let PlaylistExportJobCompletion::Failed { error, .. } = completion else {
        panic!("ожидался отказ, получено {completion:?}");
    };
    assert!(
        matches!(error, PlaylistExportJobError::NothingToExport(counts) if counts.total() == 1)
    );
    assert!(!target_path.exists(), "пустой файл создаваться не должен");
    assert_eq!(
        messages::failure_feedback(error),
        "Нечего экспортировать: ни одна из 1 записей не подходит для этого формата \
         (путь к локальному файлу нельзя записать)"
    );
}

#[test]
fn several_skip_reasons_are_listed_briefly() {
    let report = PlaylistExportReport {
        exported_tracks: 4,
        skipped: PlaylistExportSkipCounts {
            local_path_not_representable: 1,
            no_stable_web_link: 2,
            other: 0,
        },
        flattened_compound_groups: true,
    };
    assert_eq!(
        messages::written_feedback(PlaylistExportDurability::Durable, &report).as_deref(),
        Some(
            "Экспортировано 4, пропущено 3: ссылки без стабильного адреса — 2, \
             файлы с неподходящим путём — 1. Составные группы записаны отдельными строками"
        )
    );
}

#[test]
fn clean_export_has_no_feedback() {
    assert_eq!(
        messages::written_feedback(PlaylistExportDurability::Durable, &single_track_report()),
        None
    );
}

#[cfg(unix)]
#[test]
fn read_only_directory_gives_permission_text_without_directory_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempdir().expect("tempdir");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555))
        .expect("make directory read-only");
    let target_path = directory.path().join("out.m3u8");
    // root игнорирует права доступа: в таком окружении сценарий не воспроизвести.
    let can_still_write = fs::write(directory.path().join("probe"), b"x").is_ok();

    let completion = run_export(&queue_with_middle_entry_unexportable(), target_path.clone());

    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).expect("restore");
    if can_still_write {
        return;
    }
    let PlaylistExportJobCompletion::Failed { error, .. } = completion else {
        panic!("запись в read-only каталог обязана провалиться: {completion:?}");
    };
    assert_eq!(
        error,
        PlaylistExportJobError::WriteFailed(PlaylistExportWriteFailure::PermissionDenied)
    );
    let text = messages::failure_feedback(error);
    assert_eq!(
        text,
        "Не удалось сохранить плейлист: нет прав на запись в выбранную папку"
    );
    assert!(!text.contains(directory.path().to_string_lossy().as_ref()));
    assert!(!target_path.exists(), "полуфайл остаться не должен");
}

#[test]
fn io_kinds_map_to_human_write_failures() {
    use PlaylistExportWriteFailure as Failure;
    assert_eq!(
        Failure::from_io_kind(io::ErrorKind::StorageFull),
        Failure::DiskFull
    );
    assert_eq!(
        Failure::from_io_kind(io::ErrorKind::QuotaExceeded),
        Failure::DiskFull
    );
    assert_eq!(
        Failure::from_io_kind(io::ErrorKind::ReadOnlyFilesystem),
        Failure::ReadOnlyFilesystem
    );
    assert_eq!(
        Failure::from_io_kind(io::ErrorKind::NotFound),
        Failure::DirectoryMissing
    );
    assert_eq!(Failure::from_io_kind(io::ErrorKind::Other), Failure::Other);
}

#[test]
fn disk_full_from_writer_reaches_user_text() {
    let completion = write_prepared_export_with(
        continuation(1, PathBuf::from("/tmp/redacted-full.m3u8")),
        |_path, _bytes| {
            atomic_file_store::AtomicFileWriteOutcome::NotReplaced(
                atomic_file_store::AtomicFileWriteFailure {
                    stage: atomic_file_store::AtomicFileWriteStage::WriteTempFile,
                    cause: atomic_file_store::AtomicFileWriteCause::Io(io::ErrorKind::StorageFull),
                },
            )
        },
    );
    let PlaylistExportJobCompletion::Failed { error, .. } = completion else {
        panic!("ожидался отказ записи: {completion:?}");
    };
    assert_eq!(
        messages::failure_feedback(error),
        "Не удалось сохранить плейлист: на диске не хватает места"
    );
}
