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
        sentences.push(
            "Файл заменён, но файловая система не подтвердила долговечность записи".to_owned(),
        );
    }
    if report.skipped.total() > 0 {
        sentences.push(format!(
            "Экспортировано {}, пропущено {}: {}",
            report.exported_tracks,
            report.skipped.total(),
            skip_reasons_text(report.skipped)
        ));
    }
    if report.flattened_compound_groups {
        sentences.push("Составные группы записаны отдельными строками".to_owned());
    }
    (!sentences.is_empty()).then(|| sentences.join(". "))
}

/// Текст терминального отказа всего экспорта.
pub(super) fn failure_feedback(error: PlaylistExportJobError) -> String {
    match error {
        PlaylistExportJobError::InvalidTarget => {
            "Выбранный путь нельзя использовать для экспорта".to_owned()
        }
        PlaylistExportJobError::LocatorIneligible => {
            "Плейлист содержит источник, который нельзя безопасно экспортировать".to_owned()
        }
        PlaylistExportJobError::NothingToExport(skipped) => format!(
            "Нечего экспортировать: ни одна из {} записей не подходит для этого формата ({})",
            skipped.total(),
            skip_reasons_text(skipped)
        ),
        PlaylistExportJobError::WriteFailed(failure) => write_failure_text(failure).to_owned(),
        PlaylistExportJobError::WorkerPanicked => {
            "Фоновый экспорт плейлиста аварийно завершился".to_owned()
        }
    }
}

fn write_failure_text(failure: PlaylistExportWriteFailure) -> &'static str {
    match failure {
        PlaylistExportWriteFailure::PermissionDenied => {
            "Не удалось сохранить плейлист: нет прав на запись в выбранную папку"
        }
        PlaylistExportWriteFailure::ReadOnlyFilesystem => {
            "Не удалось сохранить плейлист: выбранная папка доступна только для чтения"
        }
        PlaylistExportWriteFailure::DiskFull => {
            "Не удалось сохранить плейлист: на диске не хватает места"
        }
        PlaylistExportWriteFailure::DirectoryMissing => {
            "Не удалось сохранить плейлист: выбранной папки не существует"
        }
        PlaylistExportWriteFailure::Other => "Не удалось сохранить файл плейлиста",
    }
}

/// Одна причина — фраза целиком; несколько — короткий список с числами.
fn skip_reasons_text(skipped: PlaylistExportSkipCounts) -> String {
    let categories = [
        (
            skipped.no_stable_web_link,
            "у ссылки нет стабильного адреса",
            "ссылки без стабильного адреса",
        ),
        (
            skipped.local_path_not_representable,
            "путь к локальному файлу нельзя записать",
            "файлы с неподходящим путём",
        ),
        (skipped.other, "формат не поддерживает запись", "прочее"),
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
