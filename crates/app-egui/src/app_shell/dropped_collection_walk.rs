//! Доставка результата фонового обхода брошенных папок из `PlaylistRuntime` в `AppState`.

use super::AppShell;

impl AppShell {
    /// Забирает завершённый обход и применяет его к очереди; `true`, если UI изменился.
    ///
    /// Пока окна/renderer-а нет (suspend), результат остаётся у владельца и будет применён
    /// при следующем drain после resume: терять его нельзя, а применять без UI нечему.
    pub(super) fn drain_dropped_collection_walk(&mut self) -> bool {
        let (Some(app_state), Some(renderer)) = (self.app_state.as_mut(), self.renderer.as_ref())
        else {
            return false;
        };
        let Some(completion) = self
            .playlist_runtime
            .take_dropped_collection_walk_completion()
        else {
            return false;
        };
        app_state.apply_dropped_collection_walk_completion(
            completion,
            &mut self.playlist_runtime,
            renderer,
        );
        true
    }
}
