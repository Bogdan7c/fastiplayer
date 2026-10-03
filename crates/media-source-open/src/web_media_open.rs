//! Чистые (без app-состояния) части YtDlp/extractor open orchestration.
//!
//! Сейчас здесь живёт то, что нужно протокольным opener-ам HLS/DASH и дереву
//! `web_media_open` в `app-egui` одновременно: намерение выбора candidate-а,
//! финализация independent component variants и capability probe каталога.
//! Перенос разрывает цикл «opener-ы ↔ `web_media_open`»: оба теперь зависят от
//! этого модуля, а не друг от друга. Остаток дерева `web_media_open` переедет
//! сюда позже под тем же именем модуля, поэтому пути тестов
//! (`web_media_open::component_variants_tests::...`) не меняются.

pub mod catalog_capabilities;
pub mod component_variants;
#[cfg(test)]
mod component_variants_tests;
#[cfg(test)]
#[cfg(unix)]
mod open_intent_tests;

use component_variants::{YtDlpComposedCandidateOpenIntent, YtDlpExactCandidateOpenIntent};

/// Намерение selection: новый лучший playable candidate либо semantic rematch старого exact выбора.
///
/// Варианты `Exact`/`Composed` создаются только именованными конструкторами из
/// `component_variants`: их поля закрыты, чтобы вызывающий код не мог собрать
/// несогласованное намерение в обход этих конструкторов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YtDlpCandidateOpenIntent {
    /// Первичное открытие либо явная runtime override/reselection.
    BestPlayable,
    /// Restore/rebuild обязан сохранить semantic candidate identity.
    Exact(Box<YtDlpExactCandidateOpenIntent>),
    /// Service-owned video-only + audio-only composition из одного fresh snapshot-а.
    Composed(Box<YtDlpComposedCandidateOpenIntent>),
}
