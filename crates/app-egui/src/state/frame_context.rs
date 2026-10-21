//! Неизменяемый контекст одного render frame-а.

use std::sync::Arc;

use player_core::{FrameCounters, PlayerSnapshot};
use render_core::RenderDiagnostics;

/// Immutable данные, зафиксированные один раз для текущего render frame-а.
pub struct AppFrameContext {
    /// Snapshot player-а, общий для UI, renderer boundary, desktop integration и pacing.
    /// Это `Arc` того же снимка, что держит worker handle: контекст кадра его не копирует.
    player_snapshot: Arc<PlayerSnapshot>,

    /// Счётчики кадров окна на начало frame-а. Это данные UI/показа, а не состояния
    /// плеера, поэтому живут здесь, а не в `PlayerSnapshot`.
    frame_counters: FrameCounters,

    /// Renderer-neutral diagnostics, которые UI показывает в этом же frame-е.
    render_diagnostics: RenderDiagnostics,
}

impl AppFrameContext {
    /// Фиксирует снимок плеера, счётчики кадров и диагностику рендера на один кадр.
    pub(super) const fn new(
        player_snapshot: Arc<PlayerSnapshot>,
        frame_counters: FrameCounters,
        render_diagnostics: RenderDiagnostics,
    ) -> Self {
        Self {
            player_snapshot,
            frame_counters,
            render_diagnostics,
        }
    }

    /// Возвращает единственный player snapshot текущего render frame-а.
    #[must_use]
    pub fn player_snapshot(&self) -> &PlayerSnapshot {
        &self.player_snapshot
    }

    /// Возвращает счётчики кадров (показано/отброшено/повторено), зафиксированные для кадра.
    #[must_use]
    pub const fn frame_counters(&self) -> FrameCounters {
        self.frame_counters
    }

    /// Возвращает render diagnostics, зафиксированные для текущего render frame-а.
    #[must_use]
    pub const fn render_diagnostics(&self) -> &RenderDiagnostics {
        &self.render_diagnostics
    }
}
