//! Вложения контейнера (например, шрифты MKV) на границе demuxer → потребитель.

use bytes::Bytes;

/// Вложенный в контейнер файл, целиком прочитанный в память.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttachment {
    /// Имя файла вложения, как записано в контейнере.
    pub file_name: String,

    /// MIME-тип вложения, если контейнер его указал.
    pub media_type: Option<String>,

    /// Содержимое вложения.
    pub bytes: Bytes,
}

/// Почему вложение не было загружено в память.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedAttachmentReason {
    /// Вложение больше лимита на один файл.
    TooLarge {
        /// Объявленный размер вложения в байтах.
        size_bytes: u64,
        /// Лимит на одно вложение в байтах.
        limit_bytes: u64,
    },

    /// Вложение само по себе допустимо, но суммарный лимит уже исчерпан бы им.
    TotalLimitExceeded {
        /// Объявленный размер вложения в байтах.
        size_bytes: u64,
        /// Сколько байт вложений уже принято до этого.
        accepted_total_bytes: u64,
        /// Суммарный лимит в байтах.
        limit_bytes: u64,
    },

    /// Размер вложения не удалось определить или данные нельзя прочитать.
    Unreadable,
}

/// Вложение, содержимое которого намеренно не читалось в память.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedAttachment {
    /// Имя файла вложения, если контейнер его указал.
    pub file_name: String,

    /// Причина пропуска.
    pub reason: SkippedAttachmentReason,
}

/// Результат `Demuxer::take_attachments`: принятые и пропущенные вложения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaAttachments {
    /// Вложения, прочитанные в память в порядке контейнера.
    pub attachments: Vec<MediaAttachment>,

    /// Вложения, пропущенные из-за лимитов (без чтения содержимого).
    pub skipped: Vec<SkippedAttachment>,
}

impl MediaAttachments {
    /// Возвращает `true`, если нет ни принятых, ни пропущенных вложений.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attachments.is_empty() && self.skipped.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_attachments_are_empty_and_skipped_ones_make_it_non_empty() {
        let mut attachments = MediaAttachments::default();
        assert!(attachments.is_empty());

        attachments.skipped.push(SkippedAttachment {
            file_name: "big.ttf".to_owned(),
            reason: SkippedAttachmentReason::Unreadable,
        });
        assert!(!attachments.is_empty());
    }
}
