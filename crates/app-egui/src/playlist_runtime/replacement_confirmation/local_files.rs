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
    let noun = russian_files_noun(count);
    let prefix = match truncation {
        Some(DroppedCollectionTruncation::FileLimit) => "первые ",
        Some(DroppedCollectionTruncation::DepthLimit) | None => "",
    };
    let others = match source.other_item_count {
        0 => String::new(),
        other_count => format!(" и ещё {other_count}"),
    };
    SafeMediaLabel::from_service_safe_label(&format!(
        "{prefix}{count} {noun} из папки „{}“{others}",
        source.first_folder_name
    ))
}

/// «1 файл», «3 файла», «5 файлов» — подпись набора без имён и путей.
fn local_files_count_label(count: usize) -> SafeMediaLabel {
    SafeMediaLabel::from_service_safe_label(&format!("{count} {}", russian_files_noun(count)))
}

/// Форма слова «файл» для числа: 1 файл, 2 файла, 5 файлов, 11 файлов, 21 файл.
fn russian_files_noun(count: usize) -> &'static str {
    let last_two_digits = count % 100;
    let last_digit = count % 10;
    if (11..=14).contains(&last_two_digits) {
        "файлов"
    } else {
        match last_digit {
            1 => "файл",
            2..=4 => "файла",
            _ => "файлов",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_label_uses_correct_russian_plural_forms() {
        let label = |count| local_files_count_label(count).as_str().to_owned();
        assert_eq!(label(1), "1 файл");
        assert_eq!(label(2), "2 файла");
        assert_eq!(label(5), "5 файлов");
        assert_eq!(label(11), "11 файлов");
        assert_eq!(label(21), "21 файл");
        assert_eq!(label(24), "24 файла");
        assert_eq!(label(112), "112 файлов");
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
        assert_eq!(label(23, None), "23 файла из папки „Музыка“");
        assert_eq!(
            label(2000, Some(DroppedCollectionTruncation::FileLimit)),
            "первые 2000 файлов из папки „Музыка“"
        );
        let with_others = DroppedCollectionSource {
            other_item_count: 2,
            ..source
        };
        assert_eq!(
            dropped_collection_label(1, &with_others, None).as_str(),
            "1 файл из папки „Музыка“ и ещё 2"
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
