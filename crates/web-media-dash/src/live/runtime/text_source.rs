//! Живой источник снимков для субтитров: читает тот же принятый снимок MPD, что и демуксер A/V.

use std::sync::Arc;
use std::time::Duration;

use super::DashLiveShared;
use crate::text::{
    DashTextError, DashTextRefresh, DashTextSnapshot, DashTextSnapshotSource, DashTextSubtitles,
};

/// Нижняя граница периода перечитывания индекса: защита от `minimumUpdatePeriod` почти в ноль.
const MINIMUM_TEXT_REFRESH: Duration = Duration::from_secs(1);

/// Читает текущий снимок из общего состояния live-сессии.
struct DashLiveTextSource {
    shared: Arc<DashLiveShared>,
}

impl DashTextSnapshotSource for DashLiveTextSource {
    /// Согласованное чтение: план, контекст/поколение сети и начало шкалы плеера за один lock.
    fn snapshot(&self) -> Result<DashTextSnapshot, DashTextError> {
        let state = self.shared.state.lock().map_err(|_| DashTextError::Stale)?;
        let minimum_update_period =
            Duration::from_millis(state.snapshot.mpd.minimum_update_period_milliseconds);
        Ok(DashTextSnapshot {
            plan: Arc::clone(&state.snapshot.text),
            // Вспомогательный вид: сбои субтитров не взводят endpoint recovery видео.
            http: state.http.for_auxiliary_resources(),
            generation: state.generation,
            source_origin: self.shared.session_timeline.source_origin(),
            refresh: DashTextRefresh::After(minimum_update_period.max(MINIMUM_TEXT_REFRESH)),
        })
    }
}

/// Дорожки субтитров живой сессии по первому принятому снимку; `None`, если дорожек нет.
pub(super) fn live_text_subtitles(shared: &Arc<DashLiveShared>) -> Option<DashTextSubtitles> {
    let initial_plan = Arc::clone(&shared.state.lock().ok()?.snapshot.text);
    DashTextSubtitles::new(
        &initial_plan,
        Box::new(DashLiveTextSource {
            shared: Arc::clone(shared),
        }),
    )
}
