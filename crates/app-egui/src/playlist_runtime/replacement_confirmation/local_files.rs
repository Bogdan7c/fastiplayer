//! Замена очереди несколькими локальными файлами: типы admission и подпись для диалога.
//!
//! Вынесено из `replacement_confirmation.rs` (модуль близок к лимиту размера). Slot,
//! supersede-правила и Confirm/Cancel остаются у родителя: здесь только данные варианта
//! `QueueReplacementTarget::LocalFiles`.

use std::fmt;
use std::path::PathBuf;

use super::{InAppQueueReplacementIntent, QueueReplacementTarget};
use crate::media_open::SafeMediaLabel;
use crate::playlist_runtime::{DroppedCollectionSource, DroppedCollectionTruncation};

/// Набор локальных файлов, прошедший empty-queue gate либо matching Confirm.
///
/// Порядок путей — порядок броска; потребитель обязан сохранить его в очереди.
pub(crate) struct AdmittedLocalFilesOpen {
    paths: Vec<PathBuf>,
    /// Усечение обхода папки; `None` для обычного набора файлов.
    truncation: Option<DroppedCollectionTruncation>,
}

impl AdmittedLocalFilesOpen {
    /// Создаётся только родителем в `QueueReplacementTarget::admit`.
    pub(super) fn new(
        paths: Vec<PathBuf>,
        truncation: Option<DroppedCollectionTruncation>,
    ) -> Self {
        Self { paths, truncation }
    }

    /// Усечён ли набор лимитом обхода папки (для уведомления в момент commit).
    pub(crate) fn truncation(&self) -> Option<DroppedCollectionTruncation> {
        self.truncation
    }

    /// Передаёт точные native paths owner-у замены очереди.
    pub(crate) fn into_paths(self) -> Vec<PathBuf> {
        self.paths
    }
}

impl fmt::Debug for AdmittedLocalFilesOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdmittedLocalFilesOpen(<redacted>)")
    }
}

impl InAppQueueReplacementIntent {
    /// Захватывает набор файлов без probe/stat/open; подпись — только их число.
    pub(crate) fn local_files(paths: Vec<PathBuf>) -> Self {
        let safe_label = local_files_count_label(paths.len());
        Self {
            target: QueueReplacementTarget::LocalFiles {
                paths,
                truncation: None,
            },
            safe_label,
        }
    }
}

impl InAppQueueReplacementIntent {
    /// Набор файлов, собранный из брошенных папок: подпись с числом файлов и ИМЕНЕМ папки
    /// (без родительских каталогов), например `23 файла из папки „Музыка“ и ещё 2`.
    ///
    /// Подтверждение показывается уже после обхода папки (осознанное исключение S14A, см.
    /// `playlist_runtime::dropped_collection`), поэтому число файлов известно и попадает в вопрос.
    pub(crate) fn local_files_from_dropped_collection(
        paths: Vec<PathBuf>,
        source: &DroppedCollectionSource,
        truncation: Option<DroppedCollectionTruncation>,
    ) -> Self {
        let safe_label = dropped_collection_label(paths.len(), source, truncation);
        Self {
            target: QueueReplacementTarget::LocalFiles { paths, truncation },
            safe_label,
        }
    }
}

/// Подпись набора из папки: «23 файла из папки „Музыка“», «первые 2000 файлов из папки …».
fn dropped_collection_label(
    count: usize,
    source: &DroppedCollectionSource,
    truncation: Option<DroppedCollectionTruncation>,
) -> SafeMediaLabel {
    let noun = files_noun(count);
    let prefix = match truncation {
        Some(DroppedCollectionTruncation::FileLimit) => "first ",
        Some(DroppedCollectionTruncation::DepthLimit) | None => "",
    };
    let others = match source.other_item_count {
        0 => String::new(),
        other_count => format!(" and {other_count} more"),
    };
    SafeMediaLabel::from_service_safe_label(&format!(
        "{prefix}{count} {noun} from folder “{}”{others}",
        source.first_folder_name
    ))
}

/// «1 файл», «3 файла», «5 файлов» — подпись набора без имён и путей.
fn local_files_count_label(count: usize) -> SafeMediaLabel {
    SafeMediaLabel::from_service_safe_label(&format!("{count} {}", files_noun(count)))
}

/// Английское единственное число используется только для ровно одного файла.
fn files_noun(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_label_uses_correct_english_plural_forms() {
        let label = |count| local_files_count_label(count).as_str().to_owned();
        assert_eq!(label(1), "1 file");
        assert_eq!(label(2), "2 files");
        assert_eq!(label(5), "5 files");
        assert_eq!(label(11), "11 files");
        assert_eq!(label(21), "21 files");
        assert_eq!(label(24), "24 files");
        assert_eq!(label(112), "112 files");
    }

    #[test]
    fn dropped_folder_label_shows_count_folder_name_and_truncation_without_parents() {
        let source = DroppedCollectionSource {
            first_folder_name: "Музыка".to_owned(),
            other_item_count: 0,
        };
        let label = |count, truncation| {
            dropped_collection_label(count, &source, truncation)
                .as_str()
                .to_owned()
        };
        assert_eq!(label(23, None), "23 files from folder “Музыка”");
        assert_eq!(
            label(2000, Some(DroppedCollectionTruncation::FileLimit)),
            "first 2000 files from folder “Музыка”"
        );
        let with_others = DroppedCollectionSource {
            other_item_count: 2,
            ..source
        };
        assert_eq!(
            dropped_collection_label(1, &with_others, None).as_str(),
            "1 file from folder “Музыка” and 2 more"
        );
    }

    #[test]
    fn admitted_files_keep_exact_order_and_hide_paths_in_debug() {
        let admitted = AdmittedLocalFilesOpen::new(
            vec![PathBuf::from("/b/secret.mkv"), PathBuf::from("/a.mkv")],
            None,
        );
        assert!(!format!("{admitted:?}").contains("secret"));
        assert_eq!(
            admitted.into_paths(),
            vec![PathBuf::from("/b/secret.mkv"), PathBuf::from("/a.mkv")]
        );
    }
}
