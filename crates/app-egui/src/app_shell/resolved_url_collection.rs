//! Доставка разобранной коллекции ссылки (бросок на видео) из `PlaylistRuntime` в `AppState`.

use super::AppShell;

impl AppShell {
    /// Забирает разобранную коллекцию и применяет её; `true`, если UI изменился.
    ///
    /// Пока окна/renderer-а нет (suspend), коллекция остаётся у владельца URL-импорта и будет
    /// применена при следующем drain после resume; отмена/новое открытие очищают её там же.
    pub(super) fn drain_resolved_url_collection(&mut self) -> bool {
        let (Some(app_state), Some(renderer)) = (self.app_state.as_mut(), self.renderer.as_ref())
        else {
            return false;
        };
        let Some(collection) = self.playlist_runtime.take_resolved_url_collection() else {
            return false;
        };
        app_state.apply_resolved_url_collection(collection, &mut self.playlist_runtime, renderer);
        true
    }
}
