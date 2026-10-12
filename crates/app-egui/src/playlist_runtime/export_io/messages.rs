//! Русские тексты итога экспорта плейлиста. Без путей к каталогам и без
//! технических деталей (они остаются в tracing).

use playlist_io::PlaylistExportSkipCounts;

use super::report::{PlaylistExportReport, PlaylistExportWriteFailure};
use super::{PlaylistExportDurability, PlaylistExportJobError};

/// Текст после успешной записи; `None`, если сообщать нечего (всё записано без
/// оговорок).
pub(super) fn written_feedback(
    durability: PlaylistExportDurability,
    report: &PlaylistExportReport,
) -> Option<String> {
    let mut sentences = Vec::new();
    if durability == PlaylistExportDurability::ReplacedDurabilityUnconfirmed {
        sentences
            .push("File replaced, but the file system did not confirm durable storage".to_owned());
    }
    if report.skipped.total() > 0 {
        sentences.push(format!(
            "Exported {}, skipped {}: {}",
            report.exported_tracks,
            report.skipped.total(),
            skip_reasons_text(report.skipped)
        ));
    }
    if report.flattened_compound_groups {
        sentences.push("Compound groups were written as separate rows".to_owned());
    }
    (!sentences.is_empty()).then(|| sentences.join(". "))
}

/// Текст терминального отказа всего экспорта.
pub(super) fn failure_feedback(error: PlaylistExportJobError) -> String {
    match error {
        PlaylistExportJobError::InvalidTarget => {
            "The selected path cannot be used for export".to_owned()
        }
        PlaylistExportJobError::LocatorIneligible => {
            "Playlist contains a source that cannot be exported safely".to_owned()
        }
        PlaylistExportJobError::NothingToExport(skipped) => format!(
            "Nothing to export: none of the {} entries is suitable for this format ({})",
            skipped.total(),
            skip_reasons_text(skipped)
        ),
        PlaylistExportJobError::WriteFailed(failure) => write_failure_text(failure).to_owned(),
        PlaylistExportJobError::WorkerPanicked => "Background Playlist export crashed".to_owned(),
    }
}

fn write_failure_text(failure: PlaylistExportWriteFailure) -> &'static str {
    match failure {
        PlaylistExportWriteFailure::PermissionDenied => {
            "Could not save Playlist: no permission to write to the selected folder"
        }
        PlaylistExportWriteFailure::ReadOnlyFilesystem => {
            "Could not save Playlist: the selected folder is read-only"
        }
        PlaylistExportWriteFailure::DiskFull => "Could not save Playlist: not enough disk space",
        PlaylistExportWriteFailure::DirectoryMissing => {
            "Could not save Playlist: the selected folder does not exist"
        }
        PlaylistExportWriteFailure::Other => "Could not save the Playlist file",
    }
}

/// Одна причина — фраза целиком; несколько — короткий список с числами.
fn skip_reasons_text(skipped: PlaylistExportSkipCounts) -> String {
    let categories = [
        (
            skipped.no_stable_web_link,
            "link has no stable address",
            "links without stable addresses",
        ),
        (
            skipped.local_path_not_representable,
            "local file path cannot be written",
            "files with unsuitable paths",
        ),
        (skipped.other, "format does not support this entry", "other"),
    ];
    let present: Vec<_> = categories.iter().filter(|(count, ..)| *count > 0).collect();
    match present.as_slice() {
        [(_, single_phrase, _)] => (*single_phrase).to_owned(),
        _ => present
            .iter()
            .map(|(count, _, short_label)| format!("{short_label} — {count}"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}
