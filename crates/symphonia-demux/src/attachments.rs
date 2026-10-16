//! Вложения контейнера (шрифты MKV и т.п.): лимиты чтения и перенос в нейтральную модель.
//!
//! Владелец инварианта «вложения больше лимита не загружаются в память» — additive-патч
//! `symphonia-format-mkv`. Этот модуль только:
//! 1. описывает лимиты на уровне `DemuxerOptions` ([`AttachmentReadLimits`]);
//! 2. оборачивает probe в область лимитов патча и переводит его отчёт о пропусках;
//! 3. превращает принятые Symphonia-вложения в `media_core::MediaAttachments`.

use bytes::Bytes;
use media_core::{MediaAttachment, MediaAttachments, SkippedAttachment, SkippedAttachmentReason};
use symphonia::core::formats::Attachment;
use symphonia_format_mkv::{
    AttachmentReadLimits as PatchLimits, SkippedAttachment as PatchSkippedAttachment,
    SkippedAttachmentReason as PatchSkippedReason,
};

/// Дефолтный лимит размера одного вложения: 20 MiB.
pub const DEFAULT_MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

/// Дефолтный лимит суммарного размера вложений: 100 MiB.
pub const DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

/// Лимиты чтения вложений контейнера в память.
///
/// Поля именованные, а менять значения удобно через builder-методы: на месте вызова видно,
/// какой из двух лимитов задаётся.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentReadLimits {
    /// Максимальный размер содержимого одного вложения в байтах.
    pub max_bytes_per_attachment: u64,

    /// Максимальная сумма содержимого всех принятых вложений в байтах.
    pub max_total_bytes: u64,
}

impl AttachmentReadLimits {
    /// Дефолтные лимиты (20 MiB на вложение, 100 MiB всего) как const для `const fn` конструкторов.
    pub const DEFAULT: Self = Self {
        max_bytes_per_attachment: DEFAULT_MAX_ATTACHMENT_BYTES,
        max_total_bytes: DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
    };

    /// Задаёт лимит на одно вложение, не трогая суммарный.
    #[must_use]
    pub const fn with_max_bytes_per_attachment(mut self, max_bytes_per_attachment: u64) -> Self {
        self.max_bytes_per_attachment = max_bytes_per_attachment;
        self
    }

    /// Задаёт суммарный лимит, не трогая лимит на одно вложение.
    #[must_use]
    pub const fn with_max_total_bytes(mut self, max_total_bytes: u64) -> Self {
        self.max_total_bytes = max_total_bytes;
        self
    }
}

impl Default for AttachmentReadLimits {
    /// 20 MiB на вложение и 100 MiB всего (решение владельца, сессия 06).
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<AttachmentReadLimits> for PatchLimits {
    /// Передаёт лимиты additive-API MKV-патча.
    fn from(limits: AttachmentReadLimits) -> Self {
        PatchLimits::new(limits.max_bytes_per_attachment, limits.max_total_bytes)
    }
}

/// Выполняет `probe` внутри области лимитов MKV-патча и возвращает отчёт о пропущенных вложениях.
///
/// Область нужна, потому что generic Symphonia `Probe` создаёт reader-ы без параметров.
/// Для не-Matroska контейнеров область ничего не меняет и отчёт пустой.
pub(crate) fn probe_with_attachment_limits<T>(
    limits: AttachmentReadLimits,
    probe: impl FnOnce() -> T,
) -> (T, Vec<SkippedAttachment>) {
    let (result, skipped) = symphonia_format_mkv::with_attachment_read_limits(limits.into(), probe);
    (
        result,
        skipped
            .into_iter()
            .map(skipped_attachment_from_patch)
            .collect(),
    )
}

/// Переводит отчёт патча в нейтральный тип media-core.
fn skipped_attachment_from_patch(skipped: PatchSkippedAttachment) -> SkippedAttachment {
    let reason = match skipped.reason {
        PatchSkippedReason::TooLarge {
            size_bytes,
            limit_bytes,
        } => SkippedAttachmentReason::TooLarge {
            size_bytes,
            limit_bytes,
        },
        PatchSkippedReason::TotalLimitExceeded {
            size_bytes,
            accepted_total_bytes,
            limit_bytes,
        } => SkippedAttachmentReason::TotalLimitExceeded {
            size_bytes,
            accepted_total_bytes,
            limit_bytes,
        },
    };
    SkippedAttachment {
        file_name: skipped.file_name,
        reason,
    }
}

/// Собирает `MediaAttachments` из принятых reader-ом вложений и ранее записанных пропусков.
///
/// Vendor-specific вложения без файла игнорируются: плееру нужны только файлы (шрифты).
/// Содержимое копируется в `Bytes`; это ограничено теми же лимитами, что применил reader.
pub(crate) fn media_attachments_from_reader(
    reader_attachments: &[Attachment],
    skipped: &[SkippedAttachment],
) -> MediaAttachments {
    let attachments = reader_attachments
        .iter()
        .filter_map(|attachment| match attachment {
            Attachment::File(file) => Some(MediaAttachment {
                file_name: file.name.clone(),
                media_type: file
                    .media_type
                    .clone()
                    .filter(|media_type| !media_type.is_empty()),
                bytes: Bytes::copy_from_slice(&file.data),
            }),
            _ => None,
        })
        .collect();

    MediaAttachments {
        attachments,
        skipped: skipped.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use symphonia::core::formats::FileAttachment;

    use super::*;

    #[test]
    fn default_limits_match_owner_decision_and_builders_change_one_field() {
        let defaults = AttachmentReadLimits::default();
        assert_eq!(defaults.max_bytes_per_attachment, 20 * 1024 * 1024);
        assert_eq!(defaults.max_total_bytes, 100 * 1024 * 1024);

        let tightened = defaults.with_max_bytes_per_attachment(7);
        assert_eq!(tightened.max_bytes_per_attachment, 7);
        assert_eq!(tightened.max_total_bytes, defaults.max_total_bytes);
        assert_eq!(tightened.with_max_total_bytes(9).max_total_bytes, 9);
    }

    #[test]
    fn reader_attachments_become_media_attachments_with_files_only() {
        let reader_attachments = [
            Attachment::File(FileAttachment {
                name: "Font.ttf".to_owned(),
                description: None,
                media_type: Some("font/ttf".to_owned()),
                data: vec![1, 2, 3].into_boxed_slice(),
            }),
            Attachment::File(FileAttachment {
                name: "x.bin".to_owned(),
                description: None,
                media_type: Some(String::new()),
                data: Box::default(),
            }),
        ];
        let skipped = [SkippedAttachment {
            file_name: "big.otf".to_owned(),
            reason: SkippedAttachmentReason::TooLarge {
                size_bytes: 9,
                limit_bytes: 1,
            },
        }];

        let attachments = media_attachments_from_reader(&reader_attachments, &skipped);

        assert_eq!(attachments.attachments.len(), 2);
        assert_eq!(attachments.attachments[0].file_name, "Font.ttf");
        assert_eq!(
            attachments.attachments[0].media_type.as_deref(),
            Some("font/ttf")
        );
        assert_eq!(&attachments.attachments[0].bytes[..], &[1, 2, 3]);
        assert_eq!(attachments.attachments[1].media_type, None);
        assert_eq!(attachments.skipped, skipped);
    }

    #[test]
    fn probe_scope_returns_closure_result_and_empty_report_without_matroska() {
        let (value, skipped) =
            probe_with_attachment_limits(AttachmentReadLimits::default(), || 41 + 1);
        assert_eq!(value, 42);
        assert!(skipped.is_empty());
    }
}
