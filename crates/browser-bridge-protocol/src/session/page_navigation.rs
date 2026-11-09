//! Навигация страницы (протокол 1.7, сессия 14a): есть ли на странице следующая серия.
//!
//! Экстеншен сообщает это сообщением `page_navigation`; оно не привязано к поколению
//! потока, последнее слово главнее. До первого сообщения плеер считает, что следующей серии
//! нет, а со старым экстеншеном (общая версия ниже 1.7) кнопки Next нет вовсе.

use serde::{Deserialize, Serialize};

use crate::strict_shape::strict_label;
use crate::version::ProtocolVersion;

strict_label!(NextEpisodeAvailability);

/// Версия протокола, в которой появился `page_navigation`.
pub const PAGE_NAVIGATION_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_7;

/// Есть ли на странице следующая серия.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum NextEpisodeAvailability {
    /// Следующая серия есть: `next_episode` имеет смысл.
    Available,
    /// Следующей серии нет (или адаптер её не нашёл).
    Unavailable,
}
