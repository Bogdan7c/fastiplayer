//! «Общая доска» (04c): обычное окно плеера с очередью не меняется, когда принимает
//! настройки соседа (план 04c, «Тесты»). Маршрут очереди здесь — настоящий
//! `PlaylistRuntime` с элементами, а не записывающая заглушка.

use super::shared_board::{
    apply_in_settings_window, copy_on, deliver_file_signal, shared_config_file, text_size,
};
use super::*;

/// Адаптер, у которого маршрут очереди — настоящий `PlaylistRuntime` с элементами.
struct QueueBackedAdapter {
    inner: RecordingRuntimeAdapter,
    playlist_runtime: crate::playlist_runtime::PlaylistRuntime,
    staged_playlist_settings: Vec<bool>,
}

impl RenderLiveSettingsAdapter for QueueBackedAdapter {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.inner.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.inner.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.inner.rollback_live_settings(baseline)
    }
}

impl SettingsRuntimeReconfigureHost for QueueBackedAdapter {
    fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        self.inner.sync_committed_config_snapshot(snapshot);
    }

    fn finalize_settings_transaction(&mut self) {
        self.playlist_runtime.finalize_playlist_settings();
    }

    fn apply_playlist_runtime_settings(
        &mut self,
        update: &PlaylistRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        match self
            .playlist_runtime
            .stage_playlist_settings(update.playlist)
        {
            Ok(changed) => {
                self.staged_playlist_settings.push(changed);
                AppRouteApplyResult::Applied
            }
            Err(_error) => AppRouteApplyResult::Failed {
                message: "playlist settings rejected".to_string(),
            },
        }
    }

    fn recreate_renderer(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        self.inner.recreate_renderer(previous, next)
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult {
        self.inner
            .apply_player_runtime_settings(update, target_policy)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
        target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult {
        self.inner
            .apply_media_service_runtime_settings(update, affected_settings, target_policy)
    }

    fn rollback_playlist_runtime_settings(&mut self) -> AppRouteApplyResult {
        match self.playlist_runtime.rollback_playlist_settings() {
            Ok(_changed) => AppRouteApplyResult::Applied,
            Err(message) => AppRouteApplyResult::Failed { message },
        }
    }
}

/// Обычное окно с очередью не меняется: принятие политики очереди и стиля субтитров от
/// соседа не трогает элементы, текущий элемент и запись очереди.
#[test]
fn adoption_keeps_the_regular_window_queue_untouched() {
    let config = custom_config_for_test();
    let (directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut playlist_runtime = crate::playlist_runtime::PlaylistRuntime::new_memory_only(
        crate::app_wake::AppWakePort::disconnected(crate::app_wake::AppWakeOwner::PlaylistRuntime),
        config.playlist,
    )
    .expect("queue owner");
    let queue_files = ["one.mkv", "two.mkv", "three.mkv"]
        .map(|name| directory.path().join(name))
        .to_vec();
    let _replaced = playlist_runtime.replace_queue_with_local_files(queue_files);
    let queue_before = playlist_runtime.playlist_view_model();
    let mut second_adapter = QueueBackedAdapter {
        inner: RecordingRuntimeAdapter::from_config(&config).expect("adapter"),
        playlist_runtime,
        staged_playlist_settings: Vec::new(),
    };
    assert_eq!(queue_before.item_count(), 3);

    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![
            (
                "playlist.load_siblings",
                SettingValue::Bool(!config.playlist.load_siblings),
            ),
            (
                "subtitles.text_size_percent",
                text_size(config.subtitles.text_size_percent + 5),
            ),
        ],
    );
    let board_frame = deliver_file_signal(&mut second, &mut second_adapter);

    assert_eq!(board_frame.notice, None);
    assert_eq!(
        second_adapter.staged_playlist_settings,
        vec![true],
        "policy adopted"
    );
    let queue_after = second_adapter.playlist_runtime.playlist_view_model();
    assert_eq!(
        queue_after.structural_revision(),
        queue_before.structural_revision()
    );
    assert_eq!(queue_after.item_count(), queue_before.item_count());
    assert_eq!(queue_after.active_item_id(), queue_before.active_item_id());
    for row in 0..queue_before.item_count() {
        assert_eq!(queue_after.item_id_at(row), queue_before.item_id_at(row));
    }
    assert!(
        !second_adapter
            .playlist_runtime
            .has_pending_playlist_persistence_work(),
        "queue persistence is not triggered"
    );
}
