//! Timeline UI facade: публичный контракт отделён от geometry, gesture и egui rendering.

mod geometry;
mod gesture;
mod live_scrub;
mod render;

#[allow(unused_imports)]
// Facade сохраняет прежние internal paths для соседних UI-модулей и тестов.
pub use frame_server_core::{
    DeferredLiveScrubSettingsChange, LiveScrubDecodeMode as TimelineLiveScrubDecodeMode,
    LiveScrubDiagnostics, LiveScrubSettingsSnapshot as TimelineLiveScrubSettingsSnapshot,
};
#[allow(unused_imports)] // Намеренный compatibility facade после decomposition.
pub use geometry::{TimelineBounds, format_media_duration, format_media_time, format_seconds};
#[allow(unused_imports)] // Намеренный compatibility facade после decomposition.
pub use gesture::{
    TimelineAction, TimelineInteraction, TimelinePointerInput, TimelineUiState,
    map_timeline_interaction,
};
pub use render::{render_time_labels, render_timeline};
