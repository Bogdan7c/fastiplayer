//! Identity, reopen intent и состояние доказанных native web-источников
//! (HLS/DASH/HDS/Smooth), а также единый owner отката native → extractor.
//!
//! Перенесено из `app-egui/src/media_open/native_{hls,dash,hds,smooth,fallback}.rs`
//! (session-07 выноса web-media): эти типы стоят прямо в сигнатурах чистой
//! подготовки `native_startup`, которая теперь тоже живёт в crate-е. Поведение,
//! redaction URL-ов и generation/identity fences не менялись. В `app-egui` прежние
//! пути `crate::media_open::{Native*Url, …}` и `crate::media_open::native_fallback`
//! сохранены re-export-ами.

pub mod dash;
pub mod fallback;
pub mod hds;
pub mod hls;
pub mod smooth;
