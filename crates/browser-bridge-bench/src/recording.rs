//! Запись потока страницы (`*.fprec`, версия 1): разбор, сборка и синтетический генератор.
//!
//! Файл пишет отладочная сборка экстеншена (`extensions/browser`, только при явном включении),
//! а стенд повторяет его командой `replay` на странице `replay-recording`: те же вызовы MSE на
//! своём `<video>` в исходном темпе, передача плееру в момент `handoff_began`.
//!
//! Раскладка файла (контракт, общий с рекордером экстеншена):
//! 1. 8 байт ASCII-магия `FPREC001`;
//! 2. u32 little-endian — длина манифеста N;
//! 3. N байт UTF-8 JSON — манифест (поле `events` — список событий);
//! 4. «склад байтов» — подряд все полезные нагрузки `append`.
//!
//! Модули:
//! - [`format`] — константы формата, таблица известных `op`, FNV-1a;
//! - [`parse`] — разбор файла в [`Recording`] (строгий к обязательным полям, мягкий к
//!   неизвестным полям и `op`: предупреждение вместо ошибки — вперёд-совместимость);
//! - [`write`] — сборка файла ([`RecordingBuilder`]), нужна генератору и тестам;
//! - [`synthesize`] — синтетическая запись из fMP4-ассета стенда.

mod error;
pub mod format;
pub mod parse;
pub mod synthesize;
pub mod write;

#[cfg(test)]
mod tests;

pub use error::RecordingError;
pub use parse::{RecordedEvent, Recording, RecordingManifest};
pub use synthesize::{SynthesisOptions, synthesize_from_asset};
pub use write::RecordingBuilder;
