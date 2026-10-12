//! Замена очереди разобранной коллекцией ссылки: типы admission и подпись для диалога.
//!
//! Вынесено из `replacement_confirmation.rs` (модуль близок к лимиту размера). Slot,
//! supersede-правила и Confirm/Cancel остаются у родителя: здесь только данные варианта
//! `QueueReplacementTarget::ResolvedUrlCollection`.

use std::fmt;

use super::{InAppQueueReplacementIntent, QueueReplacementTarget};
use crate::media_open::SafeMediaLabel;
use crate::playlist_runtime::ResolvedUrlCollection;

/// Коллекция, прошедшая empty-queue gate либо matching Confirm.
pub(crate) struct AdmittedResolvedUrlCollection {
    collection: ResolvedUrlCollection,
}

impl AdmittedResolvedUrlCollection {
    /// Создаётся только родителем в `QueueReplacementTarget::admit`.
    pub(super) fn new(collection: ResolvedUrlCollection) -> Self {
        Self { collection }
    }

    /// Передаёт коллекцию owner-у замены очереди.
    pub(crate) fn into_collection(self) -> ResolvedUrlCollection {
        self.collection
    }
}

impl fmt::Debug for AdmittedResolvedUrlCollection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdmittedResolvedUrlCollection(<redacted>)")
    }
}

impl InAppQueueReplacementIntent {
    /// Захватывает разобранную коллекцию; подпись — только число роликов и домен
    /// («12 роликов с youtube.com»), без пути, query и логина ссылки.
    pub(crate) fn resolved_url_collection(collection: ResolvedUrlCollection) -> Self {
        let safe_label = collection_label(collection.item_count(), collection.display_host());
        Self {
            target: QueueReplacementTarget::ResolvedUrlCollection(collection),
            safe_label,
        }
    }
}

/// Подпись: «1 ролик с youtube.com», «12 роликов с youtube.com»; без домена — «по ссылке».
fn collection_label(item_count: usize, display_host: Option<&str>) -> SafeMediaLabel {
    let noun = videos_noun(item_count);
    let source = match display_host {
        Some(host) => format!("from {host}"),
        None => "from the link".to_owned(),
    };
    SafeMediaLabel::from_service_safe_label(&format!("{item_count} {noun} {source}"))
}

/// Английское единственное число используется только для ровно одного ролика.
fn videos_noun(count: usize) -> &'static str {
    if count == 1 { "video" } else { "videos" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_has_count_plural_and_domain_only() {
        let label = |count, host| collection_label(count, host).as_str().to_owned();
        assert_eq!(label(1, Some("youtube.com")), "1 video from youtube.com");
        assert_eq!(label(3, Some("youtube.com")), "3 videos from youtube.com");
        assert_eq!(label(12, Some("youtube.com")), "12 videos from youtube.com");
        assert_eq!(label(21, Some("youtube.com")), "21 videos from youtube.com");
        assert_eq!(label(5, None), "5 videos from the link");
    }
}
